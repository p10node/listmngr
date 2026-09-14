//! A reader's own browser sessions: what exists, and ending one or all others.
//!
//! Every row is addressed by an opaque id, never by the token digest that
//! authenticates it, and every revocation commits with its audit event under
//! the same live-session authority the other browser writes use.
use crate::web_sessions::WebSession;
use crate::{AuditContext, Database, db_error};
use listmngr_core::{Error, Result};
use sqlx::Row;
use uuid::Uuid;

/// One browser session of the signed-in reader.
#[derive(Debug, Clone)]
pub struct SessionSummary {
    /// Opaque identifier, safe to put in a page and a form.
    pub id: String,
    /// When the session was issued, as Unix milliseconds.
    pub created_at: i64,
    /// When it expires without further use, as Unix milliseconds.
    pub expires_at: i64,
    /// Whether this is the session making the request.
    pub current: bool,
}

impl Database {
    /// The reader's own live sessions, newest first.
    /// # Errors
    /// Rejects a stale or unauthenticated session; returns database errors.
    pub async fn browser_sessions(&self, session: &WebSession) -> Result<Vec<SessionSummary>> {
        let live = self
            .web_session(&session.token, chrono::Utc::now().timestamp_millis())
            .await?;
        let user = live.user_id.ok_or(Error::Authentication)?;
        let rows = sqlx::query(
            "SELECT id,created_at,expires_at,token_hash FROM web_sessions
             WHERE user_id=$1 AND expires_at>$2 AND id IS NOT NULL
             ORDER BY created_at DESC,id DESC LIMIT 100",
        )
        .bind(user.to_string())
        .bind(chrono::Utc::now().timestamp_millis())
        .fetch_all(self.pool())
        .await
        .map_err(db_error)?;
        let current = crate::web_sessions::digest(&session.token);
        rows.into_iter()
            .map(|row| {
                let hash: String = row.try_get("token_hash").map_err(db_error)?;
                Ok(SessionSummary {
                    id: row.try_get("id").map_err(db_error)?,
                    created_at: row.try_get("created_at").map_err(db_error)?,
                    expires_at: row.try_get("expires_at").map_err(db_error)?,
                    current: hash == current,
                })
            })
            .collect()
    }

    /// End one of the reader's own sessions, by its opaque id.
    ///
    /// Returns whether the session ended was the one making the request, so the
    /// caller can clear its cookie.
    /// # Errors
    /// Rejects a stale session, a failed CSRF binding, an id that is not this
    /// reader's, or audit failure.
    pub async fn browser_revoke_session(&self, session: &WebSession, id: &str) -> Result<bool> {
        if id.len() > 64 || !id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
            return Err(Error::NotFound("session".into()));
        }
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_user_tx(&mut tx, session).await?;
        let current = crate::web_sessions::digest(&session.token);
        let hash: Option<String> =
            sqlx::query_scalar("SELECT token_hash FROM web_sessions WHERE id=$1 AND user_id=$2")
                .bind(id)
                .bind(user.to_string())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_error)?;
        let hash = hash.ok_or_else(|| Error::NotFound("session".into()))?;
        let removed = sqlx::query("DELETE FROM web_sessions WHERE id=$1 AND user_id=$2")
            .bind(id)
            .bind(user.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?
            .rows_affected();
        if removed != 1 {
            return Err(Error::NotFound("session".into()));
        }
        let was_current = hash == current;
        Self::record_tx_with_context(
            &mut tx,
            &AuditContext::new(Some(user), None, None),
            "web.session.revoke",
            "user",
            &user.to_string(),
            serde_json::json!({"sessions":1,"included_current":was_current}),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(was_current)
    }

    /// End every session of the reader except the one making the request.
    /// # Errors
    /// Rejects a stale session or a failed CSRF binding; returns audit and
    /// database errors.
    pub async fn browser_revoke_other_sessions(&self, session: &WebSession) -> Result<u64> {
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_user_tx(&mut tx, session).await?;
        let removed = sqlx::query("DELETE FROM web_sessions WHERE user_id=$1 AND token_hash<>$2")
            .bind(user.to_string())
            .bind(crate::web_sessions::digest(&session.token))
            .execute(&mut *tx)
            .await
            .map_err(db_error)?
            .rows_affected();
        Self::record_tx_with_context(
            &mut tx,
            &AuditContext::new(Some(user), None, None),
            "web.session.revoke",
            "user",
            &user.to_string(),
            serde_json::json!({"sessions":removed,"included_current":false}),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(removed)
    }
}

/// A fresh opaque identifier for a session row.
pub fn session_id() -> String {
    Uuid::now_v7().to_string()
}
