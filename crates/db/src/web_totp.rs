//! Enrolling, using and dropping a time-based one-time password.
//!
//! Enrolment starts with a pending secret that only becomes the account's
//! second factor once the reader proves an authenticator produced a code for
//! it; confirming also mints ten single-use recovery codes, stored as SHA-256
//! digests and shown once. A password-only login leaves a session that knows
//! whom it is for but is anonymous to every page; the second step rotates it
//! into a signed-in session, refuses a replayed step and gives up after five
//! wrong codes. Disabling and regenerating recovery codes need the password.
use crate::totp;
use crate::web_sessions::{WebSession, digest};
use crate::{AuditContext, Database, db_error};
use argon2::{PasswordHash, PasswordVerifier};
use listmngr_core::{Error, Result, UserId};
use rand::TryRngCore as _;
use sha2::{Digest as _, Sha256};
use sqlx::Row;
use uuid::Uuid;

/// Recovery codes minted at confirmation and on regeneration.
pub const RECOVERY_CODES: usize = 10;
/// Wrong second-step attempts a pending session survives.
pub const SECOND_FACTOR_ATTEMPTS: i64 = 5;

/// Where the reader stands with their second factor.
#[derive(Debug, Clone)]
pub struct TotpStatus {
    /// A confirmed secret exists: login takes two steps.
    pub enabled: bool,
    /// The site requires this reader to enrol before privileged pages open.
    pub required: bool,
    /// Enrolment material while nothing is confirmed: `(secret, otpauth URI,
    /// inline SVG)`.
    pub pending: Option<(String, String, String)>,
}

fn recovery_digest(code: &str) -> String {
    let normalized: String = code
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .collect();
    format!("{:x}", Sha256::digest(normalized.as_bytes()))
}

/// Ten fresh recovery codes, `XXXXX-XXXXX` in the base32 alphabet, and their
/// digests written for `user`.
async fn mint_recovery_codes(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    user: UserId,
    now_ms: i64,
) -> Result<Vec<String>> {
    sqlx::query("DELETE FROM user_recovery_codes WHERE user_id=$1")
        .bind(user.to_string())
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    let mut codes = Vec::with_capacity(RECOVERY_CODES);
    for _ in 0..RECOVERY_CODES {
        let mut bytes = [0_u8; 10];
        rand::rngs::OsRng
            .try_fill_bytes(&mut bytes)
            .map_err(db_error)?;
        let raw = totp::encode(&bytes);
        let code = format!("{}-{}", &raw[..5], &raw[5..10]);
        sqlx::query(
            "INSERT INTO user_recovery_codes(id,user_id,code_hash,created_at) VALUES($1,$2,$3,$4)",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(user.to_string())
        .bind(recovery_digest(&code))
        .bind(now_ms)
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
        codes.push(code);
    }
    Ok(codes)
}

/// Whether the site's policy makes `user` enrol before privileged pages open.
async fn policy_requires(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    user: UserId,
    required_roles: &[String],
) -> Result<bool> {
    if !required_roles.iter().any(|role| role == "server_owner") {
        return Ok(false);
    }
    let owner: i64 = sqlx::query_scalar("SELECT is_server_owner FROM users WHERE id=$1")
        .bind(user.to_string())
        .fetch_one(&mut **tx)
        .await
        .map_err(db_error)?;
    Ok(owner == 1)
}

impl Database {
    /// The reader's second-factor status, starting enrolment when nothing is
    /// pending or confirmed yet.
    /// # Errors
    /// A stale session or failed CSRF binding; database errors.
    pub async fn browser_totp_status(
        &self,
        session: &WebSession,
        required_roles: &[String],
        now_ms: i64,
    ) -> Result<TotpStatus> {
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_user_tx(&mut tx, session).await?;
        let required = policy_requires(&mut tx, user, required_roles).await?;
        let row = sqlx::query("SELECT secret, confirmed_at FROM user_totp WHERE user_id=$1")
            .bind(user.to_string())
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_error)?;
        let (secret, confirmed) = if let Some(row) = row {
            (
                self.open_totp_secret(
                    user,
                    &row.try_get::<String, _>("secret").map_err(db_error)?,
                )?
                .to_string(),
                row.try_get::<Option<i64>, _>("confirmed_at")
                    .map_err(db_error)?
                    .is_some(),
            )
        } else {
            let mut bytes = [0_u8; totp::SECRET_BYTES];
            rand::rngs::OsRng
                .try_fill_bytes(&mut bytes)
                .map_err(db_error)?;
            let secret = totp::encode(&bytes);
            sqlx::query("INSERT INTO user_totp(user_id,secret,created_at) VALUES($1,$2,$3)")
                .bind(user.to_string())
                .bind(self.seal_totp_secret(user, &secret)?)
                .bind(now_ms)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
            (secret, false)
        };
        tx.commit().await.map_err(db_error)?;
        if confirmed {
            return Ok(TotpStatus {
                enabled: true,
                required,
                pending: None,
            });
        }
        let account: String = sqlx::query_scalar(
            "SELECT a.email FROM users u JOIN addresses a ON a.id=u.preferred_address_id WHERE u.id=$1",
        )
        .bind(user.to_string())
        .fetch_optional(self.pool())
        .await
        .map_err(db_error)?
        .unwrap_or_else(|| user.to_string());
        let bytes = totp::decode(&secret).ok_or_else(|| Error::Database("stored secret".into()))?;
        let uri = totp::provisioning_uri(self.site_name(), &account, &bytes);
        let svg = totp::qr_svg(&uri).unwrap_or_default();
        Ok(TotpStatus {
            enabled: false,
            required,
            pending: Some((secret, uri, svg)),
        })
    }

    /// Whether `session` is a password-only login waiting for its second step.
    /// # Errors
    /// Database errors.
    pub async fn browser_second_factor_pending(&self, session: &WebSession) -> Result<bool> {
        let pending: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM web_sessions WHERE token_hash=$1 AND pending_user_id IS NOT NULL AND expires_at>$2",
        )
        .bind(digest(&session.token))
        .bind(chrono::Utc::now().timestamp_millis())
        .fetch_one(self.pool())
        .await
        .map_err(db_error)?;
        Ok(pending == 1)
    }

    /// Whether the site's policy keeps this reader out of privileged pages
    /// until they enrol.
    /// # Errors
    /// Database errors; an anonymous session is simply not gated.
    pub async fn browser_second_factor_missing(
        &self,
        session: &WebSession,
        required_roles: &[String],
    ) -> Result<bool> {
        let Some(user) = session.user_id else {
            return Ok(false);
        };
        let mut tx = self.write_tx().await?;
        if !policy_requires(&mut tx, user, required_roles).await? {
            return Ok(false);
        }
        // A confirmed one-time password or a passkey (verified on the
        // authenticator) is a second factor.
        let enrolled: i64 = sqlx::query_scalar(
            "SELECT (SELECT COUNT(*) FROM user_totp WHERE user_id=$1 AND confirmed_at IS NOT NULL)
                  + (SELECT COUNT(*) FROM user_passkeys WHERE user_id=$1)",
        )
        .bind(user.to_string())
        .fetch_one(&mut *tx)
        .await
        .map_err(db_error)?;
        Ok(enrolled == 0)
    }

    /// Confirm the pending secret with a code from the authenticator; returns
    /// the recovery codes, shown once.
    /// # Errors
    /// No pending secret or a wrong code (validation); a stale session or
    /// failed CSRF binding; or audit failure.
    pub async fn browser_totp_confirm(
        &self,
        session: &WebSession,
        code: &str,
        now_ms: i64,
    ) -> Result<Vec<String>> {
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_user_tx(&mut tx, session).await?;
        let row = sqlx::query("SELECT secret, confirmed_at FROM user_totp WHERE user_id=$1")
            .bind(user.to_string())
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_error)?
            .ok_or_else(|| Error::Validation("no enrolment in progress".into()))?;
        if row
            .try_get::<Option<i64>, _>("confirmed_at")
            .map_err(db_error)?
            .is_some()
        {
            return Err(Error::Validation(
                "a second factor is already enabled".into(),
            ));
        }
        let secret =
            self.open_totp_secret(user, &row.try_get::<String, _>("secret").map_err(db_error)?)?;
        let bytes = totp::decode(&secret).ok_or_else(|| Error::Database("stored secret".into()))?;
        let step = totp::verify(&bytes, now_ms / 1000, code, 0)
            .ok_or_else(|| Error::Validation("the code did not match".into()))?;
        sqlx::query("UPDATE user_totp SET confirmed_at=$1,last_counter=$2 WHERE user_id=$3")
            .bind(now_ms)
            .bind(step)
            .bind(user.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        let codes = mint_recovery_codes(&mut tx, user, now_ms).await?;
        Self::record_tx_with_context(
            &mut tx,
            &AuditContext::new(Some(user), None, None),
            "user.totp.enable",
            "user",
            &user.to_string(),
            serde_json::json!({"recovery_codes": RECOVERY_CODES}),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(codes)
    }

    /// Complete a password-only login with a one-time code or a recovery
    /// code: the pending session becomes a fresh signed-in one.
    /// # Errors
    /// Authentication for a session that is not pending, a wrong code (five
    /// of which end the session) or a replayed step; audit failure.
    pub async fn browser_second_factor(
        &self,
        session: &WebSession,
        presented: &str,
        now_ms: i64,
    ) -> Result<WebSession> {
        let mut tx = self.browser_write_tx().await?;
        let row = sqlx::query(
            "SELECT s.csrf, s.pending_user_id, s.credential_version, s.second_factor_attempts, c.password_updated_at
             FROM web_sessions s JOIN user_credentials c ON c.user_id=s.pending_user_id
             WHERE s.token_hash=$1 AND s.expires_at>$2 AND s.pending_user_id IS NOT NULL",
        )
        .bind(digest(&session.token))
        .bind(now_ms)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?
        .ok_or(Error::Authentication)?;
        let csrf: String = row.try_get("csrf").map_err(db_error)?;
        let pending: String = row.try_get("pending_user_id").map_err(db_error)?;
        let version: String = row.try_get("credential_version").map_err(db_error)?;
        let current: String = row.try_get("password_updated_at").map_err(db_error)?;
        let attempts: i64 = row.try_get("second_factor_attempts").map_err(db_error)?;
        if !session.verifies_csrf(&csrf) || version != current {
            return Err(Error::Authentication);
        }
        let user: UserId = pending.parse().map_err(db_error)?;
        let Some(factor) = self
            .accept_second_factor(&mut tx, user, presented, now_ms)
            .await?
        else {
            // A wrong code counts; the fifth ends the attempt outright.
            let sql = if attempts + 1 >= SECOND_FACTOR_ATTEMPTS {
                "DELETE FROM web_sessions WHERE token_hash=$1"
            } else {
                "UPDATE web_sessions SET second_factor_attempts=second_factor_attempts+1 WHERE token_hash=$1"
            };
            sqlx::query(sql)
                .bind(digest(&session.token))
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
            tx.commit().await.map_err(db_error)?;
            return Err(Error::Authentication);
        };
        sqlx::query("DELETE FROM web_sessions WHERE token_hash=$1")
            .bind(digest(&session.token))
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        let token = crate::web_sessions::secret();
        let csrf = crate::web_sessions::secret();
        sqlx::query("INSERT INTO web_sessions(token_hash,csrf,user_id,credential_version,expires_at,id,created_at) VALUES($1,$2,$3,$4,$5,$6,$7)")
            .bind(digest(&token)).bind(&csrf).bind(user.to_string()).bind(&version).bind(now_ms + 28_800_000)
            .bind(crate::web_session_inventory::session_id()).bind(now_ms)
            .execute(&mut *tx).await.map_err(db_error)?;
        Self::record_tx_with_context(
            &mut tx,
            &AuditContext::new(Some(user), None, None),
            "web.login",
            "user",
            &user.to_string(),
            serde_json::json!({"second_factor": factor}),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(WebSession {
            token,
            csrf,
            user_id: Some(user),
        })
    }

    /// A one-time code (recording its step) or an unused recovery code
    /// (marking it used); `None` when neither matches.
    async fn accept_second_factor(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Any>,
        user: UserId,
        presented: &str,
        now_ms: i64,
    ) -> Result<Option<&'static str>> {
        let row = sqlx::query("SELECT secret, last_counter FROM user_totp WHERE user_id=$1 AND confirmed_at IS NOT NULL")
            .bind(user.to_string())
            .fetch_optional(&mut **tx)
            .await
            .map_err(db_error)?
            .ok_or(Error::Authentication)?;
        let secret =
            self.open_totp_secret(user, &row.try_get::<String, _>("secret").map_err(db_error)?)?;
        let last: i64 = row.try_get("last_counter").map_err(db_error)?;
        let bytes = totp::decode(&secret).ok_or_else(|| Error::Database("stored secret".into()))?;
        if let Some(step) = totp::verify(&bytes, now_ms / 1000, presented, last) {
            sqlx::query("UPDATE user_totp SET last_counter=$1 WHERE user_id=$2")
                .bind(step)
                .bind(user.to_string())
                .execute(&mut **tx)
                .await
                .map_err(db_error)?;
            return Ok(Some("totp"));
        }
        let consumed = sqlx::query("UPDATE user_recovery_codes SET used_at=$1 WHERE user_id=$2 AND code_hash=$3 AND used_at IS NULL")
            .bind(now_ms).bind(user.to_string()).bind(recovery_digest(presented))
            .execute(&mut **tx).await.map_err(db_error)?.rows_affected();
        Ok((consumed == 1).then_some("recovery"))
    }

    /// Fresh recovery codes, after the password; the old ones stop working.
    /// # Errors
    /// A wrong password or no enabled second factor (authentication); a stale
    /// session or failed CSRF binding; audit failure.
    pub async fn browser_totp_recovery_codes(
        &self,
        session: &WebSession,
        password: &str,
        now_ms: i64,
    ) -> Result<Vec<String>> {
        let user = self.reauthenticate(session, password).await?;
        let mut tx = self.browser_write_tx().await?;
        if Self::browser_user_tx(&mut tx, session).await? != user {
            return Err(Error::Authentication);
        }
        let enabled: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM user_totp WHERE user_id=$1 AND confirmed_at IS NOT NULL",
        )
        .bind(user.to_string())
        .fetch_one(&mut *tx)
        .await
        .map_err(db_error)?;
        if enabled == 0 {
            return Err(Error::Authentication);
        }
        let codes = mint_recovery_codes(&mut tx, user, now_ms).await?;
        Self::record_tx_with_context(
            &mut tx,
            &AuditContext::new(Some(user), None, None),
            "user.totp.recovery",
            "user",
            &user.to_string(),
            serde_json::json!({"recovery_codes": RECOVERY_CODES}),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(codes)
    }

    /// Drop the second factor and its recovery codes, after the password.
    /// # Errors
    /// A wrong password (authentication); a stale session or failed CSRF
    /// binding; audit failure.
    pub async fn browser_totp_disable(&self, session: &WebSession, password: &str) -> Result<()> {
        let user = self.reauthenticate(session, password).await?;
        let mut tx = self.browser_write_tx().await?;
        if Self::browser_user_tx(&mut tx, session).await? != user {
            return Err(Error::Authentication);
        }
        for sql in [
            "DELETE FROM user_recovery_codes WHERE user_id=$1",
            "DELETE FROM user_totp WHERE user_id=$1",
        ] {
            sqlx::query(sql)
                .bind(user.to_string())
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
        }
        Self::record_tx_with_context(
            &mut tx,
            &AuditContext::new(Some(user), None, None),
            "user.totp.disable",
            "user",
            &user.to_string(),
            serde_json::json!({}),
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }

    /// The signed-in user, after their password checks out; Argon2 runs on
    /// the pool, before any writer reservation.
    pub(crate) async fn reauthenticate(
        &self,
        session: &WebSession,
        password: &str,
    ) -> Result<UserId> {
        let user = session.user_id.ok_or(Error::Authentication)?;
        if password.len() > 1024 {
            return Err(Error::Authentication);
        }
        let hash: Option<String> =
            sqlx::query_scalar("SELECT password_hash FROM user_credentials WHERE user_id=$1")
                .bind(user.to_string())
                .fetch_optional(self.pool())
                .await
                .map_err(db_error)?;
        let hash = hash.ok_or(Error::Authentication)?;
        let parsed = PasswordHash::new(&hash).map_err(|_| Error::Authentication)?;
        self.users()
            .password_hasher()?
            .verify_password(password.as_bytes(), &parsed)
            .map_err(|_| Error::Authentication)?;
        Ok(user)
    }
}

impl Database {
    /// The stored form of a TOTP secret: sealed under the master key when
    /// the site has one, the base32 text otherwise.
    fn seal_totp_secret(&self, user: UserId, secret: &str) -> Result<String> {
        self.master_key().map_or_else(
            || Ok(secret.to_owned()),
            |key| {
                key.seal(
                    crate::keyring::TOTP_PURPOSE,
                    user.to_string().as_bytes(),
                    secret.as_bytes(),
                )
            },
        )
    }

    /// The base32 secret from its stored form; a sealed row needs the key
    /// it was sealed under, and a plain row reads as every release before
    /// 1.1 wrote it.
    fn open_totp_secret(&self, user: UserId, stored: &str) -> Result<zeroize::Zeroizing<String>> {
        if !crate::keyring::is_sealed(stored) {
            return Ok(zeroize::Zeroizing::new(stored.to_owned()));
        }
        let key = self
            .master_key()
            .ok_or_else(|| Error::Database("a sealed secret needs security.master_key".into()))?;
        let bytes = key.open(
            crate::keyring::TOTP_PURPOSE,
            user.to_string().as_bytes(),
            stored,
        )?;
        String::from_utf8(bytes.to_vec())
            .map(zeroize::Zeroizing::new)
            .map_err(|_| Error::Database("stored secret".into()))
    }
}
