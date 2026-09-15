//! Accounts behind an `OpenID` Connect provider.
//!
//! The browser's session keeps the ceremony it started; when the provider
//! sends it back, the verified identity resolves to an account in one
//! transaction: a linked `(provider, subject)` signs in; otherwise a verified
//! email that already belongs to a verified address links to that account;
//! otherwise a verified email gets a just-in-time account whose password is
//! random and marked unusable. An unverified email is never trusted. A link
//! can be removed only while another way in remains.
use crate::web_sessions::{LoginOutcome, SECOND_FACTOR_WINDOW_MS, WebSession, digest, secret};
use crate::{AuditContext, Database, db_error};
use argon2::{
    PasswordHasher,
    password_hash::{SaltString, rand_core::OsRng},
};
use listmngr_core::{Address, Error, Result, UserId};
use rand::TryRngCore as _;
use sqlx::Row;
use uuid::Uuid;

/// What the provider asserted, already verified by the caller.
#[derive(Debug, Clone)]
pub struct VerifiedIdentity {
    pub provider: String,
    pub subject: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub name: Option<String>,
}

/// One provider link of the reader.
#[derive(Debug, Clone)]
pub struct OwnLink {
    pub provider: String,
    pub email: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

/// How the reader can get in, which decides what may be unlinked.
#[derive(Debug, Clone)]
pub struct LoginMethods {
    pub usable_password: bool,
    pub passkeys: i64,
    pub links: Vec<OwnLink>,
}

impl LoginMethods {
    /// Whether removing one link would leave no way to sign in.
    #[must_use]
    pub const fn link_is_last_way_in(&self) -> bool {
        !self.usable_password && self.passkeys == 0 && self.links.len() <= 1
    }
}

impl Database {
    /// Park a started ceremony on the session, replacing any earlier one.
    /// # Errors
    /// A stale session; database errors.
    pub async fn browser_oidc_park(
        &self,
        session: &WebSession,
        ceremony: &str,
        now_ms: i64,
    ) -> Result<()> {
        let mut tx = self.browser_write_tx().await?;
        crate::web_signup::live_anonymous_session(&mut tx, session, now_ms).await?;
        sqlx::query("UPDATE web_sessions SET oidc_state=$1 WHERE token_hash=$2")
            .bind(ceremony)
            .bind(digest(&session.token))
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        tx.commit().await.map_err(db_error)
    }

    /// Take the session's parked ceremony, clearing it so a callback answers
    /// it once.
    /// # Errors
    /// No ceremony parked (validation); a stale session; database errors.
    pub async fn browser_oidc_take(&self, session: &WebSession, now_ms: i64) -> Result<String> {
        let mut tx = self.browser_write_tx().await?;
        crate::web_signup::live_anonymous_session(&mut tx, session, now_ms).await?;
        let parked: Option<Option<String>> =
            sqlx::query_scalar("SELECT oidc_state FROM web_sessions WHERE token_hash=$1")
                .bind(digest(&session.token))
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_error)?;
        sqlx::query("UPDATE web_sessions SET oidc_state=NULL WHERE token_hash=$1")
            .bind(digest(&session.token))
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        parked
            .flatten()
            .ok_or_else(|| Error::Validation("no sign-in with this provider is in progress".into()))
    }

    /// Sign in with a verified identity: the linked account, the account
    /// owning the verified address, or a new one. Like the password, this is
    /// half a login for an account with a one-time password enrolled.
    /// # Errors
    /// Forbidden when the email is unverified and nothing is linked, or when
    /// the address belongs to an account that never proved it; a stale
    /// session; audit failure.
    pub async fn browser_oidc_login(
        &self,
        session: &WebSession,
        identity: &VerifiedIdentity,
        language: &str,
        now_ms: i64,
    ) -> Result<LoginOutcome> {
        let mut tx = self.browser_write_tx().await?;
        crate::web_signup::live_anonymous_session(&mut tx, session, now_ms).await?;
        let user = match linked_user(&mut tx, identity).await? {
            Some(user) => user,
            None => resolve_or_create(&mut tx, self, identity, language, now_ms).await?,
        };
        sqlx::query("UPDATE user_oidc SET last_used_at=$1 WHERE provider=$2 AND subject=$3")
            .bind(now_ms)
            .bind(&identity.provider)
            .bind(&identity.subject)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        let version: String =
            sqlx::query_scalar("SELECT password_updated_at FROM user_credentials WHERE user_id=$1")
                .bind(user.to_string())
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?;
        sqlx::query("DELETE FROM web_sessions WHERE token_hash=$1")
            .bind(digest(&session.token))
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        let token = secret();
        let csrf = secret();
        let enrolled: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM user_totp WHERE user_id=$1 AND confirmed_at IS NOT NULL",
        )
        .bind(user.to_string())
        .fetch_one(&mut *tx)
        .await
        .map_err(db_error)?;
        let outcome = if enrolled > 0 {
            sqlx::query("INSERT INTO web_sessions(token_hash,csrf,user_id,credential_version,expires_at,id,created_at,pending_user_id) VALUES($1,$2,NULL,$3,$4,$5,$6,$7)")
                .bind(digest(&token)).bind(&csrf).bind(&version).bind(now_ms + SECOND_FACTOR_WINDOW_MS)
                .bind(crate::web_session_inventory::session_id()).bind(now_ms).bind(user.to_string())
                .execute(&mut *tx).await.map_err(db_error)?;
            LoginOutcome::SecondFactor(WebSession {
                token,
                csrf,
                user_id: None,
            })
        } else {
            sqlx::query("INSERT INTO web_sessions(token_hash,csrf,user_id,credential_version,expires_at,id,created_at) VALUES($1,$2,$3,$4,$5,$6,$7)")
                .bind(digest(&token)).bind(&csrf).bind(user.to_string()).bind(&version).bind(now_ms + 28_800_000)
                .bind(crate::web_session_inventory::session_id()).bind(now_ms)
                .execute(&mut *tx).await.map_err(db_error)?;
            LoginOutcome::Complete(WebSession {
                token,
                csrf,
                user_id: Some(user),
            })
        };
        Self::record_tx_with_context(
            &mut tx,
            &AuditContext::new(Some(user), None, None),
            if enrolled > 0 {
                "web.login.password"
            } else {
                "web.login"
            },
            "user",
            &user.to_string(),
            serde_json::json!({"method": "oidc", "provider": identity.provider}),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(outcome)
    }

    /// Link a verified identity to the signed-in reader.
    /// # Errors
    /// Conflict when the subject is linked elsewhere; a stale session or
    /// failed CSRF binding; audit failure.
    pub async fn browser_oidc_link(
        &self,
        session: &WebSession,
        identity: &VerifiedIdentity,
        now_ms: i64,
    ) -> Result<()> {
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_user_tx(&mut tx, session).await?;
        if let Some(owner) = linked_user(&mut tx, identity).await? {
            if owner == user {
                return tx.commit().await.map_err(db_error);
            }
            return Err(Error::Conflict(
                "this provider account is linked to another account".into(),
            ));
        }
        insert_link(&mut tx, user, identity, now_ms).await?;
        Self::record_tx_with_context(
            &mut tx,
            &AuditContext::new(Some(user), None, None),
            "user.oidc.link",
            "user",
            &user.to_string(),
            serde_json::json!({"provider": identity.provider}),
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }

    /// The reader's links and the other ways they can sign in.
    /// # Errors
    /// A stale or anonymous session; database errors.
    pub async fn browser_login_methods(&self, session: &WebSession) -> Result<LoginMethods> {
        let live = self
            .web_session(&session.token, chrono::Utc::now().timestamp_millis())
            .await?;
        let user = live.user_id.ok_or(Error::Authentication)?;
        let usable: i64 =
            sqlx::query_scalar("SELECT usable FROM user_credentials WHERE user_id=$1")
                .bind(user.to_string())
                .fetch_optional(self.pool())
                .await
                .map_err(db_error)?
                .unwrap_or(0);
        let passkeys: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM user_passkeys WHERE user_id=$1")
                .bind(user.to_string())
                .fetch_one(self.pool())
                .await
                .map_err(db_error)?;
        let rows = sqlx::query(
            "SELECT provider,email,created_at,last_used_at FROM user_oidc WHERE user_id=$1 ORDER BY provider",
        )
        .bind(user.to_string())
        .fetch_all(self.pool())
        .await
        .map_err(db_error)?;
        let links = rows
            .into_iter()
            .map(|row| {
                Ok(OwnLink {
                    provider: row.try_get("provider").map_err(db_error)?,
                    email: row.try_get("email").map_err(db_error)?,
                    created_at: row.try_get("created_at").map_err(db_error)?,
                    last_used_at: row.try_get("last_used_at").map_err(db_error)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(LoginMethods {
            usable_password: usable == 1,
            passkeys,
            links,
        })
    }

    /// Remove the reader's link to `provider`: with the password when the
    /// account has a usable one, and never when the link is the last way in.
    /// # Errors
    /// Validation for the last way in; authentication for a wrong password;
    /// not found for no such link; audit failure.
    pub async fn browser_oidc_unlink(
        &self,
        session: &WebSession,
        provider: &str,
        password: &str,
    ) -> Result<()> {
        let methods = self.browser_login_methods(session).await?;
        if !methods.links.iter().any(|link| link.provider == provider) {
            return Err(Error::NotFound("provider link".into()));
        }
        if methods.link_is_last_way_in() {
            return Err(Error::Validation(
                "this link is the last way into the account".into(),
            ));
        }
        let user = if methods.usable_password {
            self.reauthenticate(session, password).await?
        } else {
            session.user_id.ok_or(Error::Authentication)?
        };
        let mut tx = self.browser_write_tx().await?;
        if Self::browser_user_tx(&mut tx, session).await? != user {
            return Err(Error::Authentication);
        }
        let removed = sqlx::query("DELETE FROM user_oidc WHERE user_id=$1 AND provider=$2")
            .bind(user.to_string())
            .bind(provider)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?
            .rows_affected();
        if removed != 1 {
            return Err(Error::NotFound("provider link".into()));
        }
        Self::record_tx_with_context(
            &mut tx,
            &AuditContext::new(Some(user), None, None),
            "user.oidc.unlink",
            "user",
            &user.to_string(),
            serde_json::json!({"provider": provider}),
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }
}

async fn linked_user(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    identity: &VerifiedIdentity,
) -> Result<Option<UserId>> {
    let user: Option<String> =
        sqlx::query_scalar("SELECT user_id FROM user_oidc WHERE provider=$1 AND subject=$2")
            .bind(&identity.provider)
            .bind(&identity.subject)
            .fetch_optional(&mut **tx)
            .await
            .map_err(db_error)?;
    user.map(|u| u.parse().map_err(db_error)).transpose()
}

async fn insert_link(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    user: UserId,
    identity: &VerifiedIdentity,
    now_ms: i64,
) -> Result<()> {
    sqlx::query("INSERT INTO user_oidc(id,user_id,provider,subject,email,created_at,last_used_at) VALUES($1,$2,$3,$4,$5,$6,$6)")
        .bind(Uuid::now_v7().to_string()).bind(user.to_string()).bind(&identity.provider).bind(&identity.subject)
        .bind(identity.email.as_deref().unwrap_or_default()).bind(now_ms)
        .execute(&mut **tx).await.map_err(db_error)?;
    Ok(())
}

/// An identity nothing is linked to: the account that owns its verified
/// email, or a new one — only for a verified email.
async fn resolve_or_create(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    db: &Database,
    identity: &VerifiedIdentity,
    language: &str,
    now_ms: i64,
) -> Result<UserId> {
    let email = identity
        .email
        .as_deref()
        .filter(|_| identity.email_verified)
        .ok_or_else(|| Error::Forbidden("a verified email address".into()))?;
    let address = Address::new(email, String::new())?;
    let existing = sqlx::query("SELECT id,user_id,verified_on FROM addresses WHERE email=$1")
        .bind(&address.email)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_error)?;
    let user = if let Some(row) = existing {
        let id: String = row.try_get("id").map_err(db_error)?;
        let owner: Option<String> = row.try_get("user_id").map_err(db_error)?;
        let verified: Option<String> = row.try_get("verified_on").map_err(db_error)?;
        match owner {
            Some(owner) if verified.is_some() => owner.parse().map_err(db_error)?,
            Some(_) => {
                return Err(Error::Forbidden(
                    "an address the account has not verified".into(),
                ));
            }
            None => {
                // Nobody's address: the provider has verified it for us.
                let user = create_account(tx, db, identity, &address, language).await?;
                sqlx::query("UPDATE addresses SET user_id=$1,verified_on=$2 WHERE id=$3")
                    .bind(user.to_string())
                    .bind(crate::now())
                    .bind(&id)
                    .execute(&mut **tx)
                    .await
                    .map_err(db_error)?;
                sqlx::query("UPDATE users SET preferred_address_id=$1 WHERE id=$2")
                    .bind(&id)
                    .bind(user.to_string())
                    .execute(&mut **tx)
                    .await
                    .map_err(db_error)?;
                user
            }
        }
    } else {
        let user = create_account(tx, db, identity, &address, language).await?;
        sqlx::query("INSERT INTO addresses(id,email,original_email,display_name,user_id,verified_on,registered_on) VALUES($1,$2,$3,'',$4,$5,$6)")
            .bind(address.id.to_string()).bind(&address.email).bind(&address.original_email).bind(user.to_string())
            .bind(crate::now()).bind(address.registered_on.to_rfc3339())
            .execute(&mut **tx).await.map_err(db_error)?;
        sqlx::query("UPDATE users SET preferred_address_id=$1 WHERE id=$2")
            .bind(address.id.to_string())
            .bind(user.to_string())
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
        user
    };
    insert_link(tx, user, identity, now_ms).await?;
    Ok(user)
}

/// A just-in-time account: user, preferences and a credential row holding a
/// random password nobody knows, marked unusable.
async fn create_account(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    db: &Database,
    identity: &VerifiedIdentity,
    address: &Address,
    language: &str,
) -> Result<UserId> {
    let user = UserId::new();
    let preferences = listmngr_core::PreferencesId::new();
    let display_name = identity
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| {
            !name.is_empty() && name.chars().count() <= 256 && !name.chars().any(char::is_control)
        })
        .unwrap_or(&address.original_email)
        .to_owned();
    let mut random = [0_u8; 32];
    rand::rngs::OsRng
        .try_fill_bytes(&mut random)
        .map_err(db_error)?;
    let hash = db
        .users()
        .password_hasher()?
        .hash_password(&random, &SaltString::generate(&mut OsRng))
        .map_err(db_error)?
        .to_string();
    sqlx::query("INSERT INTO preferences(id) VALUES($1)")
        .bind(preferences.to_string())
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    sqlx::query("INSERT INTO users(id,display_name,is_server_owner,preferences_id,locale,timezone,preferred_address_id,created_at) VALUES($1,$2,0,$3,$4,'UTC',NULL,$5)")
        .bind(user.to_string()).bind(&display_name).bind(preferences.to_string())
        .bind(listmngr_i18n::negotiate(language)).bind(chrono::Utc::now().to_rfc3339())
        .execute(&mut **tx).await.map_err(db_error)?;
    sqlx::query("INSERT INTO user_credentials(user_id,password_hash,password_updated_at,usable) VALUES($1,$2,$3,0)")
        .bind(user.to_string()).bind(hash).bind(crate::now())
        .execute(&mut **tx).await.map_err(db_error)?;
    Database::record_tx_with_context(
        tx,
        &AuditContext::system(),
        "user.jit",
        "user",
        &user.to_string(),
        serde_json::json!({"provider": identity.provider, "email": address.email}),
    )
    .await?;
    Ok(user)
}
