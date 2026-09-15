//! Self-service account creation and mailbox proof.
//!
//! A signup creates an account nobody can sign in to and mails a single-use
//! token to the address; only the person who reads that mailbox can turn the
//! account on. From outside, every signup looks the same — an address that
//! already has a verified account is neither recreated, changed nor mailed,
//! so the form does not say who has an account. The account row, its
//! credential, the token and the mail commit in one transaction.
use crate::web_sessions::WebSession;
use crate::{AuditContext, Database, db_error};
use argon2::{
    PasswordHasher,
    password_hash::{SaltString, rand_core::OsRng},
};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use listmngr_core::{Address, Error, Result, UserId};
use listmngr_mail::templates::Placeholders;
use rand::TryRngCore as _;
use sha2::{Digest as _, Sha256};
use sqlx::Row;
use uuid::Uuid;

/// How long a mailed token stays usable.
const TOKEN_LIFE_MS: i64 = 24 * 60 * 60 * 1000;
/// At most one verification mail per address per this window.
const COOLDOWN_MS: i64 = 60 * 60 * 1000;

/// What the signup form submits.
#[derive(Debug, Clone)]
pub struct Signup {
    /// Mailbox to prove.
    pub email: String,
    /// Name shown on pages and in notices.
    pub display_name: String,
    /// Password, checked for strength before any write.
    pub password: String,
    /// Interface language the account starts with; a shipped catalog is
    /// negotiated from it.
    pub language: String,
}

/// A fresh mailed secret and the digest the row keeps.
pub(crate) fn fresh_token() -> Result<(String, String)> {
    let mut secret = [0_u8; 32];
    rand::rngs::OsRng
        .try_fill_bytes(&mut secret)
        .map_err(db_error)?;
    Ok((
        URL_SAFE_NO_PAD.encode(secret),
        format!("{:x}", Sha256::digest(secret)),
    ))
}

/// The digest of a presented token, if it has the shape a mailed token has.
pub(crate) fn token_digest(token: &str) -> Option<String> {
    let secret = URL_SAFE_NO_PAD.decode(token.trim()).ok()?;
    (secret.len() == 32).then(|| format!("{:x}", Sha256::digest(secret)))
}

fn invalid_token() -> Error {
    Error::Validation("invalid or expired token".into())
}

impl Database {
    /// Create an unverified account, or re-arm the verification of one that
    /// was never proven, and mail the token. Silent when the address already
    /// belongs to a verified account or was mailed within the hour.
    /// # Errors
    /// An invalid mailbox, name or password; a stale session; or a database,
    /// template or audit failure — nothing is written on error.
    pub async fn browser_signup(
        &self,
        session: &WebSession,
        signup: &Signup,
        now_ms: i64,
    ) -> Result<()> {
        let address = Address::new(&signup.email, signup.display_name.trim().to_owned())?;
        crate::workflows::notice_mailbox(&address.email)?;
        crate::web_profile::validate_display_name(&signup.display_name)?;
        self.users().validate_password(&signup.password)?;
        let locale = listmngr_i18n::negotiate(&signup.language).to_owned();
        // Argon2 before the writer reservation, as every credential write does.
        let hash = self
            .users()
            .password_hasher()?
            .hash_password(
                signup.password.as_bytes(),
                &SaltString::generate(&mut OsRng),
            )
            .map_err(db_error)?
            .to_string();
        let mut tx = self.browser_write_tx().await?;
        live_anonymous_session(&mut tx, session, now_ms).await?;
        let Some((address_id, user, repeat)) =
            claim_account(&mut tx, &address, &locale, &hash).await?
        else {
            return tx.commit().await.map_err(db_error);
        };
        sqlx::query(
            "UPDATE users SET preferred_address_id=$1 WHERE id=$2 AND preferred_address_id IS NULL",
        )
        .bind(&address_id)
        .bind(user.to_string())
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        let recent: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM account_tokens WHERE address_id=$1 AND purpose='verify_address' AND created_at>$2")
            .bind(&address_id).bind(now_ms.saturating_sub(COOLDOWN_MS))
            .fetch_one(&mut *tx).await.map_err(db_error)?;
        if recent > 0 {
            return tx.commit().await.map_err(db_error);
        }
        let (token, digest) = fresh_token()?;
        sqlx::query("INSERT INTO account_tokens(id,purpose,address_id,token_hash,created_at,expires_at) VALUES($1,'verify_address',$2,$3,$4,$5)")
            .bind(Uuid::now_v7().to_string()).bind(&address_id).bind(digest).bind(now_ms).bind(now_ms.saturating_add(TOKEN_LIFE_MS))
            .execute(&mut *tx).await.map_err(db_error)?;
        let verify_url = format!("{}/web/verify", self.base_url().unwrap_or_default());
        self.site_notices()
            .enqueue_tx(
                &mut tx,
                &crate::site_notices::SiteNotice {
                    to: &address.original_email,
                    language: &locale,
                    subject: "notice-site-verify-subject",
                    subject_args: &[("site_name", self.site_name())],
                    template: "site:user:action:verify",
                },
                Placeholders::new()
                    .set("user_email", address.original_email.clone())
                    .set("token", token)
                    .set("verify_url", verify_url),
                now_ms,
            )
            .await?;
        Self::record_tx_with_context(
            &mut tx,
            &AuditContext::system(),
            "user.signup",
            "user",
            &user.to_string(),
            serde_json::json!({"email": address.email, "repeat": repeat}),
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }

    /// Consume a mailed verification token: the address becomes verified and
    /// the account can sign in.
    /// # Errors
    /// An unknown, used or expired token; a stale session; or a database or
    /// audit failure — nothing is written on error.
    pub async fn browser_verify_address(
        &self,
        session: &WebSession,
        token: &str,
        now_ms: i64,
    ) -> Result<()> {
        let digest = token_digest(token).ok_or_else(invalid_token)?;
        let mut tx = self.browser_write_tx().await?;
        live_anonymous_session(&mut tx, session, now_ms).await?;
        let address_id: Option<String> = sqlx::query_scalar("UPDATE account_tokens SET consumed_at=$1 WHERE token_hash=$2 AND purpose='verify_address' AND consumed_at IS NULL AND expires_at>$1 RETURNING address_id")
            .bind(now_ms).bind(digest)
            .fetch_optional(&mut *tx).await.map_err(db_error)?;
        let address_id = address_id.ok_or_else(invalid_token)?;
        let row = sqlx::query("UPDATE addresses SET verified_on=COALESCE(verified_on,$1) WHERE id=$2 RETURNING email,user_id")
            .bind(crate::now()).bind(&address_id)
            .fetch_one(&mut *tx).await.map_err(db_error)?;
        let email: String = row.try_get("email").map_err(db_error)?;
        let user: Option<String> = row.try_get("user_id").map_err(db_error)?;
        let user: Option<UserId> = user.map(|u| u.parse()).transpose().map_err(db_error)?;
        // Sibling tokens for the same address are moot once it is proven.
        sqlx::query("UPDATE account_tokens SET consumed_at=$1 WHERE address_id=$2 AND purpose='verify_address' AND consumed_at IS NULL")
            .bind(now_ms).bind(&address_id)
            .execute(&mut *tx).await.map_err(db_error)?;
        Self::record_tx_with_context(
            &mut tx,
            &AuditContext::new(user, None, None),
            "address.verify",
            "address",
            &email,
            serde_json::json!({"verified": true, "by": "mailed token"}),
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }
}

/// The account a signup claims: created, linked to an address nobody owns, or
/// re-armed when it was never proven. `None` when the address already belongs
/// to a verified account, which the caller must not touch or reveal.
async fn claim_account(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    address: &Address,
    locale: &str,
    hash: &str,
) -> Result<Option<(String, UserId, bool)>> {
    let existing = sqlx::query("SELECT id,user_id,verified_on FROM addresses WHERE email=$1")
        .bind(&address.email)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_error)?;
    let Some(row) = existing else {
        let user = insert_account(tx, address, locale, hash).await?;
        sqlx::query("INSERT INTO addresses(id,email,original_email,display_name,user_id,registered_on) VALUES($1,$2,$3,$4,$5,$6)")
            .bind(address.id.to_string()).bind(&address.email).bind(&address.original_email).bind(&address.display_name).bind(user.to_string()).bind(address.registered_on.to_rfc3339())
            .execute(&mut **tx).await.map_err(db_error)?;
        return Ok(Some((address.id.to_string(), user, false)));
    };
    let id: String = row.try_get("id").map_err(db_error)?;
    let owner: Option<String> = row.try_get("user_id").map_err(db_error)?;
    let verified: Option<String> = row.try_get("verified_on").map_err(db_error)?;
    if verified.is_some() {
        return Ok(None);
    }
    let Some(owner) = owner else {
        let user = insert_account(tx, address, locale, hash).await?;
        sqlx::query("UPDATE addresses SET user_id=$1,display_name=$2 WHERE id=$3")
            .bind(user.to_string())
            .bind(&address.display_name)
            .bind(&id)
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
        return Ok(Some((id, user, false)));
    };
    // An account that was never proven belongs to whoever proves the
    // address; a verified account that is adding this address is not ours to
    // change.
    let proven: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM addresses WHERE user_id=$1 AND verified_on IS NOT NULL",
    )
    .bind(&owner)
    .fetch_one(&mut **tx)
    .await
    .map_err(db_error)?;
    if proven > 0 {
        return Ok(None);
    }
    let user: UserId = owner.parse().map_err(db_error)?;
    sqlx::query("UPDATE users SET display_name=$1,locale=$2 WHERE id=$3")
        .bind(&address.display_name)
        .bind(locale)
        .bind(user.to_string())
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    sqlx::query(
        "UPDATE user_credentials SET password_hash=$1,password_updated_at=$2 WHERE user_id=$3",
    )
    .bind(hash)
    .bind(crate::now())
    .bind(user.to_string())
    .execute(&mut **tx)
    .await
    .map_err(db_error)?;
    Ok(Some((id, user, true)))
}

/// The session that anchors the CSRF token must still be live, checked on
/// the writer's own connection. It may be anonymous or signed in.
async fn live_anonymous_session(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    session: &WebSession,
    now_ms: i64,
) -> Result<()> {
    let csrf: Option<String> =
        sqlx::query_scalar("SELECT csrf FROM web_sessions WHERE token_hash=$1 AND expires_at>$2")
            .bind(crate::web_sessions::digest(&session.token))
            .bind(now_ms)
            .fetch_optional(&mut **tx)
            .await
            .map_err(db_error)?;
    match csrf {
        Some(csrf) if session.verifies_csrf(&csrf) => Ok(()),
        _ => Err(Error::Authentication),
    }
}

/// A user row with its preferences and credential; the address is the
/// caller's to insert or link.
async fn insert_account(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    address: &Address,
    locale: &str,
    hash: &str,
) -> Result<UserId> {
    let user = UserId::new();
    let preferences = listmngr_core::PreferencesId::new();
    sqlx::query("INSERT INTO preferences(id) VALUES($1)")
        .bind(preferences.to_string())
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    sqlx::query("INSERT INTO users(id,display_name,is_server_owner,preferences_id,locale,timezone,preferred_address_id,created_at) VALUES($1,$2,0,$3,$4,'UTC',NULL,$5)")
        .bind(user.to_string()).bind(&address.display_name).bind(preferences.to_string()).bind(locale).bind(chrono::Utc::now().to_rfc3339())
        .execute(&mut **tx).await.map_err(db_error)?;
    sqlx::query(
        "INSERT INTO user_credentials(user_id,password_hash,password_updated_at) VALUES($1,$2,$3)",
    )
    .bind(user.to_string())
    .bind(hash)
    .bind(crate::now())
    .execute(&mut **tx)
    .await
    .map_err(db_error)?;
    Ok(user)
}
