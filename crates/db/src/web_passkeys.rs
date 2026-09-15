//! `WebAuthn` passkeys: registering one under the account, signing in with
//! one alone, and removing one.
//!
//! The relying-party ceremonies come from `webauthn_rp`; this module owns
//! what is stored — the credential's public key and dynamic state in the
//! library's binary encoding, the account's user handle, and the ceremony a
//! browser is in the middle of, kept on its session — and the authority
//! around each step. A passkey login needs no further step: user verification
//! on the authenticator is the second factor, so it also satisfies the
//! site's second-factor policy.
use crate::web_sessions::{WebSession, digest};
use crate::{AuditContext, Database, db_error};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use listmngr_core::{Error, Result, UserId};
use sqlx::Row;
use uuid::Uuid;
use webauthn_rp::bin::{Decode, Encode};
use webauthn_rp::request::register::{
    CredProtect, PublicKeyCredentialUserEntity, RegistrationVerificationOptions, UserHandle64,
};
use webauthn_rp::request::{
    AsciiDomain, ExtensionInfo, PublicKeyCredentialDescriptor, RpId,
    auth::AuthenticationVerificationOptions,
};
use webauthn_rp::response::register::{CompressedPubKey, DynamicState, StaticState};
use webauthn_rp::response::{AuthTransports, CredentialId};
use webauthn_rp::{
    AuthenticatedCredential, DiscoverableAuthentication64, DiscoverableAuthenticationServerState,
    DiscoverableCredentialRequestOptions, PublicKeyCredentialCreationOptions, Registration,
    RegistrationServerState,
};

/// Longest passkey name accepted, in characters.
pub const NAME_MAX: usize = 64;

type StoredPublicKey = CompressedPubKey<[u8; 32], [u8; 32], [u8; 48], Vec<u8>>;

/// One of the reader's passkeys; never key material.
#[derive(Debug, Clone)]
pub struct OwnPasskey {
    /// Row id for the removal form.
    pub id: String,
    /// Name the reader gave it.
    pub name: String,
    /// Unix milliseconds of registration.
    pub created_at: i64,
    /// Unix milliseconds of the last sign-in with it.
    pub last_used_at: Option<i64>,
}

/// The relying party this deployment is, from `site.base_url`.
struct RelyingParty {
    id: RpId,
    origin: String,
}

fn relying_party(base_url: Option<&str>) -> Result<RelyingParty> {
    let base = base_url.ok_or_else(|| Error::Validation("passkeys need site.base_url".into()))?;
    let (scheme, rest) = base
        .split_once("://")
        .ok_or_else(|| Error::Validation("site.base_url must be an absolute URL".into()))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority
        .rsplit_once(':')
        .map_or(authority, |(host, port)| {
            if port.chars().all(|c| c.is_ascii_digit()) {
                host
            } else {
                authority
            }
        });
    if host.is_empty()
        || host.starts_with('[')
        || host.chars().all(|c| c.is_ascii_digit() || c == '.')
    {
        return Err(Error::Validation(
            "passkeys need a domain name in site.base_url, not an address".into(),
        ));
    }
    let id = AsciiDomain::try_from(host.to_owned())
        .map_err(|_| Error::Validation("site.base_url host is not a valid domain".into()))?;
    Ok(RelyingParty {
        id: RpId::Domain(id),
        origin: format!("{scheme}://{authority}"),
    })
}

fn ceremony_error(kind: &str) -> Error {
    Error::Validation(format!("passkey {kind} ceremony failed"))
}

impl Database {
    /// The reader's passkeys, oldest first.
    /// # Errors
    /// A stale or anonymous session; database errors.
    pub async fn browser_passkeys(&self, session: &WebSession) -> Result<Vec<OwnPasskey>> {
        let live = self
            .web_session(&session.token, chrono::Utc::now().timestamp_millis())
            .await?;
        let user = live.user_id.ok_or(Error::Authentication)?;
        let rows = sqlx::query(
            "SELECT id,name,created_at,last_used_at FROM user_passkeys WHERE user_id=$1 ORDER BY created_at,id",
        )
        .bind(user.to_string())
        .fetch_all(self.pool())
        .await
        .map_err(db_error)?;
        rows.into_iter()
            .map(|row| {
                Ok(OwnPasskey {
                    id: row.try_get("id").map_err(db_error)?,
                    name: row.try_get("name").map_err(db_error)?,
                    created_at: row.try_get("created_at").map_err(db_error)?,
                    last_used_at: row.try_get("last_used_at").map_err(db_error)?,
                })
            })
            .collect()
    }

    /// Begin registering a passkey: the creation options for the browser,
    /// with the ceremony state kept on the session.
    /// # Errors
    /// A stale session or failed CSRF binding; a `site.base_url` that names
    /// no domain; database errors.
    pub async fn browser_passkey_register_start(&self, session: &WebSession) -> Result<String> {
        let party = relying_party(self.base_url())?;
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_user_tx(&mut tx, session).await?;
        let row = sqlx::query(
            "SELECT u.display_name, u.webauthn_handle, a.email FROM users u
             LEFT JOIN addresses a ON a.id=u.preferred_address_id WHERE u.id=$1",
        )
        .bind(user.to_string())
        .fetch_one(&mut *tx)
        .await
        .map_err(db_error)?;
        let display_name: String = row.try_get("display_name").map_err(db_error)?;
        let handle: Option<String> = row.try_get("webauthn_handle").map_err(db_error)?;
        let email: Option<String> = row.try_get("email").map_err(db_error)?;
        let handle = if let Some(handle) = handle {
            decode_handle(&handle)?
        } else {
            let fresh = UserHandle64::new();
            sqlx::query("UPDATE users SET webauthn_handle=$1 WHERE id=$2")
                .bind(URL_SAFE_NO_PAD.encode(fresh.as_ref()))
                .bind(user.to_string())
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
            fresh
        };
        let existing: Vec<String> =
            sqlx::query_scalar("SELECT credential_id FROM user_passkeys WHERE user_id=$1")
                .bind(user.to_string())
                .fetch_all(&mut *tx)
                .await
                .map_err(db_error)?;
        let mut exclude = Vec::with_capacity(existing.len());
        for id in existing {
            let bytes = URL_SAFE_NO_PAD
                .decode(id)
                .map_err(|_| Error::Database("stored credential id".into()))?;
            exclude.push(PublicKeyCredentialDescriptor {
                id: CredentialId::<Vec<u8>>::decode(bytes)
                    .map_err(|_| Error::Database("stored credential id".into()))?,
                transports: AuthTransports::decode(0)
                    .map_err(|_| Error::Database("transports".into()))?,
            });
        }
        let name = email.unwrap_or_else(|| user.to_string());
        let entity = PublicKeyCredentialUserEntity {
            name: name
                .as_str()
                .try_into()
                .map_err(|_| Error::Validation("account name".into()))?,
            id: &handle,
            display_name: display_name.as_str().try_into().ok(),
        };
        let mut options = PublicKeyCredentialCreationOptions::passkey(&party.id, entity, exclude);
        // Ask for credential protection but let an authenticator that cannot
        // offer it enrol anyway.
        options.extensions.cred_protect =
            CredProtect::UserVerificationRequired(ExtensionInfo::AllowDontEnforceValue);
        let (server, client) = options
            .start_ceremony()
            .map_err(|_| ceremony_error("registration"))?;
        let state = server
            .encode()
            .map_err(|_| ceremony_error("registration"))?;
        sqlx::query("UPDATE web_sessions SET webauthn_state=$1 WHERE token_hash=$2")
            .bind(format!("reg:{}", STANDARD.encode(state)))
            .bind(digest(&session.token))
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        serde_json::to_string(&client).map_err(|_| ceremony_error("registration"))
    }

    /// Finish registering a passkey from the browser's credential JSON.
    /// # Errors
    /// No ceremony in progress or a response that does not verify for this
    /// site (validation); a stale session or failed CSRF binding; audit
    /// failure. A failed ceremony is discarded either way.
    pub async fn browser_passkey_register_finish(
        &self,
        session: &WebSession,
        name: &str,
        credential_json: &str,
        now_ms: i64,
    ) -> Result<()> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > NAME_MAX || name.chars().any(char::is_control)
        {
            return Err(Error::Validation("passkey name".into()));
        }
        let party = relying_party(self.base_url())?;
        let registration = Registration::from_json_relaxed(credential_json.as_bytes())
            .map_err(|_| ceremony_error("registration"))?;
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_user_tx(&mut tx, session).await?;
        let state = take_state(&mut tx, session, "reg:").await?;
        let server = RegistrationServerState::<64>::decode(&state)
            .map_err(|_| ceremony_error("registration"))?;
        let options = RegistrationVerificationOptions::<&str, &str> {
            allowed_origins: &[party.origin.as_str()],
            ..Default::default()
        };
        let registered = server
            .verify(&party.id, &registration, &options)
            .map_err(|_| ceremony_error("registration"))?;
        let handle: Option<String> =
            sqlx::query_scalar("SELECT webauthn_handle FROM users WHERE id=$1")
                .bind(user.to_string())
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?;
        if handle.as_deref()
            != Some(
                URL_SAFE_NO_PAD
                    .encode(registered.user_id().as_ref())
                    .as_str(),
            )
        {
            return Err(ceremony_error("registration"));
        }
        let credential_id = URL_SAFE_NO_PAD.encode(registered.id().as_ref());
        let static_state = registered
            .static_state()
            .encode()
            .map_err(|_| ceremony_error("registration"))?;
        let dynamic_state = registered
            .dynamic_state()
            .encode()
            .map_err(|_| ceremony_error("registration"))?;
        let inserted = sqlx::query("INSERT INTO user_passkeys(id,user_id,name,credential_id,static_state,dynamic_state,created_at) VALUES($1,$2,$3,$4,$5,$6,$7)")
            .bind(Uuid::now_v7().to_string()).bind(user.to_string()).bind(name).bind(&credential_id)
            .bind(STANDARD.encode(static_state)).bind(STANDARD.encode(dynamic_state)).bind(now_ms)
            .execute(&mut *tx).await;
        if inserted.is_err() {
            return Err(Error::Conflict("this passkey is already registered".into()));
        }
        Self::record_tx_with_context(
            &mut tx,
            &AuditContext::new(Some(user), None, None),
            "user.passkey.add",
            "user",
            &user.to_string(),
            serde_json::json!({"name": name}),
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }

    /// Begin a passkey sign-in on an anonymous session: the request options
    /// for the browser, with the ceremony state kept on the session.
    /// # Errors
    /// A stale session or failed CSRF binding; a `site.base_url` that names
    /// no domain; database errors.
    pub async fn browser_passkey_login_start(
        &self,
        session: &WebSession,
        now_ms: i64,
    ) -> Result<String> {
        let party = relying_party(self.base_url())?;
        let mut tx = self.browser_write_tx().await?;
        crate::web_signup::live_anonymous_session(&mut tx, session, now_ms).await?;
        let (server, client) = DiscoverableCredentialRequestOptions::passkey(&party.id)
            .start_ceremony()
            .map_err(|_| ceremony_error("authentication"))?;
        let state = server
            .encode()
            .map_err(|_| ceremony_error("authentication"))?;
        sqlx::query("UPDATE web_sessions SET webauthn_state=$1 WHERE token_hash=$2")
            .bind(format!("auth:{}", STANDARD.encode(state)))
            .bind(digest(&session.token))
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        serde_json::to_string(&client).map_err(|_| ceremony_error("authentication"))
    }

    /// Finish a passkey sign-in: the assertion completes the ceremony, the
    /// credential's counter advances, and the anonymous session becomes a
    /// signed-in one.
    /// # Errors
    /// Authentication for any assertion that does not verify, names an
    /// unknown credential or account, or answers no live ceremony.
    pub async fn browser_passkey_login_finish(
        &self,
        session: &WebSession,
        assertion_json: &str,
        now_ms: i64,
    ) -> Result<WebSession> {
        let party = relying_party(self.base_url())?;
        let authentication =
            DiscoverableAuthentication64::from_json_relaxed(assertion_json.as_bytes())
                .map_err(|_| Error::Authentication)?;
        let mut tx = self.browser_write_tx().await?;
        crate::web_signup::live_anonymous_session(&mut tx, session, now_ms).await?;
        let state = take_state(&mut tx, session, "auth:")
            .await
            .map_err(|_| Error::Authentication)?;
        let server = DiscoverableAuthenticationServerState::decode(&state)
            .map_err(|_| Error::Authentication)?;
        let credential_id = URL_SAFE_NO_PAD.encode(authentication.raw_id().as_ref());
        let handle = URL_SAFE_NO_PAD.encode(authentication.response().user_handle().as_ref());
        let row = sqlx::query(
            "SELECT p.id, p.user_id, p.static_state, p.dynamic_state, c.password_updated_at
             FROM user_passkeys p JOIN users u ON u.id=p.user_id
             JOIN user_credentials c ON c.user_id=u.id
             WHERE p.credential_id=$1 AND u.webauthn_handle=$2",
        )
        .bind(&credential_id)
        .bind(&handle)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?
        .ok_or(Error::Authentication)?;
        let row_id: String = row.try_get("id").map_err(db_error)?;
        let user: String = row.try_get("user_id").map_err(db_error)?;
        let user: UserId = user.parse().map_err(db_error)?;
        let version: String = row.try_get("password_updated_at").map_err(db_error)?;
        let static_state: String = row.try_get("static_state").map_err(db_error)?;
        let dynamic_state: String = row.try_get("dynamic_state").map_err(db_error)?;
        let static_state = StaticState::<StoredPublicKey>::decode(
            &STANDARD
                .decode(static_state)
                .map_err(|_| Error::Database("stored passkey".into()))?,
        )
        .map_err(|_| Error::Database("stored passkey".into()))?;
        let dynamic_bytes: [u8; 7] = STANDARD
            .decode(dynamic_state)
            .map_err(|_| Error::Database("stored passkey".into()))?
            .try_into()
            .map_err(|_| Error::Database("stored passkey".into()))?;
        let dynamic_state = DynamicState::decode(dynamic_bytes)
            .map_err(|_| Error::Database("stored passkey".into()))?;
        let mut credential = AuthenticatedCredential::new(
            authentication.raw_id(),
            authentication.response().user_handle(),
            static_state,
            dynamic_state,
        )
        .map_err(|_| Error::Authentication)?;
        let options = AuthenticationVerificationOptions::<&str, &str> {
            allowed_origins: &[party.origin.as_str()],
            ..Default::default()
        };
        server
            .verify(&party.id, &authentication, &mut credential, &options)
            .map_err(|_| Error::Authentication)?;
        let updated = credential
            .dynamic_state()
            .encode()
            .map_err(|_| Error::Authentication)?;
        sqlx::query("UPDATE user_passkeys SET dynamic_state=$1,last_used_at=$2 WHERE id=$3")
            .bind(STANDARD.encode(updated))
            .bind(now_ms)
            .bind(&row_id)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
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
            serde_json::json!({"method": "passkey", "passkey_id": row_id}),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(WebSession {
            token,
            csrf,
            user_id: Some(user),
        })
    }

    /// Remove one of the reader's passkeys, after the password.
    /// # Errors
    /// A wrong password (authentication); a passkey that is not the reader's
    /// (not found); a stale session or failed CSRF binding; audit failure.
    pub async fn browser_passkey_remove(
        &self,
        session: &WebSession,
        id: &str,
        password: &str,
    ) -> Result<()> {
        let user = self.reauthenticate(session, password).await?;
        let mut tx = self.browser_write_tx().await?;
        if Self::browser_user_tx(&mut tx, session).await? != user {
            return Err(Error::Authentication);
        }
        let removed = sqlx::query("DELETE FROM user_passkeys WHERE id=$1 AND user_id=$2")
            .bind(id)
            .bind(user.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?
            .rows_affected();
        if removed != 1 {
            return Err(Error::NotFound("passkey".into()));
        }
        Self::record_tx_with_context(
            &mut tx,
            &AuditContext::new(Some(user), None, None),
            "user.passkey.remove",
            "user",
            &user.to_string(),
            serde_json::json!({"passkey_id": id}),
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }
}

fn decode_handle(encoded: &str) -> Result<UserHandle64> {
    let bytes: [u8; 64] = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| Error::Database("stored user handle".into()))?
        .try_into()
        .map_err(|_| Error::Database("stored user handle".into()))?;
    UserHandle64::decode(bytes).map_err(|_| Error::Database("stored user handle".into()))
}

/// The ceremony state a session holds for `purpose`, cleared as it is read
/// so a response can answer a challenge once.
async fn take_state(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    session: &WebSession,
    purpose: &str,
) -> Result<Vec<u8>> {
    let stored: Option<Option<String>> =
        sqlx::query_scalar("SELECT webauthn_state FROM web_sessions WHERE token_hash=$1")
            .bind(digest(&session.token))
            .fetch_optional(&mut **tx)
            .await
            .map_err(db_error)?;
    let stored = stored
        .flatten()
        .ok_or_else(|| Error::Validation("no passkey ceremony in progress".into()))?;
    sqlx::query("UPDATE web_sessions SET webauthn_state=NULL WHERE token_hash=$1")
        .bind(digest(&session.token))
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    let encoded = stored
        .strip_prefix(purpose)
        .ok_or_else(|| Error::Validation("no passkey ceremony in progress".into()))?;
    STANDARD
        .decode(encoded)
        .map_err(|_| Error::Validation("no passkey ceremony in progress".into()))
}
