//! Password reset by mailbox proof.
//!
//! A request mails a single-use token to a verified address; confirming it
//! with a new password replaces the credential and ends every session of the
//! account. A request for an address without a verified account commits
//! nothing, so the form does not say who has one.
use crate::web_sessions::WebSession;
use crate::web_signup::{fresh_token, token_digest};
use crate::{AuditContext, Database, db_error};
use argon2::{
    PasswordHasher,
    password_hash::{SaltString, rand_core::OsRng},
};
use listmngr_core::{Address, Error, Result, UserId};
use listmngr_mail::templates::Placeholders;
use sqlx::Row;
use uuid::Uuid;

const TOKEN_LIFE_MS: i64 = 24 * 60 * 60 * 1000;
const COOLDOWN_MS: i64 = 60 * 60 * 1000;

fn invalid_token() -> Error {
    Error::Validation("invalid or expired token".into())
}

impl Database {
    /// Mail a reset token to `email` when it is a verified address of an
    /// account with a password; otherwise commit nothing. Silent within the
    /// hour after a previous request.
    /// # Errors
    /// An invalid mailbox, a stale session, or a database, template or audit
    /// failure — nothing is written on error.
    pub async fn browser_reset_request(
        &self,
        session: &WebSession,
        email: &str,
        language: &str,
        now_ms: i64,
    ) -> Result<()> {
        let address = Address::new(email, String::new())?;
        crate::workflows::notice_mailbox(&address.email)?;
        let mut tx = self.browser_write_tx().await?;
        crate::web_signup::live_anonymous_session(&mut tx, session, now_ms).await?;
        let row = sqlx::query(
            "SELECT a.id AS address_id, u.id AS user_id, u.locale FROM addresses a
             JOIN users u ON u.id=a.user_id
             JOIN user_credentials c ON c.user_id=u.id
             WHERE a.email=$1 AND a.verified_on IS NOT NULL",
        )
        .bind(&address.email)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?;
        let Some(row) = row else {
            return tx.commit().await.map_err(db_error);
        };
        let address_id: String = row.try_get("address_id").map_err(db_error)?;
        let user: String = row.try_get("user_id").map_err(db_error)?;
        let locale: String = row.try_get("locale").map_err(db_error)?;
        let recent: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM account_tokens WHERE address_id=$1 AND purpose='password_reset' AND created_at>$2")
            .bind(&address_id).bind(now_ms.saturating_sub(COOLDOWN_MS))
            .fetch_one(&mut *tx).await.map_err(db_error)?;
        if recent > 0 {
            return tx.commit().await.map_err(db_error);
        }
        let (token, digest) = fresh_token()?;
        sqlx::query("INSERT INTO account_tokens(id,purpose,address_id,token_hash,created_at,expires_at) VALUES($1,'password_reset',$2,$3,$4,$5)")
            .bind(Uuid::now_v7().to_string()).bind(&address_id).bind(digest).bind(now_ms).bind(now_ms.saturating_add(TOKEN_LIFE_MS))
            .execute(&mut *tx).await.map_err(db_error)?;
        // The account's own language first, then what the browser asked for.
        let language = listmngr_i18n::choose([locale.as_str(), language]);
        let reset_url = format!("{}/web/reset/confirm", self.base_url().unwrap_or_default());
        self.site_notices()
            .enqueue_tx(
                &mut tx,
                &crate::site_notices::SiteNotice {
                    to: &address.original_email,
                    language,
                    subject: "notice-site-reset-subject",
                    subject_args: &[("site_name", self.site_name())],
                    template: "site:user:action:reset",
                },
                Placeholders::new()
                    .set("user_email", address.original_email.clone())
                    .set("token", token)
                    .set("reset_url", reset_url),
                now_ms,
            )
            .await?;
        Self::record_tx_with_context(
            &mut tx,
            &AuditContext::system(),
            "user.reset.request",
            "user",
            &user,
            serde_json::json!({"email": address.email}),
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }

    /// Consume a reset token and set `password`, ending every session of the
    /// account. A refused password leaves the token usable.
    /// # Errors
    /// A weak password; an unknown, used or expired token; a stale session;
    /// or a database or audit failure — nothing is written on error.
    pub async fn browser_reset_confirm(
        &self,
        session: &WebSession,
        token: &str,
        password: &str,
        now_ms: i64,
    ) -> Result<()> {
        let digest = token_digest(token).ok_or_else(invalid_token)?;
        self.users().validate_password(password)?;
        // Argon2 before the writer reservation, as every credential write does.
        let hash = self
            .users()
            .password_hasher()?
            .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
            .map_err(db_error)?
            .to_string();
        let mut tx = self.browser_write_tx().await?;
        crate::web_signup::live_anonymous_session(&mut tx, session, now_ms).await?;
        let address_id: Option<String> = sqlx::query_scalar("UPDATE account_tokens SET consumed_at=$1 WHERE token_hash=$2 AND purpose='password_reset' AND consumed_at IS NULL AND expires_at>$1 RETURNING address_id")
            .bind(now_ms).bind(digest)
            .fetch_optional(&mut *tx).await.map_err(db_error)?;
        let address_id = address_id.ok_or_else(invalid_token)?;
        let user: Option<String> = sqlx::query_scalar(
            "SELECT user_id FROM addresses WHERE id=$1 AND verified_on IS NOT NULL",
        )
        .bind(&address_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?
        .flatten();
        let user: UserId = user.ok_or_else(invalid_token)?.parse().map_err(db_error)?;
        let changed = sqlx::query(
            "UPDATE user_credentials SET password_hash=$1,password_updated_at=$2,failed_attempts=0 WHERE user_id=$3",
        )
        .bind(&hash)
        .bind(crate::now())
        .bind(user.to_string())
        .execute(&mut *tx)
        .await
        .map_err(db_error)?
        .rows_affected();
        if changed != 1 {
            return Err(invalid_token());
        }
        sqlx::query("DELETE FROM web_sessions WHERE user_id=$1")
            .bind(user.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        sqlx::query("UPDATE account_tokens SET consumed_at=$1 WHERE address_id=$2 AND purpose='password_reset' AND consumed_at IS NULL")
            .bind(now_ms).bind(&address_id)
            .execute(&mut *tx).await.map_err(db_error)?;
        Self::record_tx_with_context(
            &mut tx,
            &AuditContext::new(Some(user), None, None),
            "user.password",
            "user",
            &user.to_string(),
            serde_json::json!({"password":"reset","browser_sessions":"revoked","by":"mailed token"}),
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }
}
