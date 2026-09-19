//! RFC 8058 one-click unsubscribe, database side.
//!
//! The site-generated MAC key, the per-recipient link a personalized
//! delivery carries, and redemption — which removes the membership
//! immediately, as the RFC requires, with its goodbye notice and audit event
//! in one transaction.
use crate::{AuditContext, Database, db_error};
use base64::Engine;
use listmngr_core::{Error, ListId, Member, MemberId, Result, one_click};
use rand::RngCore;

const SECRET_NAME: &str = "one-click-unsubscribe";

#[derive(Debug, Clone, Copy)]
pub struct OneClickRepo<'a> {
    db: &'a Database,
}

impl Database {
    #[must_use]
    pub const fn one_click(&self) -> OneClickRepo<'_> {
        OneClickRepo { db: self }
    }
}

impl OneClickRepo<'_> {
    /// The site's one-click key, generated on first use. Rotating it (delete
    /// the `site_secrets` row) invalidates every link already delivered.
    /// # Errors
    /// Returns a database error.
    pub async fn signer(&self) -> Result<one_click::Signer> {
        let existing: Option<String> =
            sqlx::query_scalar("SELECT secret FROM site_secrets WHERE name=$1")
                .bind(SECRET_NAME)
                .fetch_optional(self.db.pool())
                .await
                .map_err(db_error)?;
        let secret = if let Some(secret) = existing {
            secret
        } else {
            let mut bytes = [0_u8; 32];
            rand::rng().fill_bytes(&mut bytes);
            let fresh = base64::engine::general_purpose::STANDARD.encode(bytes);
            // Two nodes may race on first use; the first insert wins for both.
            sqlx::query("INSERT INTO site_secrets(name,secret,created_at) VALUES($1,$2,$3) ON CONFLICT(name) DO NOTHING")
                .bind(SECRET_NAME).bind(&fresh).bind(chrono::Utc::now().to_rfc3339())
                .execute(self.db.pool()).await.map_err(db_error)?;
            sqlx::query_scalar("SELECT secret FROM site_secrets WHERE name=$1")
                .bind(SECRET_NAME)
                .fetch_one(self.db.pool())
                .await
                .map_err(db_error)?
        };
        let key = base64::engine::general_purpose::STANDARD
            .decode(secret.trim())
            .map_err(|_| Error::Validation("corrupt site secret".into()))?;
        one_click::Signer::new(&key)
    }

    /// The one-click URL for a known membership, from a signer obtained once.
    #[must_use]
    pub fn url_for_member(
        signer: &one_click::Signer,
        base_url: &str,
        list: &ListId,
        member: MemberId,
        now_secs: i64,
    ) -> String {
        one_click::url(base_url, list, &signer.issue(list, member, now_secs))
    }

    /// The one-click URL for `email`'s membership of `list`, when they are a
    /// member and the site has a base URL.
    /// # Errors
    /// Returns a database error.
    pub async fn url_for(
        &self,
        base_url: &str,
        list: &ListId,
        email: &str,
        now_secs: i64,
    ) -> Result<Option<String>> {
        let Ok(address) = listmngr_core::Address::new(email, String::new()) else {
            return Ok(None);
        };
        let member: Option<String> = sqlx::query_scalar("SELECT m.id FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id=$1 AND a.email=$2 AND m.role='member' LIMIT 1")
            .bind(list.as_str()).bind(&address.email)
            .fetch_optional(self.db.pool()).await.map_err(db_error)?;
        let Some(member) = member else {
            return Ok(None);
        };
        let member: MemberId = member
            .parse()
            .map_err(|_| Error::Validation("corrupt member id".into()))?;
        let token = self.signer().await?.issue(list, member, now_secs);
        Ok(Some(one_click::url(base_url, list, &token)))
    }

    /// Redeem a token: the named membership of `list` is removed at once,
    /// with the goodbye notice the list configures and a
    /// `member.unsubscribe.one_click` audit event. An invalid, expired,
    /// foreign or already-redeemed token is not found.
    /// # Errors
    /// Returns not-found for a token that names no current membership of
    /// `list`, or a database error.
    pub async fn redeem(
        &self,
        list: &ListId,
        token: &str,
        now_secs: i64,
        context: &AuditContext,
    ) -> Result<Member> {
        let not_found = || Error::NotFound("unsubscribe link".into());
        let member_id = self
            .signer()
            .await?
            .verify(list, token, now_secs)
            .ok_or_else(not_found)?;
        let member = self
            .db
            .members()
            .get(member_id)
            .await
            .map_err(|_| not_found())?;
        if &member.list_id != list || member.role != listmngr_core::MemberRole::Member {
            return Err(not_found());
        }
        let mut tx = self.db.write_tx().await?;
        let preferences_id: Option<String> =
            sqlx::query_scalar("SELECT preferences_id FROM members WHERE id=$1")
                .bind(member_id.to_string())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_error)?;
        let Some(preferences_id) = preferences_id else {
            return Err(not_found());
        };
        if !crate::workflows::delete_member_with_goodbye(&mut tx, self.db, &member_id.to_string())
            .await?
        {
            return Err(not_found());
        }
        sqlx::query("DELETE FROM preferences WHERE id=$1")
            .bind(preferences_id)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        Database::record_tx_with_context(
            &mut tx,
            context,
            "member.unsubscribe.one_click",
            "member",
            &member_id.to_string(),
            serde_json::json!({"list_id": list.as_str(), "rfc": "8058"}),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(member)
    }
}
