//! Short-lived browser sessions: only opaque token digests are persisted.
#[path = "web_membership.rs"]
mod membership;
#[path = "web_password.rs"]
mod password;
use crate::{AuditContext, Database, db_error};
use base64::Engine;
use listmngr_core::{Error, Result, UserId};
use rand::RngCore;
use sha2::{Digest, Sha256};
use sqlx::Row;
use subtle::ConstantTimeEq;

/// Authentication material deliberately has no Debug implementation.
#[allow(missing_debug_implementations)]
pub struct WebSession {
    pub token: String,
    pub csrf: String,
    pub user_id: Option<UserId>,
}
fn digest(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}
fn secret() -> String {
    let mut b = [0_u8; 32];
    rand::rng().fill_bytes(&mut b);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}
impl WebSession {
    #[must_use]
    pub fn verifies_csrf(&self, value: &str) -> bool {
        bool::from(self.csrf.as_bytes().ct_eq(value.as_bytes()))
    }
}
// Private, non-Debug proof: only the password verifier can construct it.
struct VerifiedBrowserLogin {
    user: UserId,
    email: String,
    hash: String,
    version: String,
}

impl Database {
    /// Verify a browser password and rotate its live CSRF session.
    /// # Errors
    /// Rejects invalid credentials, revoked authority, or database failures.
    pub async fn browser_login(
        &self,
        email: &str,
        password: &str,
        previous: &WebSession,
    ) -> Result<WebSession> {
        let proof = self.verify_browser_login(email, password).await?;
        self.issue_browser_login(proof, previous).await
    }

    async fn verify_browser_login(
        &self,
        email: &str,
        password: &str,
    ) -> Result<VerifiedBrowserLogin> {
        use argon2::{PasswordHash, PasswordVerifier};
        if password.len() > 1024 {
            return Err(Error::Authentication);
        }
        let user = self
            .users()
            .get_by_email(email)
            .await
            .map_err(|_| Error::Authentication)?;
        let row = sqlx::query(
            "SELECT password_hash,password_updated_at FROM user_credentials WHERE user_id=$1",
        )
        .bind(user.id.to_string())
        .fetch_optional(self.pool())
        .await
        .map_err(db_error)?
        .ok_or(Error::Authentication)?;
        let hash: String = row.try_get("password_hash").map_err(db_error)?;
        let version: String = row.try_get("password_updated_at").map_err(db_error)?;
        if !PasswordHash::new(&hash).ok().is_some_and(|parsed| {
            self.users().password_hasher().is_ok_and(|verifier| {
                verifier
                    .verify_password(password.as_bytes(), &parsed)
                    .is_ok()
            })
        }) {
            return Err(Error::Authentication);
        }
        let address = self.addresses().get(email).await?;
        if address.verified_on.is_none() || address.user_id != Some(user.id) {
            return Err(Error::Authentication);
        }
        Ok(VerifiedBrowserLogin {
            user: user.id,
            email: address.email,
            hash,
            version,
        })
    }

    async fn issue_browser_login(
        &self,
        proof: VerifiedBrowserLogin,
        previous: &WebSession,
    ) -> Result<WebSession> {
        // Argon2 has already finished. Reserve the writer BEFORE re-reading
        // authority; ordinary password/address/session DML must conflict here.
        let mut tx = self.browser_write_tx().await?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        let authorized: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM user_credentials c JOIN users u ON u.id=c.user_id JOIN addresses a ON a.user_id=u.id WHERE u.id=$1 AND c.password_hash=$2 AND c.password_updated_at=$3 AND a.email=$4 AND a.verified_on IS NOT NULL")
            .bind(proof.user.to_string()).bind(&proof.hash).bind(&proof.version).bind(&proof.email)
            .fetch_one(&mut *tx).await.map_err(db_error)?;
        let predecessor = sqlx::query("SELECT s.csrf,s.user_id FROM web_sessions s LEFT JOIN user_credentials c ON c.user_id=s.user_id WHERE s.token_hash=$1 AND s.expires_at>$2 AND (s.user_id IS NULL OR s.credential_version=c.password_updated_at)")
            .bind(digest(&previous.token)).bind(now_ms).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or(Error::Authentication)?;
        let csrf: String = predecessor.try_get("csrf").map_err(db_error)?;
        let user: Option<String> = predecessor.try_get("user_id").map_err(db_error)?;
        if authorized != 1
            || !previous.verifies_csrf(&csrf)
            || user != previous.user_id.map(|u| u.to_string())
        {
            return Err(Error::Authentication);
        }
        let token = secret();
        let csrf = secret();
        sqlx::query("DELETE FROM web_sessions WHERE token_hash IN (SELECT token_hash FROM web_sessions WHERE expires_at <= $1 ORDER BY expires_at,token_hash LIMIT 100)")
            .bind(now_ms).execute(&mut *tx).await.map_err(db_error)?;
        sqlx::query("DELETE FROM web_sessions WHERE token_hash=$1")
            .bind(digest(&previous.token))
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        sqlx::query("INSERT INTO web_sessions(token_hash,csrf,user_id,credential_version,expires_at) VALUES($1,$2,$3,$4,$5)")
            .bind(digest(&token)).bind(&csrf).bind(proof.user.to_string()).bind(&proof.version).bind(now_ms+28_800_000).execute(&mut *tx).await.map_err(db_error)?;
        Self::record_tx_with_context(
            &mut tx,
            &AuditContext::new(Some(proof.user), None, None),
            "web.login",
            "user",
            &proof.user.to_string(),
            serde_json::json!({}),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(WebSession {
            token,
            csrf,
            user_id: Some(proof.user),
        })
    }

    /// Create or rotate a session, atomically revoking its predecessor and auditing.
    /// # Errors
    /// Returns database errors or missing credentials.
    pub async fn create_web_session(
        &self,
        user: Option<UserId>,
        previous: Option<&str>,
        now_ms: i64,
    ) -> Result<WebSession> {
        let token = secret();
        let csrf = secret();
        let mut tx = self.pool().begin().await.map_err(db_error)?;
        let version: Option<String> = if let Some(u) = user {
            Some(
                sqlx::query_scalar(
                    "SELECT password_updated_at FROM user_credentials WHERE user_id=$1",
                )
                .bind(u.to_string())
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?,
            )
        } else {
            None
        };
        sqlx::query("DELETE FROM web_sessions WHERE token_hash IN (SELECT token_hash FROM web_sessions WHERE expires_at <= $1 ORDER BY expires_at,token_hash LIMIT 100)")
            .bind(now_ms)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        if let Some(old) = previous {
            sqlx::query("DELETE FROM web_sessions WHERE token_hash=$1")
                .bind(digest(old))
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
        }
        sqlx::query("INSERT INTO web_sessions(token_hash,csrf,user_id,credential_version,expires_at) VALUES($1,$2,$3,$4,$5)").bind(digest(&token)).bind(&csrf).bind(user.map(|u|u.to_string())).bind(version).bind(now_ms+if user.is_some(){28_800_000}else{1_800_000}).execute(&mut *tx).await.map_err(db_error)?;
        if let Some(user_id) = user {
            Self::record_tx_with_context(
                &mut tx,
                &AuditContext::new(user, None, None),
                "web.login",
                "user",
                &user_id.to_string(),
                serde_json::json!({}),
            )
            .await?;
        }
        tx.commit().await.map_err(db_error)?;
        Ok(WebSession {
            token,
            csrf,
            user_id: user,
        })
    }
    /// Look up an unexpired token, rejecting sessions after password changes.
    /// # Errors
    /// Returns authentication or database errors.
    pub async fn web_session(&self, token: &str, now_ms: i64) -> Result<WebSession> {
        if token.len() != 43 {
            return Err(Error::Authentication);
        }
        // Evaluate the clock after waiting for a pooled connection, not before it.
        let mut connection = self.pool().acquire().await.map_err(db_error)?;
        let effective_now = now_ms.max(chrono::Utc::now().timestamp_millis());
        let row=sqlx::query("SELECT s.csrf,s.user_id FROM web_sessions s LEFT JOIN user_credentials c ON c.user_id=s.user_id WHERE s.token_hash=$1 AND s.expires_at>$2 AND (s.user_id IS NULL OR s.credential_version=c.password_updated_at)").bind(digest(token)).bind(effective_now).fetch_optional(&mut *connection).await.map_err(db_error)?.ok_or(Error::Authentication)?;
        let user: Option<String> = row.try_get("user_id").map_err(db_error)?;
        Ok(WebSession {
            token: token.into(),
            csrf: row.try_get("csrf").map_err(db_error)?,
            user_id: user.map(|u| u.parse().map_err(db_error)).transpose()?,
        })
    }
    /// Revoke a session and audit logout atomically.
    /// # Errors
    /// Returns database errors.
    pub async fn delete_web_session(&self, session: &WebSession) -> Result<()> {
        let mut tx = self.pool().begin().await.map_err(db_error)?;
        sqlx::query("DELETE FROM web_sessions WHERE token_hash=$1")
            .bind(digest(&session.token))
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        Self::record_tx_with_context(
            &mut tx,
            &AuditContext::new(session.user_id, None, None),
            "web.logout",
            "user",
            &session.user_id.map(|u| u.to_string()).unwrap_or_default(),
            serde_json::json!({}),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(())
    }
}

impl Database {
    /// Acquire the browser write serialization boundary before reading authority.
    /// `SQLite` obtains its single writer reservation before creating a snapshot.
    /// `PostgreSQL` table locks conflict with ordinary DML, including revocation
    /// SQL issued outside these repositories. NOWAIT plus whole-transaction
    /// rollback avoids hold-and-wait cycles with writers using other table orders.
    /// Locks remain held through the business write and audit commit.
    pub(crate) async fn browser_write_tx(&self) -> Result<sqlx::Transaction<'_, sqlx::Any>> {
        let sqlite = self
            .pool()
            .acquire()
            .await
            .map_err(db_error)?
            .backend_name()
            == "SQLite";
        if sqlite {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                match self.pool().begin_with("BEGIN IMMEDIATE").await {
                    Ok(tx) => return Ok(tx),
                    Err(error) => {
                        let busy = error
                            .as_database_error()
                            .and_then(sqlx::error::DatabaseError::code)
                            .is_some_and(|code| matches!(code.as_ref(), "5" | "6"));
                        if !busy || std::time::Instant::now() >= deadline {
                            return Err(db_error(error));
                        }
                        #[cfg(test)]
                        let _ = BROWSER_LOCK_BUSY.try_with(|sender| {
                            let _ = sender.send(());
                        });
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                }
            }
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let mut tx = self
                .pool()
                .begin_with("BEGIN ISOLATION LEVEL READ COMMITTED")
                .await
                .map_err(db_error)?;
            match sqlx::query("LOCK TABLE web_sessions,user_credentials,users,addresses,members,preferences,mailing_lists,held_messages IN SHARE ROW EXCLUSIVE MODE NOWAIT").execute(&mut *tx).await {
                Ok(_) => return Ok(tx),
                Err(error) => {
                    let busy = error.as_database_error().and_then(sqlx::error::DatabaseError::code).is_some_and(|code| code == "55P03");
                    tx.rollback().await.map_err(db_error)?;
                    if !busy || std::time::Instant::now() >= deadline { return Err(db_error(error)); }
                    #[cfg(test)]
                    let _ = BROWSER_LOCK_BUSY.try_with(|sender| { let _ = sender.send(()); });
                    // No transaction/authorization locks are held while backing off.
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            }
        }
    }

    pub(crate) async fn browser_user_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Any>,
        session: &WebSession,
    ) -> Result<UserId> {
        // Use wall clock AFTER every acquisition/retry. The earlier HTTP lookup
        // is not authority, and caller-constructed WebSession fields are untrusted.
        let row = sqlx::query("SELECT s.csrf,s.user_id FROM web_sessions s JOIN user_credentials c ON c.user_id=s.user_id WHERE s.token_hash=$1 AND s.expires_at>$2 AND s.credential_version=c.password_updated_at")
            .bind(digest(&session.token)).bind(chrono::Utc::now().timestamp_millis())
            .fetch_optional(&mut **tx).await.map_err(db_error)?.ok_or(Error::Authentication)?;
        let csrf: String = row.try_get("csrf").map_err(db_error)?;
        let user: String = row.try_get("user_id").map_err(db_error)?;
        let user: UserId = user.parse().map_err(db_error)?;
        if !bool::from(csrf.as_bytes().ct_eq(session.csrf.as_bytes()))
            || Some(user) != session.user_id
        {
            return Err(Error::Authentication);
        }
        Ok(user)
    }

    /// Set browser delivery preferences with live authority and policy in the
    /// same serialized transaction as the preference update and audit event.
    /// # Errors
    /// Rejects revoked/expired sessions, changed ownership or restricted policy.
    pub async fn browser_preferences(
        &self,
        session: &WebSession,
        member: listmngr_core::MemberId,
        mode: listmngr_core::DeliveryMode,
        status: listmngr_core::DeliveryStatus,
    ) -> Result<()> {
        self.browser_preferences_with_own_postings(session, member, mode, status, None)
            .await
    }

    /// Update membership delivery preferences; an omitted own-postings value
    /// preserves the current stored override and the legacy audit shape.
    /// # Errors
    /// Rejects revoked/expired sessions, changed ownership or restricted policy.
    pub async fn browser_preferences_with_own_postings(
        &self,
        session: &WebSession,
        member: listmngr_core::MemberId,
        mode: listmngr_core::DeliveryMode,
        status: listmngr_core::DeliveryStatus,
        receive_own_postings: Option<bool>,
    ) -> Result<()> {
        self.browser_preferences_with_options(
            session,
            member,
            mode,
            status,
            receive_own_postings,
            None,
        )
        .await
    }

    /// Update only member delivery options. Omitted booleans preserve stored NULL
    /// or explicit overrides.
    ///
    /// Omitted options do not add keys to the legacy audit diff.
    /// # Errors
    /// Rejects revoked/expired sessions, changed ownership or restricted policy.
    pub async fn browser_preferences_with_options(
        &self,
        session: &WebSession,
        member: listmngr_core::MemberId,
        mode: listmngr_core::DeliveryMode,
        status: listmngr_core::DeliveryStatus,
        receive_own_postings: Option<bool>,
        receive_list_copy: Option<bool>,
    ) -> Result<()> {
        use listmngr_core::{DeliveryMode, DeliveryStatus};
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_user_tx(&mut tx, session).await?;
        let row = sqlx::query("SELECT m.preferences_id,COALESCE(mp.delivery_status,ap.delivery_status,up.delivery_status,'enabled') AS delivery_status FROM members m JOIN addresses a ON a.id=m.address_id JOIN preferences mp ON mp.id=m.preferences_id LEFT JOIN preferences ap ON ap.id=a.preferences_id LEFT JOIN users u ON u.id=m.user_id LEFT JOIN preferences up ON up.id=u.preferences_id WHERE m.id=$1 AND m.role='member' AND a.user_id=$2 AND a.verified_on IS NOT NULL AND (m.subscription_mode='as_address' OR m.user_id=$2)")
            .bind(member.to_string()).bind(user.to_string()).fetch_optional(&mut *tx).await.map_err(db_error)?
            .ok_or_else(|| Error::Forbidden("browser member ownership".into()))?;
        let current: String = row.try_get("delivery_status").map_err(db_error)?;
        if !matches!(status, DeliveryStatus::Enabled | DeliveryStatus::ByUser)
            || mode == DeliveryMode::SummaryDigests
            || !matches!(current.as_str(), "enabled" | "by_user")
        {
            return Err(Error::Forbidden("browser preference policy".into()));
        }
        let preference: String = row.try_get("preferences_id").map_err(db_error)?;
        sqlx::query("UPDATE preferences SET delivery_mode=$1,delivery_status=$2,receive_own_postings=COALESCE($4,receive_own_postings),receive_list_copy=COALESCE($5,receive_list_copy),delivery_generation=delivery_generation+1 WHERE id=$3")
            .bind(mode.to_string())
            .bind(status.to_string())
            .bind(&preference)
            .bind(receive_own_postings.map(i32::from))
            .bind(receive_list_copy.map(i32::from))
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        let mut diff = serde_json::json!({"delivery_mode":mode,"delivery_status":status});
        if let Some(copy) = receive_list_copy {
            diff["receive_list_copy"] = serde_json::json!(copy);
        }
        if let Some(own) = receive_own_postings {
            diff["receive_own_postings"] = serde_json::json!(own);
        }
        Self::record_tx_with_context(
            &mut tx,
            &AuditContext::new(Some(user), None, None),
            "preferences.update",
            "preferences",
            &preference,
            diff,
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }

    /// Review held mail with live session, identity and list authority, atomically
    /// with the existing disposition, Out child, moderation log and audit logic.
    /// # Errors
    /// Rejects revoked authority, wrong-list held IDs, or failed mutations.
    pub async fn browser_review(
        &self,
        session: &WebSession,
        list: &listmngr_core::ListId,
        held: crate::moderation::HeldId,
        action: &crate::moderation::ReviewAction,
        reason: &str,
    ) -> Result<()> {
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_user_tx(&mut tx, session).await?;
        let allowed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users u WHERE u.id=$1 AND ((u.is_server_owner=1 AND EXISTS (SELECT 1 FROM addresses a WHERE a.user_id=u.id AND a.verified_on IS NOT NULL)) OR EXISTS (SELECT 1 FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id=$2 AND a.user_id=u.id AND a.verified_on IS NOT NULL AND (m.subscription_mode='as_address' OR m.user_id=u.id) AND m.role IN ('owner','moderator')))")
            .bind(user.to_string()).bind(list.as_str()).fetch_one(&mut *tx).await.map_err(db_error)?;
        if allowed != 1 {
            return Err(Error::Forbidden("browser moderator authority".into()));
        }
        let matches: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM held_messages WHERE id=$1 AND list_id=$2")
                .bind(held.0.to_string())
                .bind(list.as_str())
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?;
        if matches != 1 {
            return Err(Error::NotFound("held message".into()));
        }
        crate::moderation::ModerationRepo::review_tx(
            &mut tx,
            self,
            held,
            &AuditContext::new(Some(user), None, None),
            action,
            reason,
            None,
            chrono::Utc::now().timestamp_millis(),
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }
}

#[cfg(test)]
tokio::task_local! {
    // Observation only: emitted after the backend actually reports a busy write
    // reservation. The fixture cannot authorize requests or bypass database SQL.
    static BROWSER_LOCK_BUSY: tokio::sync::mpsc::UnboundedSender<()>;
}

#[cfg(test)]
#[path = "web_sessions_tests.rs"]
mod concurrency_tests;

#[cfg(test)]
#[path = "web_login_tests.rs"]
mod login_tests;

#[cfg(test)]
#[path = "web_settings_tests.rs"]
mod settings_tests;
