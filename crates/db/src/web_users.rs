//! The server owner's user administration.
//!
//! Search, one account's addresses and memberships, its display name and
//! server-owner flag, and forced address verification. Writes are one transaction under live
//! browser session authority, with their audit event.
use crate::{AuditContext, Database, db_error, web_sessions::WebSession};
use listmngr_core::{Address, AddressId, Error, MemberRole, Result, User, UserId};
use sqlx::Row as _;

/// One account as the search lists it.
#[derive(Debug)]
pub struct UserRow {
    pub id: UserId,
    pub display_name: String,
    /// The account's first address, if any.
    pub email: String,
    pub server_owner: bool,
    pub created_at: String,
}

/// One membership of an account.
#[derive(Debug)]
pub struct UserMembership {
    pub member_id: String,
    pub list_id: String,
    pub role: MemberRole,
    pub email: String,
}

/// An account as the administrator's page shows it.
#[derive(Debug)]
pub struct UserDetail {
    pub user: User,
    pub addresses: Vec<Address>,
    pub memberships: Vec<UserMembership>,
}

fn escape_like(query: &str) -> String {
    format!(
        "%{}%",
        query
            .to_lowercase()
            .replace('!', "!!")
            .replace('%', "!%")
            .replace('_', "!_")
    )
}

impl Database {
    /// A page of accounts (21 rows at most) whose display name or any
    /// address contains `query`, for a server owner.
    /// # Errors
    /// Authority, an invalid offset, database failures.
    pub async fn browser_users(
        &self,
        session: &WebSession,
        query: &str,
        offset: i64,
    ) -> Result<Vec<UserRow>> {
        crate::web_admin::valid_offset(offset)?;
        let mut tx = self.browser_write_tx().await?;
        Self::server_owner_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)?;
        let rows = sqlx::query("SELECT u.id, substr(u.display_name,1,256) AS display_name, u.is_server_owner, u.created_at, (SELECT a.email FROM addresses a WHERE a.user_id=u.id ORDER BY a.verified_on IS NULL, a.registered_on, a.email LIMIT 1) AS email FROM users u WHERE lower(u.display_name) LIKE $1 ESCAPE '!' OR EXISTS (SELECT 1 FROM addresses a WHERE a.user_id=u.id AND a.email LIKE $1 ESCAPE '!') ORDER BY u.created_at, u.id LIMIT 21 OFFSET $2")
            .bind(escape_like(query.trim()))
            .bind(offset)
            .fetch_all(self.pool())
            .await
            .map_err(db_error)?;
        rows.iter()
            .map(|row| {
                Ok(UserRow {
                    id: row
                        .try_get::<String, _>("id")
                        .map_err(db_error)?
                        .parse()
                        .map_err(db_error)?,
                    display_name: row.try_get("display_name").map_err(db_error)?,
                    email: row
                        .try_get::<Option<String>, _>("email")
                        .map_err(db_error)?
                        .unwrap_or_default(),
                    server_owner: row.try_get::<i64, _>("is_server_owner").map_err(db_error)? != 0,
                    created_at: row.try_get("created_at").map_err(db_error)?,
                })
            })
            .collect()
    }

    /// One account with its addresses and memberships, for a server owner.
    /// # Errors
    /// Authority, `NotFound`, database failures.
    pub async fn browser_user(&self, session: &WebSession, id: UserId) -> Result<UserDetail> {
        let mut tx = self.browser_write_tx().await?;
        Self::server_owner_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)?;
        let user = self.users().get(id).await?;
        let addresses = self.addresses().by_user(id).await?;
        let rows = sqlx::query("SELECT m.id, m.list_id, m.role, a.email FROM members m JOIN addresses a ON a.id=m.address_id WHERE a.user_id=$1 ORDER BY m.list_id, m.role, a.email")
            .bind(id.to_string())
            .fetch_all(self.pool())
            .await
            .map_err(db_error)?;
        let memberships = rows
            .iter()
            .map(|row| {
                Ok(UserMembership {
                    member_id: row.try_get("id").map_err(db_error)?,
                    list_id: row.try_get("list_id").map_err(db_error)?,
                    role: row
                        .try_get::<String, _>("role")
                        .map_err(db_error)?
                        .parse()?,
                    email: row.try_get("email").map_err(db_error)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(UserDetail {
            user,
            addresses,
            memberships,
        })
    }

    /// Sets an account's display name and server-owner flag in one audited
    /// transaction. Demoting the last server owner with a verified address
    /// is refused, so the site keeps an administrator.
    /// # Errors
    /// Authority, `NotFound`, `Validation` for a bad name or the last owner,
    /// database or audit failures.
    pub async fn browser_user_update(
        &self,
        session: &WebSession,
        id: UserId,
        display_name: &str,
        server_owner: bool,
    ) -> Result<()> {
        let display_name = display_name.trim();
        if display_name.is_empty()
            || display_name.len() > 256
            || display_name.chars().any(char::is_control)
        {
            return Err(Error::Validation("display name".into()));
        }
        let mut tx = self.browser_write_tx().await?;
        let actor = Self::server_owner_tx(&mut tx, session).await?;
        let current: Option<i64> =
            sqlx::query_scalar("SELECT is_server_owner FROM users WHERE id=$1")
                .bind(id.to_string())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_error)?;
        let Some(current) = current else {
            return Err(Error::NotFound(id.to_string()));
        };
        if current == 1 && !server_owner {
            let others: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users u WHERE u.is_server_owner=1 AND u.id<>$1 AND EXISTS (SELECT 1 FROM addresses a WHERE a.user_id=u.id AND a.verified_on IS NOT NULL)")
                .bind(id.to_string())
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?;
            if others == 0 {
                return Err(Error::Validation(
                    "the last server owner cannot be demoted".into(),
                ));
            }
        }
        sqlx::query("UPDATE users SET display_name=$1, is_server_owner=$2 WHERE id=$3")
            .bind(display_name)
            .bind(i64::from(server_owner))
            .bind(id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        Self::record_tx_with_context(
            &mut tx,
            &AuditContext::new(Some(actor), None, None),
            "user.update",
            "user",
            &id.to_string(),
            serde_json::json!({"display_name": display_name, "is_server_owner": server_owner, "source": "browser-admin"}),
        )
        .await?;
        Self::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)
    }

    /// Marks one of the account's addresses verified or not, without a
    /// mailbox proof, in one audited transaction.
    /// # Errors
    /// Authority, `NotFound` when the address is not the account's, database
    /// or audit failures.
    pub async fn browser_user_address_verify(
        &self,
        session: &WebSession,
        id: UserId,
        address: AddressId,
        verified: bool,
    ) -> Result<()> {
        let mut tx = self.browser_write_tx().await?;
        let actor = Self::server_owner_tx(&mut tx, session).await?;
        let email: Option<String> =
            sqlx::query_scalar("SELECT email FROM addresses WHERE id=$1 AND user_id=$2")
                .bind(address.to_string())
                .bind(id.to_string())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_error)?;
        let Some(email) = email else {
            return Err(Error::NotFound("address".into()));
        };
        sqlx::query("UPDATE addresses SET verified_on=$1 WHERE id=$2")
            .bind(verified.then(|| chrono::Utc::now().to_rfc3339()))
            .bind(address.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        Self::record_tx_with_context(
            &mut tx,
            &AuditContext::new(Some(actor), None, None),
            if verified {
                "address.verify"
            } else {
                "address.unverify"
            },
            "address",
            &email,
            serde_json::json!({"verified": verified, "source": "browser-admin"}),
        )
        .await?;
        Self::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)
    }
}
