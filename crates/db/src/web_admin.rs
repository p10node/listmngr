//! Owner-only browser member administration with transaction-bound authority.
use crate::{AuditContext, Database, db_error, web_sessions::WebSession};
use listmngr_core::{Error, ListId, MemberId, ModerationAction, Result, UserId};

const OWNED_LISTS: &str = "SELECT l.list_id,substr(l.display_name,1,512) FROM mailing_lists l WHERE (EXISTS (SELECT 1 FROM users u JOIN addresses a ON a.user_id=u.id WHERE u.id=$1 AND u.is_server_owner=1 AND a.verified_on IS NOT NULL) OR EXISTS (SELECT 1 FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id=l.list_id AND m.role='owner' AND a.user_id=$1 AND a.verified_on IS NOT NULL AND (m.subscription_mode='as_address' OR m.user_id=$1)))";

/// Bounded member-role projection; no credentials or other list's members.
#[derive(Debug)]
pub struct AdminMember {
    pub id: MemberId,
    pub email: String,
    pub action: Option<ModerationAction>,
}

pub(crate) fn valid_offset(offset: i64) -> Result<()> {
    if !(0..=200_000).contains(&offset) {
        return Err(Error::Validation("page out of range".into()));
    }
    Ok(())
}

impl Database {
    /// Read settings under live owner and browser session authority.
    /// # Errors
    /// Rejects revoked authority or database failures.
    pub async fn browser_list_settings(
        &self,
        session: &WebSession,
        list: &ListId,
    ) -> Result<listmngr_core::MailingList> {
        let mut tx = self.browser_write_tx().await?;
        Self::browser_owner_tx(&mut tx, session, list).await?;
        let current = crate::lock_list_for_patch(&mut tx, list).await?;
        Self::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(current)
    }

    /// Apply an owner configuration patch using the ordinary list validator.
    /// Authority, fresh locked snapshot, settings and attributed audit are atomic.
    /// # Errors
    /// Rejects revoked authority, invalid settings or audit/database failures.
    pub async fn browser_update_list_settings(
        &self,
        session: &WebSession,
        list: &ListId,
        patch: &serde_json::Value,
    ) -> Result<()> {
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_owner_tx(&mut tx, session, list).await?;
        crate::ListRepo::update_tx(
            &mut tx,
            list,
            patch,
            &AuditContext::new(Some(user), None, None),
        )
        .await?;
        Self::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)
    }

    /// Lists visible to this live owner, permission-filtered before pagination.
    /// # Errors
    /// Rejects stale sessions, invalid offsets and database failures.
    pub async fn browser_admin_lists(
        &self,
        session: &WebSession,
        offset: i64,
    ) -> Result<Vec<(String, String)>> {
        valid_offset(offset)?;
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_user_tx(&mut tx, session).await?;
        let rows = sqlx::query_as(&format!(
            "{OWNED_LISTS} ORDER BY l.list_id LIMIT 21 OFFSET $2"
        ))
        .bind(user.to_string())
        .bind(offset)
        .fetch_all(&mut *tx)
        .await
        .map_err(db_error)?;
        Self::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(rows)
    }

    pub(crate) async fn browser_owner_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Any>,
        session: &WebSession,
        list: &ListId,
    ) -> Result<UserId> {
        let user = Self::browser_user_tx(tx, session).await?;
        let count: i64 = sqlx::query_scalar(&format!(
            "SELECT COUNT(*) FROM ({OWNED_LISTS} AND l.list_id=$2) owned"
        ))
        .bind(user.to_string())
        .bind(list.as_str())
        .fetch_one(&mut **tx)
        .await
        .map_err(db_error)?;
        if count != 1 {
            return Err(Error::Forbidden("browser list owner authority".into()));
        }
        Ok(user)
    }

    /// List only member-role subscriptions on a currently owned list.
    /// # Errors
    /// Rejects revoked authority, invalid offsets and database failures.
    pub async fn browser_admin_members(
        &self,
        session: &WebSession,
        list: &ListId,
        offset: i64,
    ) -> Result<Vec<AdminMember>> {
        self.browser_search_members(session, list, offset, "").await
    }

    /// Literal, case-normalized email substring search before pagination.
    /// # Errors
    /// Rejects stale owner authority, overlong queries and invalid offsets.
    pub async fn browser_search_members(
        &self,
        session: &WebSession,
        list: &ListId,
        offset: i64,
        query: &str,
    ) -> Result<Vec<AdminMember>> {
        valid_offset(offset)?;
        if query.len() > 320 || query.chars().any(char::is_control) {
            return Err(Error::Validation("invalid member search".into()));
        }
        let pattern = format!(
            "%{}%",
            query
                .to_lowercase()
                .replace('!', "!!")
                .replace('%', "!%")
                .replace('_', "!_")
        );
        let mut tx = self.browser_write_tx().await?;
        Self::browser_owner_tx(&mut tx, session, list).await?;
        let rows: Vec<(String, String, Option<String>)> = sqlx::query_as("SELECT m.id,substr(a.email,1,320),m.moderation_action FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id=$1 AND m.role='member' AND a.email LIKE $3 ESCAPE '!' ORDER BY a.email,m.id LIMIT 21 OFFSET $2")
            .bind(list.as_str()).bind(offset).bind(pattern).fetch_all(&mut *tx).await.map_err(db_error)?;
        let members = rows
            .into_iter()
            .map(|(id, email, action)| {
                Ok(AdminMember {
                    id: id.parse().map_err(db_error)?,
                    email,
                    action: action.map(|s| s.parse()).transpose()?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Self::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(members)
    }

    /// Change one member's or nonmember's posting override and audit in one
    /// transaction.
    /// # Errors
    /// Rejects stale owner authority, foreign/non-member IDs and audit failures.
    pub async fn browser_member_policy(
        &self,
        session: &WebSession,
        list: &ListId,
        member: MemberId,
        action: Option<ModerationAction>,
    ) -> Result<()> {
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_owner_tx(&mut tx, session, list).await?;
        let affected = sqlx::query(
            "UPDATE members SET moderation_action=$1 WHERE id=$2 AND list_id=$3 AND role IN ('member','nonmember')",
        )
        .bind(action.map(|a| a.to_string()))
        .bind(member.to_string())
        .bind(list.as_str())
        .execute(&mut *tx)
        .await
        .map_err(db_error)?
        .rows_affected();
        if affected != 1 {
            return Err(Error::NotFound("member".into()));
        }
        Self::record_tx_with_context(
            &mut tx,
            &AuditContext::new(Some(user), None, None),
            "member.update",
            "member",
            &member.to_string(),
            serde_json::json!({"moderation_action":action,"list_id":list,"source":"browser"}),
        )
        .await?;
        Self::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)
    }
}
