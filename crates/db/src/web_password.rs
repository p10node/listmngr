//! Password change keeps live session authority, credential CAS, revocation and audit atomic.
#[cfg(test)]
#[path = "web_password_tests.rs"]
mod tests;
use super::{WebSession, digest};
use crate::{AuditContext, Database, db_error};
use argon2::{
    PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{SaltString, rand_core::OsRng},
};
use listmngr_core::{Error, Result};
use sqlx::Row;

impl Database {
    /// Change the authenticated user's password and revoke all their browser sessions.
    /// # Errors
    /// Rejects invalid passwords, stale credentials/session authority or audit failure.
    pub async fn browser_change_password(
        &self,
        session: &WebSession,
        current: &str,
        replacement: &str,
    ) -> Result<()> {
        let user = session.user_id.ok_or(Error::Authentication)?;
        if current.len() > 1024 {
            return Err(Error::Authentication);
        }
        self.users().validate_password(replacement)?;
        let live = self
            .web_session(&session.token, chrono::Utc::now().timestamp_millis())
            .await?;
        if live.user_id != Some(user) || !live.verifies_csrf(&session.csrf) {
            return Err(Error::Authentication);
        }
        let row = sqlx::query(
            "SELECT password_hash,password_updated_at FROM user_credentials WHERE user_id=$1",
        )
        .bind(user.to_string())
        .fetch_one(self.pool())
        .await
        .map_err(db_error)?;
        let old_hash: String = row.try_get("password_hash").map_err(db_error)?;
        let old_version: String = row.try_get("password_updated_at").map_err(db_error)?;
        let hasher = self.users().password_hasher()?;
        let parsed = PasswordHash::new(&old_hash).map_err(|_| Error::Authentication)?;
        hasher
            .verify_password(current.as_bytes(), &parsed)
            .map_err(|_| Error::Authentication)?;
        let new_hash = hasher
            .hash_password(replacement.as_bytes(), &SaltString::generate(&mut OsRng))
            .map_err(db_error)?
            .to_string();
        // No Argon2 work under the writer lock. Proof is rebound after acquisition.
        let mut tx = self.browser_write_tx().await?;
        if Self::browser_user_tx(&mut tx, session).await? != user {
            return Err(Error::Authentication);
        }
        let verified: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM addresses WHERE user_id=$1 AND verified_on IS NOT NULL",
        )
        .bind(user.to_string())
        .fetch_one(&mut *tx)
        .await
        .map_err(db_error)?;
        if verified == 0 {
            return Err(Error::Authentication);
        }
        let expires: i64 =
            sqlx::query_scalar("SELECT expires_at FROM web_sessions WHERE token_hash=$1")
                .bind(digest(&session.token))
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?;
        let changed = sqlx::query("UPDATE user_credentials SET password_hash=$1,password_updated_at=$2 WHERE user_id=$3 AND password_hash=$4 AND password_updated_at=$5")
            .bind(new_hash).bind(crate::now()).bind(user.to_string()).bind(old_hash).bind(old_version)
            .execute(&mut *tx).await.map_err(db_error)?.rows_affected();
        if changed != 1 {
            return Err(Error::Authentication);
        }
        sqlx::query("DELETE FROM web_sessions WHERE user_id=$1")
            .bind(user.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        Self::record_tx_with_context(
            &mut tx,
            &AuditContext::new(Some(user), None, None),
            "user.password",
            "user",
            &user.to_string(),
            serde_json::json!({"password":"changed","browser_sessions":"revoked"}),
        )
        .await?;
        if chrono::Utc::now().timestamp_millis() >= expires {
            return Err(Error::Authentication);
        }
        tx.commit().await.map_err(db_error)
    }
}
