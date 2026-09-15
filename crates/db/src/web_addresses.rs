//! The reader's own addresses: adding one (proven from its mailbox), choosing
//! the primary one, and letting one go.
//!
//! Adding never reveals whether an address belongs to someone else: a foreign
//! address links nothing and mails nothing. Removing unlinks the address and
//! forgets its verification, so whoever claims it next must prove it again;
//! the address row and its memberships stay, since the mailbox is the same.
use crate::web_sessions::WebSession;
use crate::{AuditContext, Database, db_error};
use listmngr_core::{Address, Error, Result};
use sqlx::Row;

/// One address of the signed-in reader.
#[derive(Debug, Clone)]
pub struct OwnAddress {
    /// Address id, safe to put in a form.
    pub id: String,
    /// Normalized address.
    pub email: String,
    /// Whether it has been proven from its mailbox.
    pub verified: bool,
    /// Whether it is the account's primary address.
    pub primary: bool,
}

fn not_ours() -> Error {
    Error::NotFound("address".into())
}

impl Database {
    /// The reader's own addresses, primary first, then by registration.
    /// # Errors
    /// A stale or anonymous session; database errors.
    pub async fn browser_addresses(&self, session: &WebSession) -> Result<Vec<OwnAddress>> {
        let live = self
            .web_session(&session.token, chrono::Utc::now().timestamp_millis())
            .await?;
        let user = live.user_id.ok_or(Error::Authentication)?;
        let rows = sqlx::query(
            "SELECT a.id, a.email, a.verified_on,
                    CASE WHEN a.id = u.preferred_address_id THEN 1 ELSE 0 END AS is_primary
             FROM addresses a JOIN users u ON u.id=a.user_id
             WHERE a.user_id=$1
             ORDER BY is_primary DESC, a.registered_on, a.id",
        )
        .bind(user.to_string())
        .fetch_all(self.pool())
        .await
        .map_err(db_error)?;
        rows.into_iter()
            .map(|row| {
                let verified: Option<String> = row.try_get("verified_on").map_err(db_error)?;
                let primary: i64 = row.try_get("is_primary").map_err(db_error)?;
                Ok(OwnAddress {
                    id: row.try_get("id").map_err(db_error)?,
                    email: row.try_get("email").map_err(db_error)?,
                    verified: verified.is_some(),
                    primary: primary == 1,
                })
            })
            .collect()
    }

    /// Add `email` to the account and mail a verification token — or, for an
    /// address the account already holds unverified, mail a fresh one. An
    /// address another account owns changes nothing.
    /// # Errors
    /// An invalid mailbox, a stale session or failed CSRF binding, or a
    /// database, template or audit failure — nothing is written on error.
    pub async fn browser_add_address(
        &self,
        session: &WebSession,
        email: &str,
        language: &str,
        now_ms: i64,
    ) -> Result<()> {
        let address = Address::new(email, String::new())?;
        crate::workflows::notice_mailbox(&address.email)?;
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_user_tx(&mut tx, session).await?;
        let existing = sqlx::query("SELECT id,user_id,verified_on FROM addresses WHERE email=$1")
            .bind(&address.email)
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_error)?;
        let (address_id, linked) = match existing {
            None => {
                sqlx::query("INSERT INTO addresses(id,email,original_email,display_name,user_id,registered_on) VALUES($1,$2,$3,'',$4,$5)")
                    .bind(address.id.to_string()).bind(&address.email).bind(&address.original_email).bind(user.to_string()).bind(address.registered_on.to_rfc3339())
                    .execute(&mut *tx).await.map_err(db_error)?;
                (address.id.to_string(), true)
            }
            Some(row) => {
                let id: String = row.try_get("id").map_err(db_error)?;
                let owner: Option<String> = row.try_get("user_id").map_err(db_error)?;
                let verified: Option<String> = row.try_get("verified_on").map_err(db_error)?;
                match owner {
                    Some(owner) if owner == user.to_string() => {
                        if verified.is_some() {
                            return tx.commit().await.map_err(db_error);
                        }
                        (id, false)
                    }
                    Some(_) => return tx.commit().await.map_err(db_error),
                    None => {
                        sqlx::query("UPDATE addresses SET user_id=$1,verified_on=NULL WHERE id=$2")
                            .bind(user.to_string())
                            .bind(&id)
                            .execute(&mut *tx)
                            .await
                            .map_err(db_error)?;
                        (id, true)
                    }
                }
            }
        };
        let mailed = self
            .issue_verification(&mut tx, &address_id, &address, language, now_ms)
            .await?;
        if linked || mailed {
            Self::record_tx_with_context(
                &mut tx,
                &AuditContext::new(Some(user), None, None),
                "address.add",
                "address",
                &address.email,
                serde_json::json!({"linked": linked, "verification_mailed": mailed}),
            )
            .await?;
        }
        tx.commit().await.map_err(db_error)
    }

    /// Make one of the reader's verified addresses the primary one.
    /// # Errors
    /// An address that is not the reader's (not found), an unverified one
    /// (validation), a stale session or failed CSRF binding, or audit failure.
    pub async fn browser_primary_address(&self, session: &WebSession, id: &str) -> Result<()> {
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_user_tx(&mut tx, session).await?;
        let verified: Option<Option<String>> =
            sqlx::query_scalar("SELECT verified_on FROM addresses WHERE id=$1 AND user_id=$2")
                .bind(id)
                .bind(user.to_string())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_error)?;
        match verified {
            None => return Err(not_ours()),
            Some(None) => {
                return Err(Error::Validation(
                    "only a verified address can be primary".into(),
                ));
            }
            Some(Some(_)) => {}
        }
        sqlx::query("UPDATE users SET preferred_address_id=$1 WHERE id=$2")
            .bind(id)
            .bind(user.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        Self::record_tx_with_context(
            &mut tx,
            &AuditContext::new(Some(user), None, None),
            "address.primary",
            "user",
            &user.to_string(),
            serde_json::json!({"address_id": id}),
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }

    /// Unlink one of the reader's addresses and forget its verification. The
    /// primary address and the last verified address stay.
    /// # Errors
    /// An address that is not the reader's (not found); the primary or the
    /// last verified address (validation); a stale session or failed CSRF
    /// binding; or audit failure.
    pub async fn browser_remove_address(&self, session: &WebSession, id: &str) -> Result<()> {
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_user_tx(&mut tx, session).await?;
        let row = sqlx::query(
            "SELECT a.email, a.verified_on,
                    CASE WHEN a.id = u.preferred_address_id THEN 1 ELSE 0 END AS is_primary
             FROM addresses a JOIN users u ON u.id=a.user_id WHERE a.id=$1 AND a.user_id=$2",
        )
        .bind(id)
        .bind(user.to_string())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?
        .ok_or_else(not_ours)?;
        let email: String = row.try_get("email").map_err(db_error)?;
        let verified: Option<String> = row.try_get("verified_on").map_err(db_error)?;
        let primary: i64 = row.try_get("is_primary").map_err(db_error)?;
        if primary == 1 {
            return Err(Error::Validation(
                "choose another primary address first".into(),
            ));
        }
        if verified.is_some() {
            let others: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM addresses WHERE user_id=$1 AND verified_on IS NOT NULL AND id<>$2",
            )
            .bind(user.to_string())
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(db_error)?;
            if others == 0 {
                return Err(Error::Validation(
                    "the last verified address cannot be removed".into(),
                ));
            }
        }
        sqlx::query("UPDATE addresses SET user_id=NULL,verified_on=NULL WHERE id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        sqlx::query("DELETE FROM account_tokens WHERE address_id=$1 AND consumed_at IS NULL")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        Self::record_tx_with_context(
            &mut tx,
            &AuditContext::new(Some(user), None, None),
            "address.remove",
            "address",
            &email,
            serde_json::json!({"unlinked": true, "verification": "forgotten"}),
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }
}
