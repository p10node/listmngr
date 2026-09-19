#![forbid(unsafe_code)]

//! Portable PostgreSQL/SQLite repositories for the Phase 1 model.

pub mod archive;
pub mod autoresponse;
pub mod bans;
pub mod bounce_maintenance;
pub mod bounce_processing;
pub mod bounces;
pub mod digests;
pub mod header_matches;
pub use header_matches::{FieldEdit, HeaderMatchPatch, HeaderMatchRow};
pub mod delivery;
pub mod mail_queue;
pub mod moderation;
pub mod notices;
pub mod one_click;
pub mod owner_mail;
pub mod queue_operations;
mod smtp_bounces;
pub mod tasks;
pub mod templates;
pub mod test_support;
pub mod totp;
pub mod web_admin;
mod web_post;
pub use web_post::Poster;
mod web_session_inventory;
pub mod web_sessions;
pub use web_session_inventory::SessionSummary;
pub mod web_profile;
pub use web_profile::Profile;
pub mod site_notices;
pub mod web_addresses;
pub mod web_delete;
pub mod web_domains;
pub mod web_gdpr;
pub mod web_list_settings;
pub mod web_lists;
pub mod web_members;
pub mod web_moderation;
pub mod web_oidc;
pub mod web_passkeys;
pub mod web_system;
pub mod web_tokens;
pub mod web_totp;
pub mod web_users;
pub use web_addresses::OwnAddress;
pub use web_list_settings::{HeaderMatchChange, TemplateView, header_match_outcomes};
pub use web_members::{ExportRow, MassFlags, MassOutcome, MemberDetail, MemberOptions, RosterRow};
pub use web_moderation::{HeldPreview, ModeratedList};
pub use web_oidc::{LoginMethods, OwnLink, VerifiedIdentity};
pub use web_passkeys::OwnPasskey;
pub use web_tokens::{OwnToken, TokenAuthority, TokenRequest};
pub use web_totp::TotpStatus;
pub mod web_reset;
pub mod web_signup;
pub use web_signup::Signup;
pub mod workflows;

use std::{
    collections::{HashMap, HashSet},
    net::IpAddr,
    str::FromStr,
    sync::Once,
};

use argon2::{
    Algorithm, Argon2, Params, PasswordHash, PasswordHasher, PasswordVerifier, Version,
    password_hash::{SaltString, rand_core::OsRng},
};
use base64::Engine;
use chrono::{DateTime, Utc};
use listmngr_core::{
    Address, AddressId, Argon2Config, Domain, DomainId, Error, ListId, MailingList, Member,
    MemberId, MemberRole, Preferences, PreferencesId, Result, SecurityConfig, SubscriptionMode,
    TokenId, User, UserId, builtin_styles, normalize_domain,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Any, AnyPool, Row, Transaction, any::AnyPoolOptions, migrate::Migrator};
use subtle::ConstantTimeEq;
use uuid::Uuid;

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");
static INSTALL_DRIVERS: Once = Once::new();

/// Register the `SQLx` `Any` drivers once; safe to call repeatedly.
pub(crate) fn install_drivers() {
    INSTALL_DRIVERS.call_once(sqlx::any::install_default_drivers);
}

fn db_error(error: impl std::fmt::Display) -> Error {
    let message = error.to_string();
    let lower = message.to_ascii_lowercase();
    if lower.contains("unique constraint")
        || lower.contains("duplicate key")
        || lower.contains("foreign key constraint")
    {
        Error::Conflict("database constraint".into())
    } else {
        Error::Database(message)
    }
}
fn now() -> String {
    Utc::now().to_rfc3339()
}
fn parse_time(value: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|v| v.with_timezone(&Utc))
        .map_err(db_error)
}
fn parse_uuid<T: FromStr>(value: &str) -> Result<T>
where
    T::Err: std::fmt::Display,
{
    value.parse().map_err(db_error)
}

#[derive(Debug, Clone)]
pub struct Database {
    pool: AnyPool,
    argon2: Argon2Config,
    password_min_score: u8,
    /// `site.default_language`: the last resort when neither a recipient nor
    /// a list states a language, and the system layer of preference
    /// resolution.
    default_language: String,
    /// `site.base_url`: the public web origin the mail layer may point at
    /// (`List-Archive`, `Archived-At`). Empty when unknown.
    base_url: String,
    /// `[archive] archivers.mail_archive_address`: where a public list's
    /// copy goes when its `mail-archive` archiver is on. Empty when the
    /// archiver is not configured, which switches it off.
    mail_archive: String,
    /// `[mailman] bounce_probes`: probe at the threshold instead of
    /// disabling at once, and how long the probe's bounce counts. Off by
    /// default here; `serve` applies the configuration.
    bounce_probes: Option<i64>,
    /// `[mta] verp_format` / `verp_delimiter`, for the probe's own sender.
    verp_format: String,
    /// `site.name`, for mail the site sends outside any list.
    site_name: String,
    /// `site.site_owner`: the `From:` of mail the site sends outside any list,
    /// and the domain such mail is signed for.
    site_owner: String,
}

impl Database {
    /// # Errors
    ///
    /// Returns an error if the database cannot be connected to, configured, or migrated.
    pub async fn connect(url: &str, max_connections: u32) -> Result<Self> {
        Self::connect_with_security(url, max_connections, &SecurityConfig::default()).await
    }
    /// # Errors
    ///
    /// Returns an error if the database cannot be connected to, configured, or migrated.
    pub async fn connect_with_security(
        url: &str,
        max_connections: u32,
        security: &SecurityConfig,
    ) -> Result<Self> {
        INSTALL_DRIVERS.call_once(sqlx::any::install_default_drivers);
        let pool = AnyPoolOptions::new()
            .max_connections(max_connections)
            .connect(url)
            .await
            .map_err(db_error)?;
        if url.starts_with("sqlite:") {
            sqlx::query("PRAGMA foreign_keys = ON")
                .execute(&pool)
                .await
                .map_err(db_error)?;
        }
        Ok(Self {
            pool,
            argon2: security.argon2.clone(),
            password_min_score: security.password_min_score,
            default_language: "en".into(),
            base_url: String::new(),
            mail_archive: String::new(),
            bounce_probes: None,
            verp_format: listmngr_core::verp::DEFAULT_FORMAT.into(),
            site_name: "Example Lists".into(),
            site_owner: "postmaster@example.com".into(),
        })
    }
    /// Carry `[mailman] bounce_probes` / `bounce_probe_lifetime_secs` and the
    /// `[mta] verp_format` the probe sender must follow.
    #[must_use]
    pub fn with_bounce_probes(
        mut self,
        enabled: bool,
        lifetime_secs: u32,
        verp_format: &str,
    ) -> Self {
        self.bounce_probes = enabled.then_some(i64::from(lifetime_secs).saturating_mul(1000));
        verp_format.clone_into(&mut self.verp_format);
        self
    }
    /// The probe lifetime in milliseconds when probes are on.
    #[must_use]
    pub const fn bounce_probe_lifetime_ms(&self) -> Option<i64> {
        self.bounce_probes
    }
    /// The VERP format probes are addressed with.
    #[must_use]
    pub fn verp_format(&self) -> &str {
        &self.verp_format
    }
    /// Carry `site.default_language` so notices and preference resolution
    /// fall back to the operator's choice rather than English.
    #[must_use]
    pub fn with_default_language(mut self, language: &str) -> Self {
        language.trim().clone_into(&mut self.default_language);
        self
    }
    /// The site default language.
    #[must_use]
    pub fn default_language(&self) -> &str {
        &self.default_language
    }
    /// Carry `site.base_url` so cooked posts can advertise the archive.
    #[must_use]
    pub fn with_base_url(mut self, base_url: &str) -> Self {
        base_url.trim().clone_into(&mut self.base_url);
        self
    }
    /// The site's public base URL, when configured.
    #[must_use]
    pub fn base_url(&self) -> Option<&str> {
        (!self.base_url.is_empty()).then_some(self.base_url.as_str())
    }

    /// Where a public list's `mail-archive` copy is sent. An empty
    /// address leaves the archiver off however the list is configured.
    #[must_use]
    pub fn with_mail_archive_address(mut self, address: &str) -> Self {
        address.trim().clone_into(&mut self.mail_archive);
        self
    }

    /// The configured `mail-archive` address, when there is one.
    #[must_use]
    pub fn mail_archive_address(&self) -> Option<&str> {
        (!self.mail_archive.is_empty()).then_some(self.mail_archive.as_str())
    }
    /// Carry `site.name` and `site.site_owner` for mail the site sends
    /// outside any list. The defaults mirror the configuration defaults.
    #[must_use]
    pub fn with_site(mut self, name: &str, owner: &str) -> Self {
        name.trim().clone_into(&mut self.site_name);
        owner.trim().clone_into(&mut self.site_owner);
        self
    }
    /// The site name notices show.
    #[must_use]
    pub fn site_name(&self) -> &str {
        &self.site_name
    }
    /// The site owner address notices are sent from.
    #[must_use]
    pub fn site_owner(&self) -> &str {
        &self.site_owner
    }
    /// Producer of mail the site sends to a person outside any list.
    #[must_use]
    pub const fn site_notices(&self) -> site_notices::SiteNoticeRepo<'_> {
        site_notices::SiteNoticeRepo { db: self }
    }
    /// # Errors
    ///
    /// Returns an error if the database cannot be connected to, configured, or migrated.
    pub async fn migrate(&self) -> Result<()> {
        MIGRATOR.run(&self.pool).await.map_err(db_error)
    }
    #[must_use]
    pub const fn pool(&self) -> &AnyPool {
        &self.pool
    }
    #[must_use]
    pub const fn domains(&self) -> DomainRepo<'_> {
        DomainRepo { db: self }
    }
    #[must_use]
    pub const fn users(&self) -> UserRepo<'_> {
        UserRepo { db: self }
    }
    #[must_use]
    pub const fn addresses(&self) -> AddressRepo<'_> {
        AddressRepo { db: self }
    }
    #[must_use]
    pub const fn lists(&self) -> ListRepo<'_> {
        ListRepo { db: self }
    }
    #[must_use]
    pub const fn members(&self) -> MemberRepo<'_> {
        MemberRepo { db: self }
    }
    #[must_use]
    pub const fn preferences(&self) -> PreferencesRepo<'_> {
        PreferencesRepo { db: self }
    }
    #[must_use]
    pub const fn tokens(&self) -> TokenRepo<'_> {
        TokenRepo { db: self }
    }
    #[must_use]
    pub const fn audit(&self) -> AuditRepo<'_> {
        AuditRepo { db: self }
    }
    #[must_use]
    pub const fn header_matches(&self) -> header_matches::HeaderMatchRepo<'_> {
        header_matches::HeaderMatchRepo { db: self }
    }
    #[must_use]
    pub const fn templates(&self) -> templates::TemplateRepo<'_> {
        templates::TemplateRepo { db: self }
    }

    async fn record_tx_with_context(
        tx: &mut Transaction<'_, Any>,
        context: &AuditContext,
        action: &str,
        target_type: &str,
        target_id: &str,
        diff: serde_json::Value,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO audit_log(id,at,actor_user_id,actor_token_id,ip,action,target_type,target_id,diff) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(now())
        .bind(context.user_id.map(|value| value.to_string()))
        .bind(context.token_id.map(|value| value.to_string()))
        .bind(context.peer_ip.map(|value| value.to_string()))
        .bind(action)
        .bind(target_type)
        .bind(target_id)
        .bind(redact_audit_value(diff).to_string())
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
        Ok(())
    }
}

fn redact_audit_value(mut value: serde_json::Value) -> serde_json::Value {
    match &mut value {
        serde_json::Value::Object(object) => {
            for (key, child) in object {
                let key = key.to_ascii_lowercase();
                if key.contains("password") || key.contains("secret") || key.contains("hash") {
                    *child = serde_json::Value::String("[REDACTED]".into());
                } else {
                    *child = redact_audit_value(child.take());
                }
            }
        }
        serde_json::Value::Array(array) => {
            for child in array {
                *child = redact_audit_value(child.take());
            }
        }
        _ => {}
    }
    value
}

/// Immutable attribution captured at the edge before a business write starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditContext {
    user_id: Option<UserId>,
    token_id: Option<TokenId>,
    peer_ip: Option<IpAddr>,
}

impl AuditContext {
    #[must_use]
    pub const fn new(
        user_id: Option<UserId>,
        token_id: Option<TokenId>,
        peer_ip: Option<IpAddr>,
    ) -> Self {
        Self {
            user_id,
            token_id,
            peer_ip,
        }
    }

    #[must_use]
    pub const fn system() -> Self {
        Self::new(None, None, None)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: String,
    pub at: DateTime<Utc>,
    pub actor_user_id: Option<UserId>,
    pub actor_token_id: Option<TokenId>,
    pub ip: Option<IpAddr>,
    pub action: String,
    pub target_type: String,
    pub target_id: String,
    pub diff: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct NewUser {
    pub display_name: String,
    pub email: String,
    pub password: String,
    pub server_owner: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct NewList {
    pub list_id: ListId,
    pub display_name: String,
    pub style: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct NewMember {
    pub list_id: ListId,
    pub email: String,
    pub role: MemberRole,
    pub subscription_mode: SubscriptionMode,
    pub display_name: String,
}

/// Deterministic summary of one atomic role-scoped member mass operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct MemberMassResult {
    pub added: usize,
    pub removed: usize,
    pub retained: usize,
}

impl MemberMassResult {
    #[must_use]
    pub const fn processed(self) -> usize {
        self.added + self.removed + self.retained
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssuedToken {
    pub id: TokenId,
    pub token: String,
}

/// Parameters for issuing a token, including optional resource boundaries.
#[derive(Debug, Clone, Copy)]
pub struct NewToken<'a> {
    pub user: UserId,
    pub name: &'a str,
    pub scopes: &'a [&'a str],
    pub list_id: Option<&'a ListId>,
    pub domain_id: Option<DomainId>,
    pub expires: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenAuth {
    pub id: TokenId,
    pub user_id: UserId,
    pub scopes: HashSet<String>,
    pub list_id: Option<ListId>,
    pub domain_id: Option<DomainId>,
}
impl TokenAuth {
    #[must_use]
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.contains("admin") || self.scopes.contains(scope)
    }

    #[must_use]
    pub fn allows_domain(&self, domain: DomainId) -> bool {
        self.domain_id.is_none_or(|bound| bound == domain)
    }

    #[must_use]
    pub fn allows_list(&self, list: &ListId, domain: DomainId) -> bool {
        self.list_id.as_ref().is_none_or(|bound| bound == list)
            && self.domain_id.is_none_or(|bound| bound == domain)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Template {
    pub id: String,
    pub name: String,
    pub scope: String,
    pub scope_id: Option<String>,
    pub language: String,
    pub uri: Option<String>,
    pub body: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct DomainRepo<'a> {
    db: &'a Database,
}
impl DomainRepo<'_> {
    /// # Errors
    ///
    /// Returns an error for invalid domain data, missing/conflicting records, or database/audit transaction failure.
    pub async fn add_owner(&self, host: &str, user: UserId) -> Result<()> {
        self.add_owner_with_context(host, user, &AuditContext::system())
            .await
    }
    /// Adds a domain owner attributed to the supplied audit context.
    ///
    /// # Errors
    ///
    /// Returns an error if the domain or user is missing, the owner conflicts, or the database/audit transaction fails.
    pub async fn add_owner_with_context(
        &self,
        host: &str,
        user: UserId,
        context: &AuditContext,
    ) -> Result<()> {
        let domain = self.get(host).await?;
        self.db.users().get(user).await?;
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        sqlx::query("INSERT INTO domain_owners(domain_id,user_id) VALUES($1,$2)")
            .bind(domain.id.to_string())
            .bind(user.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        Database::record_tx_with_context(
            &mut tx,
            context,
            "domain.owner.add",
            "domain",
            host,
            serde_json::json!({"user_id":user}),
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }

    /// Removes a domain owner attributed to the supplied audit context.
    ///
    /// # Errors
    ///
    /// Returns an error if the domain is missing, the user is not an owner, or the database/audit transaction fails.
    pub async fn remove_owner_with_context(
        &self,
        host: &str,
        user: UserId,
        context: &AuditContext,
    ) -> Result<()> {
        let domain = self.get(host).await?;
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        Self::remove_owner_tx(&mut tx, &domain, user, context).await?;
        tx.commit().await.map_err(db_error)
    }

    pub(crate) async fn remove_owner_tx(
        tx: &mut Transaction<'_, Any>,
        domain: &Domain,
        user: UserId,
        context: &AuditContext,
    ) -> Result<()> {
        let changed = sqlx::query("DELETE FROM domain_owners WHERE domain_id=$1 AND user_id=$2")
            .bind(domain.id.to_string())
            .bind(user.to_string())
            .execute(&mut **tx)
            .await
            .map_err(db_error)?
            .rows_affected();
        if changed == 0 {
            return Err(Error::NotFound("domain owner".into()));
        }
        Database::record_tx_with_context(
            tx,
            context,
            "domain.owner.remove",
            "domain",
            &domain.mail_host,
            serde_json::json!({"user_id":user}),
        )
        .await
    }

    /// # Errors
    ///
    /// Returns an error for invalid domain data, missing/conflicting records, or database/audit transaction failure.
    pub async fn owners(&self, host: &str) -> Result<Vec<User>> {
        let domain = self.get(host).await?;
        let rows = sqlx::query("SELECT u.id,u.display_name,u.is_server_owner,u.locale,u.timezone,u.preferred_address_id,u.created_at FROM users u JOIN domain_owners o ON o.user_id=u.id WHERE o.domain_id=$1 ORDER BY u.created_at")
            .bind(domain.id.to_string()).fetch_all(&self.db.pool).await.map_err(db_error)?;
        rows.iter().map(user_from_row).collect()
    }

    /// # Errors
    ///
    /// Returns an error for invalid domain data, missing/conflicting records, or database/audit transaction failure.
    pub async fn create(
        &self,
        host: &str,
        description: &str,
        alias: Option<&str>,
    ) -> Result<Domain> {
        self.create_with_context(host, description, alias, &AuditContext::system())
            .await
    }

    /// Creates a domain attributed to the supplied immutable audit context.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid or conflicting domain data, or if the database/audit transaction fails.
    pub async fn create_with_context(
        &self,
        host: &str,
        description: &str,
        alias: Option<&str>,
        context: &AuditContext,
    ) -> Result<Domain> {
        let host = normalize_domain(host)?;
        let domain = Domain {
            id: DomainId::new(),
            mail_host: host,
            description: description.into(),
            alias_domain: alias.map(str::to_owned),
            created_at: Utc::now(),
        };
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        sqlx::query("INSERT INTO domains(id,mail_host,description,alias_domain,created_at) VALUES($1,$2,$3,$4,$5)")
            .bind(domain.id.to_string()).bind(&domain.mail_host).bind(&domain.description).bind(&domain.alias_domain)
            .bind(domain.created_at.to_rfc3339()).execute(&mut *tx).await.map_err(db_error)?;
        Database::record_tx_with_context(
            &mut tx,
            context,
            "domain.create",
            "domain",
            &domain.mail_host,
            serde_json::json!({"mail_host": domain.mail_host}),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(domain)
    }
    /// # Errors
    ///
    /// Returns an error for invalid domain data, missing/conflicting records, or database/audit transaction failure.
    pub async fn get(&self, host: &str) -> Result<Domain> {
        let row = sqlx::query("SELECT id,mail_host,description,alias_domain,created_at FROM domains WHERE mail_host=$1")
            .bind(host.to_ascii_lowercase()).fetch_optional(&self.db.pool).await.map_err(db_error)?
            .ok_or_else(|| Error::NotFound(host.into()))?;
        domain_from_row(&row)
    }
    /// # Errors
    ///
    /// Returns an error for invalid domain data, missing/conflicting records, or database/audit transaction failure.
    pub async fn list(&self) -> Result<Vec<Domain>> {
        let rows = sqlx::query("SELECT id,mail_host,description,alias_domain,created_at FROM domains ORDER BY mail_host").fetch_all(&self.db.pool).await.map_err(db_error)?;
        rows.iter().map(domain_from_row).collect()
    }
    /// # Errors
    ///
    /// Returns an error for invalid domain data, missing/conflicting records, or database/audit transaction failure.
    pub async fn delete(&self, host: &str) -> Result<()> {
        self.delete_with_context(host, &AuditContext::system())
            .await
    }
    /// Deletes a domain attributed to the supplied audit context.
    ///
    /// # Errors
    ///
    /// Returns an error if the domain is missing or nonempty, or if the database/audit transaction fails.
    pub async fn delete_with_context(&self, host: &str, context: &AuditContext) -> Result<()> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM mailing_lists WHERE mail_host=$1")
                .bind(host.to_ascii_lowercase())
                .fetch_one(&self.db.pool)
                .await
                .map_err(db_error)?;
        if count != 0 {
            return Err(Error::Conflict("domain owns lists".into()));
        }
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        let changed = sqlx::query("DELETE FROM domains WHERE mail_host=$1")
            .bind(host.to_ascii_lowercase())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?
            .rows_affected();
        if changed == 0 {
            return Err(Error::NotFound(host.into()));
        }
        Database::record_tx_with_context(
            &mut tx,
            context,
            "domain.delete",
            "domain",
            host,
            serde_json::json!({}),
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }
}
fn domain_from_row(row: &sqlx::any::AnyRow) -> Result<Domain> {
    Ok(Domain {
        id: parse_uuid(row.try_get::<String, _>("id").map_err(db_error)?.as_str())?,
        mail_host: row.try_get("mail_host").map_err(db_error)?,
        description: row.try_get("description").map_err(db_error)?,
        alias_domain: row.try_get("alias_domain").map_err(db_error)?,
        created_at: parse_time(&row.try_get::<String, _>("created_at").map_err(db_error)?)?,
    })
}

#[derive(Debug, Clone, Copy)]
pub struct UserRepo<'a> {
    db: &'a Database,
}
impl UserRepo<'_> {
    pub(crate) fn password_hasher(self) -> Result<Argon2<'static>> {
        let params = Params::new(
            self.db.argon2.memory_kib,
            self.db.argon2.iterations,
            self.db.argon2.parallelism,
            None,
        )
        .map_err(db_error)?;
        Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
    }

    pub(crate) fn validate_password(self, password: &str) -> Result<()> {
        if password.len() > 1024 {
            return Err(Error::Validation("password exceeds 1024 bytes".into()));
        }
        if u8::from(zxcvbn::zxcvbn(password, &[]).score()) < self.db.password_min_score {
            return Err(Error::Validation("password is too weak".into()));
        }
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error for invalid credentials or fields, missing/conflicting records, or database/audit transaction failure.
    pub async fn create(&self, new: NewUser) -> Result<User> {
        self.create_with_context(new, &AuditContext::system()).await
    }
    /// Creates a user attributed to the supplied audit context.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid credentials or fields, conflicting records, or database/audit transaction failure.
    pub async fn create_with_context(&self, new: NewUser, context: &AuditContext) -> Result<User> {
        self.validate_password(&new.password)?;
        let address = Address::new(&new.email, new.display_name.clone())?;
        let pref = PreferencesId::new();
        let user = User {
            id: UserId::new(),
            display_name: new.display_name,
            is_server_owner: new.server_owner,
            locale: "en".into(),
            timezone: "UTC".into(),
            preferred_address_id: Some(address.id),
            created_at: Utc::now(),
        };
        let salt = SaltString::generate(&mut OsRng);
        let hash = self
            .password_hasher()?
            .hash_password(new.password.as_bytes(), &salt)
            .map_err(db_error)?
            .to_string();
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        sqlx::query("INSERT INTO preferences(id) VALUES($1)")
            .bind(pref.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        sqlx::query("INSERT INTO users(id,display_name,is_server_owner,preferences_id,locale,timezone,preferred_address_id,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8)")
            .bind(user.id.to_string()).bind(&user.display_name).bind(i64::from(user.is_server_owner)).bind(pref.to_string()).bind(&user.locale).bind(&user.timezone).bind(Option::<String>::None).bind(user.created_at.to_rfc3339()).execute(&mut *tx).await.map_err(db_error)?;
        sqlx::query(
            "INSERT INTO user_credentials(user_id,password_hash,password_updated_at) VALUES($1,$2,$3)",
        )
        .bind(user.id.to_string())
        .bind(hash)
        .bind(now())
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        sqlx::query("INSERT INTO addresses(id,email,original_email,display_name,user_id,registered_on) VALUES($1,$2,$3,$4,$5,$6)")
            .bind(address.id.to_string()).bind(&address.email).bind(&address.original_email).bind(&address.display_name).bind(user.id.to_string()).bind(address.registered_on.to_rfc3339()).execute(&mut *tx).await.map_err(db_error)?;
        sqlx::query("UPDATE users SET preferred_address_id=$1 WHERE id=$2")
            .bind(address.id.to_string())
            .bind(user.id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        Database::record_tx_with_context(
            &mut tx,
            context,
            "user.create",
            "user",
            &user.id.to_string(),
            serde_json::json!({"email": address.email}),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(user)
    }
    /// # Errors
    ///
    /// Returns an error for invalid credentials or fields, missing/conflicting records, or database/audit transaction failure.
    pub async fn get(&self, id: UserId) -> Result<User> {
        self.get_by("id", &id.to_string()).await
    }
    /// # Errors
    ///
    /// Returns an error for invalid credentials or fields, missing/conflicting records, or database/audit transaction failure.
    pub async fn get_by_email(&self, email: &str) -> Result<User> {
        let row = sqlx::query("SELECT user_id FROM addresses WHERE email=$1")
            .bind(email.to_ascii_lowercase())
            .fetch_optional(&self.db.pool)
            .await
            .map_err(db_error)?
            .ok_or_else(|| Error::NotFound(email.into()))?;
        self.get(parse_uuid(
            row.try_get::<String, _>("user_id")
                .map_err(db_error)?
                .as_str(),
        )?)
        .await
    }
    async fn get_by(&self, column: &str, value: &str) -> Result<User> {
        let query = format!(
            "SELECT id,display_name,is_server_owner,locale,timezone,preferred_address_id,created_at FROM users WHERE {column}=$1"
        );
        let row = sqlx::query(&query)
            .bind(value)
            .fetch_optional(&self.db.pool)
            .await
            .map_err(db_error)?
            .ok_or_else(|| Error::NotFound(value.into()))?;
        user_from_row(&row)
    }
    /// # Errors
    ///
    /// Returns an error for invalid credentials or fields, missing/conflicting records, or database/audit transaction failure.
    pub async fn list(&self) -> Result<Vec<User>> {
        let rows = sqlx::query("SELECT id,display_name,is_server_owner,locale,timezone,preferred_address_id,created_at FROM users ORDER BY created_at").fetch_all(&self.db.pool).await.map_err(db_error)?;
        rows.iter().map(user_from_row).collect()
    }
    /// # Errors
    ///
    /// Returns an error for invalid credentials or fields, missing/conflicting records, or database/audit transaction failure.
    pub async fn update(&self, id: UserId, patch: &serde_json::Value) -> Result<User> {
        self.update_with_context(id, patch, &AuditContext::system())
            .await
    }
    /// Updates a user attributed to the supplied audit context.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid fields, a missing user, or database/audit transaction failure.
    pub async fn update_with_context(
        &self,
        id: UserId,
        patch: &serde_json::Value,
        context: &AuditContext,
    ) -> Result<User> {
        let current = self.get(id).await?;
        let object = patch
            .as_object()
            .ok_or_else(|| Error::Validation("user patch must be an object".into()))?;
        let string = |key: &str, old: &str| -> Result<String> {
            object.get(key).map_or_else(
                || Ok(old.to_owned()),
                |v| {
                    v.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| Error::Validation(key.into()))
                },
            )
        };
        if object
            .keys()
            .any(|key| !matches!(key.as_str(), "display_name" | "locale" | "timezone"))
        {
            return Err(Error::Validation("read-only or unknown user field".into()));
        }
        let display_name = string("display_name", &current.display_name)?;
        let locale = string("locale", &current.locale)?;
        let timezone = string("timezone", &current.timezone)?;
        crate::web_profile::Profile {
            display_name: display_name.clone(),
            locale: locale.clone(),
            timezone: timezone.clone(),
        }
        .validate()?;
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        sqlx::query("UPDATE users SET display_name=$1,locale=$2,timezone=$3 WHERE id=$4")
            .bind(display_name)
            .bind(locale)
            .bind(timezone)
            .bind(id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        Database::record_tx_with_context(
            &mut tx,
            context,
            "user.update",
            "user",
            &id.to_string(),
            patch.clone(),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        self.get(id).await
    }
    /// # Errors
    ///
    /// Returns an error for invalid credentials or fields, missing/conflicting records, or database/audit transaction failure.
    pub async fn set_password(&self, id: UserId, password: &str) -> Result<()> {
        self.set_password_with_context(id, password, &AuditContext::system())
            .await
    }
    /// Changes a password attributed to the supplied audit context.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid password, a missing user, or database/audit transaction failure.
    pub async fn set_password_with_context(
        &self,
        id: UserId,
        password: &str,
        context: &AuditContext,
    ) -> Result<()> {
        self.validate_password(password)?;
        let hash = self
            .password_hasher()?
            .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
            .map_err(db_error)?
            .to_string();
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        let changed = sqlx::query(
            "UPDATE user_credentials SET password_hash=$1,password_updated_at=$2 WHERE user_id=$3",
        )
        .bind(hash)
        .bind(now())
        .bind(id.to_string())
        .execute(&mut *tx)
        .await
        .map_err(db_error)?
        .rows_affected();
        if changed == 0 {
            return Err(Error::NotFound(id.to_string()));
        }
        Database::record_tx_with_context(
            &mut tx,
            context,
            "user.password",
            "user",
            &id.to_string(),
            serde_json::json!({"password":"changed"}),
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }
    /// # Errors
    ///
    /// Returns an error for invalid credentials or fields, missing/conflicting records, or database/audit transaction failure.
    pub async fn verify_password(&self, id: UserId, password: &str) -> Result<bool> {
        let row = sqlx::query("SELECT password_hash FROM user_credentials WHERE user_id=$1")
            .bind(id.to_string())
            .fetch_optional(&self.db.pool)
            .await
            .map_err(db_error)?
            .ok_or(Error::Authentication)?;
        let hash: String = row.try_get("password_hash").map_err(db_error)?;
        let verifier = self.password_hasher()?;
        Ok(PasswordHash::new(&hash).ok().is_some_and(|parsed| {
            verifier
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        }))
    }
    /// # Errors
    ///
    /// Returns an error for invalid credentials or fields, missing/conflicting records, or database/audit transaction failure.
    pub async fn delete(&self, id: UserId) -> Result<()> {
        self.delete_with_context(id, &AuditContext::system()).await
    }
    /// Deletes a user attributed to the supplied audit context.
    ///
    /// # Errors
    ///
    /// Returns an error if the user is missing or referenced, or if the database/audit transaction fails.
    pub async fn delete_with_context(&self, id: UserId, context: &AuditContext) -> Result<()> {
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        let preferences_id: Option<String> =
            sqlx::query_scalar("SELECT preferences_id FROM users WHERE id=$1")
                .bind(id.to_string())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_error)?;
        let Some(preferences_id) = preferences_id else {
            return Err(Error::NotFound(id.to_string()));
        };
        sqlx::query("DELETE FROM user_credentials WHERE user_id=$1")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        sqlx::query("DELETE FROM api_tokens WHERE user_id=$1")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        sqlx::query("UPDATE addresses SET user_id=NULL WHERE user_id=$1")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        let changed = sqlx::query("DELETE FROM users WHERE id=$1")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?
            .rows_affected();
        debug_assert_eq!(changed, 1);
        sqlx::query("DELETE FROM preferences WHERE id=$1")
            .bind(preferences_id)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        Database::record_tx_with_context(
            &mut tx,
            context,
            "user.delete",
            "user",
            &id.to_string(),
            serde_json::json!({}),
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }
}
fn user_from_row(row: &sqlx::any::AnyRow) -> Result<User> {
    Ok(User {
        id: parse_uuid(row.try_get::<String, _>("id").map_err(db_error)?.as_str())?,
        display_name: row.try_get("display_name").map_err(db_error)?,
        is_server_owner: row.try_get::<i64, _>("is_server_owner").map_err(db_error)? != 0,
        locale: row.try_get("locale").map_err(db_error)?,
        timezone: row.try_get("timezone").map_err(db_error)?,
        preferred_address_id: row
            .try_get::<Option<String>, _>("preferred_address_id")
            .map_err(db_error)?
            .map(|v| parse_uuid(&v))
            .transpose()?,
        created_at: parse_time(&row.try_get::<String, _>("created_at").map_err(db_error)?)?,
    })
}

#[derive(Debug, Clone, Copy)]
pub struct AddressRepo<'a> {
    db: &'a Database,
}
impl AddressRepo<'_> {
    /// # Errors
    ///
    /// Returns an error for invalid address data, missing/conflicting records, or database/audit transaction failure.
    pub async fn get_by_id(&self, id: AddressId) -> Result<Address> {
        let row = sqlx::query("SELECT id,email,original_email,display_name,user_id,verified_on,registered_on FROM addresses WHERE id=$1")
            .bind(id.to_string())
            .fetch_optional(&self.db.pool)
            .await
            .map_err(db_error)?
            .ok_or_else(|| Error::NotFound(id.to_string()))?;
        address_from_row(&row)
    }
    /// # Errors
    ///
    /// Returns an error for invalid address data, missing/conflicting records, or database/audit transaction failure.
    pub async fn get(&self, email: &str) -> Result<Address> {
        let row = sqlx::query("SELECT id,email,original_email,display_name,user_id,verified_on,registered_on FROM addresses WHERE email=$1").bind(email.to_ascii_lowercase()).fetch_optional(&self.db.pool).await.map_err(db_error)?.ok_or_else(|| Error::NotFound(email.into()))?;
        address_from_row(&row)
    }
    /// Lists every address linked to a user in stable email order.
    ///
    /// # Errors
    ///
    /// Returns an error when addresses cannot be queried or decoded.
    pub async fn by_user(&self, user: UserId) -> Result<Vec<Address>> {
        let rows = sqlx::query("SELECT id,email,original_email,display_name,user_id,verified_on,registered_on FROM addresses WHERE user_id=$1 ORDER BY email")
            .bind(user.to_string())
            .fetch_all(&self.db.pool)
            .await
            .map_err(db_error)?;
        rows.iter().map(address_from_row).collect()
    }
    /// # Errors
    ///
    /// Returns an error for invalid address data, missing/conflicting records, or database/audit transaction failure.
    pub async fn verify(&self, email: &str, verified: bool) -> Result<Address> {
        self.verify_with_context(email, verified, &AuditContext::system())
            .await
    }
    /// Changes address verification attributed to the supplied audit context.
    ///
    /// # Errors
    ///
    /// Returns an error if the address is missing or the database/audit transaction fails.
    pub async fn verify_with_context(
        &self,
        email: &str,
        verified: bool,
        context: &AuditContext,
    ) -> Result<Address> {
        let value = verified.then(now);
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        let changed = sqlx::query("UPDATE addresses SET verified_on=$1 WHERE email=$2")
            .bind(value)
            .bind(email.to_ascii_lowercase())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?
            .rows_affected();
        if changed == 0 {
            return Err(Error::NotFound(email.into()));
        }
        Database::record_tx_with_context(
            &mut tx,
            context,
            if verified {
                "address.verify"
            } else {
                "address.unverify"
            },
            "address",
            email,
            serde_json::json!({"verified":verified}),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        self.get(email).await
    }
    /// # Errors
    ///
    /// Returns an error for invalid address data, missing/conflicting records, or database/audit transaction failure.
    pub async fn link(&self, email: &str, user: Option<UserId>) -> Result<Address> {
        self.link_with_context(email, user, &AuditContext::system())
            .await
    }
    /// Changes an address/user link attributed to the supplied audit context.
    ///
    /// # Errors
    ///
    /// Returns an error if a referenced record is missing or the database/audit transaction fails.
    pub async fn link_with_context(
        &self,
        email: &str,
        user: Option<UserId>,
        context: &AuditContext,
    ) -> Result<Address> {
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        let changed = sqlx::query("UPDATE addresses SET user_id=$1 WHERE email=$2")
            .bind(user.map(|v| v.to_string()))
            .bind(email.to_ascii_lowercase())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?
            .rows_affected();
        if changed == 0 {
            return Err(Error::NotFound(email.into()));
        }
        // Membership identity is live ownership for both subscription modes:
        // preference resolution consumes it, so it must not retain the former owner.
        // Keep subscription mode, address, and historical audit attribution intact.
        sqlx::query("UPDATE members SET user_id=$1 WHERE address_id=(SELECT id FROM addresses WHERE email=$2)")
            .bind(user.map(|value| value.to_string()))
            .bind(email.to_ascii_lowercase())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        // Relink away and back must not resurrect a previously selected delivery plan.
        sqlx::query("UPDATE preferences SET delivery_generation=delivery_generation+1 WHERE id IN (SELECT m.preferences_id FROM members m JOIN addresses a ON a.id=m.address_id WHERE a.email=$1)")
            .bind(email.to_ascii_lowercase()).execute(&mut *tx).await.map_err(db_error)?;
        sqlx::query("UPDATE users SET preferred_address_id=NULL WHERE preferred_address_id=(SELECT id FROM addresses WHERE email=$1) AND id<>COALESCE($2, '')")
            .bind(email.to_ascii_lowercase())
            .bind(user.map(|value| value.to_string()))
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        Database::record_tx_with_context(
            &mut tx,
            context,
            "address.link",
            "address",
            email,
            serde_json::json!({"user_id":user}),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        self.get(email).await
    }
}
fn address_from_row(row: &sqlx::any::AnyRow) -> Result<Address> {
    Ok(Address {
        id: parse_uuid(row.try_get::<String, _>("id").map_err(db_error)?.as_str())?,
        email: row.try_get("email").map_err(db_error)?,
        original_email: row.try_get("original_email").map_err(db_error)?,
        display_name: row.try_get("display_name").map_err(db_error)?,
        user_id: row
            .try_get::<Option<String>, _>("user_id")
            .map_err(db_error)?
            .map(|v| parse_uuid(&v))
            .transpose()?,
        verified_on: row
            .try_get::<Option<String>, _>("verified_on")
            .map_err(db_error)?
            .map(|v| parse_time(&v))
            .transpose()?,
        registered_on: parse_time(
            &row.try_get::<String, _>("registered_on")
                .map_err(db_error)?,
        )?,
    })
}

/// What a config patch does to the write-only `moderator_password` column.
#[derive(Debug, Clone)]
pub(crate) enum PasswordChange {
    Unchanged,
    Clear,
    /// Argon2id PHC string, hashed before the transaction began.
    Set(String),
}

#[derive(Debug, Clone, Copy)]
pub struct ListRepo<'a> {
    db: &'a Database,
}
impl ListRepo<'_> {
    /// # Errors
    ///
    /// Returns an error for invalid list data, missing/conflicting records, or database/audit transaction failure.
    pub async fn create(&self, new: NewList) -> Result<MailingList> {
        self.create_with_context(new, &AuditContext::system()).await
    }
    /// Creates a list attributed to the supplied audit context.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid or conflicting list data, a missing domain, or database/audit transaction failure.
    pub async fn create_with_context(
        &self,
        new: NewList,
        context: &AuditContext,
    ) -> Result<MailingList> {
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        let list = Self::create_tx(&mut tx, new, context).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(list)
    }
    /// Creates a list inside `tx`: the domain must exist, the style must be
    /// built in, and the `list.create` event is recorded in the same
    /// transaction. The caller commits.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing domain, an unknown style, a conflicting
    /// id, or database/audit failure.
    pub(crate) async fn create_tx(
        tx: &mut Transaction<'_, Any>,
        new: NewList,
        context: &AuditContext,
    ) -> Result<MailingList> {
        let domain: Option<String> =
            sqlx::query_scalar("SELECT mail_host FROM domains WHERE mail_host=$1")
                .bind(new.list_id.mail_host())
                .fetch_optional(&mut **tx)
                .await
                .map_err(db_error)?;
        if domain.is_none() {
            return Err(Error::NotFound(new.list_id.mail_host().to_owned()));
        }
        let mut list = MailingList::new(new.list_id, new.display_name);
        let style = builtin_styles()
            .into_iter()
            .find(|s| s.name() == new.style)
            .ok_or_else(|| Error::Validation(format!("unknown style: {}", new.style)))?;
        style.apply(&mut list);
        sqlx::query("INSERT INTO mailing_lists(delivery_incarnation,list_id,list_name,mail_host,display_name,description,info,subject_prefix,advertised,preferred_language,anonymous_list,created_at,post_id,volume,next_digest_number,digest_last_sent_at,emergency,archive_policy,archive_rendering_mode,style_name,default_member_action,default_nonmember_action) VALUES($22,$1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21)")
            .bind(list.id.to_string()).bind(list.id.list_name()).bind(list.id.mail_host()).bind(&list.display_name).bind(&list.description).bind(&list.info).bind(&list.subject_prefix).bind(i64::from(list.advertised)).bind(&list.preferred_language).bind(i64::from(list.anonymous_list)).bind(list.created_at.to_rfc3339()).bind(list.post_id).bind(list.volume).bind(list.next_digest_number).bind(list.digest_last_sent_at.map(|value| value.to_rfc3339())).bind(i64::from(list.emergency)).bind(list.archive_policy.to_string()).bind(list.archive_rendering_mode.to_string()).bind(&list.style_name).bind(list.default_member_action.map(|value| value.to_string())).bind(list.default_nonmember_action.map(|value| value.to_string())).bind(Uuid::now_v7().to_string()).execute(&mut **tx).await.map_err(db_error)?;
        Database::record_tx_with_context(
            tx,
            context,
            "list.create",
            "list",
            list.id.as_str(),
            serde_json::json!({"style":list.style_name}),
        )
        .await?;
        Ok(list)
    }
    /// # Errors
    ///
    /// Returns an error for invalid list data, missing/conflicting records, or database/audit transaction failure.
    pub async fn get(&self, id: &ListId) -> Result<MailingList> {
        let row = sqlx::query("SELECT * FROM mailing_lists WHERE list_id=$1")
            .bind(id.as_str())
            .fetch_optional(&self.db.pool)
            .await
            .map_err(db_error)?
            .ok_or_else(|| Error::NotFound(id.to_string()))?;
        list_from_row(&row)
    }
    /// # Errors
    ///
    /// Returns an error for invalid list data, missing/conflicting records, or database/audit transaction failure.
    pub async fn list(&self, advertised: Option<bool>) -> Result<Vec<MailingList>> {
        let rows = if let Some(value) = advertised {
            sqlx::query("SELECT * FROM mailing_lists WHERE advertised=$1 ORDER BY list_id")
                .bind(i64::from(value))
                .fetch_all(&self.db.pool)
                .await
        } else {
            sqlx::query("SELECT * FROM mailing_lists ORDER BY list_id")
                .fetch_all(&self.db.pool)
                .await
        }
        .map_err(db_error)?;
        rows.iter().map(list_from_row).collect()
    }
    /// # Errors
    ///
    /// Returns an error for invalid list data, missing/conflicting records, or database/audit transaction failure.
    pub async fn by_domain(&self, host: &str) -> Result<Vec<MailingList>> {
        let rows = sqlx::query("SELECT * FROM mailing_lists WHERE mail_host=$1 ORDER BY list_id")
            .bind(host)
            .fetch_all(&self.db.pool)
            .await
            .map_err(db_error)?;
        rows.iter().map(list_from_row).collect()
    }
    /// # Errors
    ///
    /// Returns an error for invalid list data, missing/conflicting records, or database/audit transaction failure.
    pub async fn update(&self, id: &ListId, patch: &serde_json::Value) -> Result<MailingList> {
        self.update_with_context(id, patch, &AuditContext::system())
            .await
    }
    /// Updates list configuration attributed to the supplied audit context.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid fields, a missing list, or database/audit transaction failure.
    pub async fn update_with_context(
        &self,
        id: &ListId,
        patch: &serde_json::Value,
        context: &AuditContext,
    ) -> Result<MailingList> {
        // Hash outside the transaction: Argon2 is deliberately slow and must
        // not hold the list row lock while it runs.
        let password = match patch.get("moderator_password") {
            Some(value) => self.moderator_password_change(value)?,
            None => PasswordChange::Unchanged,
        };
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        let list = Self::update_tx_with_password(&mut tx, id, patch, context, password).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(list)
    }

    fn posting_limit(value: &serde_json::Value, key: &str) -> Result<u32> {
        value
            .as_u64()
            .filter(|number| *number <= 2_147_483_647)
            .and_then(|number| u32::try_from(number).ok())
            .ok_or_else(|| Error::Validation(key.into()))
    }

    fn patch_dmarc(
        settings: &mut listmngr_core::DmarcSettings,
        object: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<()> {
        if let Some(value) = object.get("dmarc_mitigate_action") {
            settings.action = serde_json::from_value(value.clone())
                .map_err(|_| Error::Validation("dmarc_mitigate_action".into()))?;
        }
        if let Some(value) = object.get("dmarc_mitigate_unconditionally") {
            settings.unconditional = value
                .as_bool()
                .ok_or_else(|| Error::Validation("dmarc_mitigate_unconditionally".into()))?;
        }
        Ok(())
    }

    fn patch_boolean(list: &mut MailingList, key: &str, value: &serde_json::Value) -> Result<()> {
        let target = match key {
            "advertised" => &mut list.advertised,
            "anonymous_list" => &mut list.anonymous_list,
            "send_welcome_message" => &mut list.send_welcome_message,
            "send_goodbye_message" => &mut list.send_goodbye_message,
            "bounce_notify_owner_on_removal" => &mut list.bounce_notify_owner_on_removal,
            "process_bounces" => &mut list.process_bounces,
            "bounce_notify_owner_on_disable" => &mut list.bounce_notify_owner_on_disable,
            "bounce_notify_owner_on_bounce_increment" => {
                &mut list.bounce_notify_owner_on_bounce_increment
            }
            "emergency" => &mut list.emergency,
            "administrivia" => &mut list.administrivia,
            "require_explicit_destination" => &mut list.require_explicit_destination,
            "respond_to_post_requests" => &mut list.respond_to_post_requests,
            "admin_immed_notify" => &mut list.admin_immed_notify,
            "admin_notify_mchanges" => &mut list.admin_notify_mchanges,
            "digests_enabled" => &mut list.digests_enabled,
            "digest_send_periodic" => &mut list.digest_send_periodic,
            "filter_content" => &mut list.alter_messages.filter_content,
            "collapse_alternatives" => &mut list.alter_messages.collapse_alternatives,
            "convert_html_to_plaintext" => &mut list.alter_messages.convert_html_to_plaintext,
            "include_rfc2369_headers" => &mut list.alter_messages.include_rfc2369_headers,
            "allow_list_posts" => &mut list.alter_messages.allow_list_posts,
            "first_strip_reply_to" => &mut list.alter_messages.first_strip_reply_to,
            "include_sender_header" => &mut list.alter_messages.include_sender_header,
            "topics_enabled" => &mut list.topics_enabled,
            _ => return Err(Error::Validation(key.into())),
        };
        *target = value
            .as_bool()
            .ok_or_else(|| Error::Validation(key.into()))?;
        Ok(())
    }

    /// The Alter Messages, Member Policy, DMARC text and bounce-forwarding
    /// settings: enums by wire name, bounded text, validated token lists.
    fn patch_alter_messages(
        list: &mut MailingList,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<()> {
        let invalid = || Error::Validation(key.to_owned());
        let messages = &mut list.alter_messages;
        match key {
            "filter_types" => messages.filter_types = parse_token_list(key, value, true)?,
            "pass_types" => messages.pass_types = parse_token_list(key, value, true)?,
            "filter_extensions" => {
                messages.filter_extensions = parse_token_list(key, value, false)?;
            }
            "pass_extensions" => messages.pass_extensions = parse_token_list(key, value, false)?,
            "filter_action" => messages.filter_action = parse_enum(key, value)?,
            "reply_goes_to_list" => messages.reply_goes_to_list = parse_enum(key, value)?,
            "personalize" => messages.personalize = parse_enum(key, value)?,
            "reply_to_address" => {
                let text = value.as_str().ok_or_else(invalid)?;
                if !text.is_empty() {
                    listmngr_core::Address::new(text, String::new()).map_err(|_| invalid())?;
                }
                messages.reply_to_address = text.into();
            }
            "subscription_policy" => {
                list.member_policy.subscription_policy = parse_enum(key, value)?;
            }
            "unsubscription_policy" => {
                list.member_policy.unsubscription_policy = parse_enum(key, value)?;
            }
            "member_roster_visibility" => {
                list.member_policy.member_roster_visibility = parse_enum(key, value)?;
            }
            "forward_unrecognized_bounces_to" => {
                list.forward_unrecognized_bounces_to = parse_enum(key, value)?;
            }
            "autorespond_owner" => {
                list.automatic_responses.autorespond_owner = parse_enum(key, value)?;
            }
            "autorespond_postings" => {
                list.automatic_responses.autorespond_postings = parse_enum(key, value)?;
            }
            "autorespond_requests" => {
                list.automatic_responses.autorespond_requests = parse_enum(key, value)?;
            }
            "autoresponse_owner_text"
            | "autoresponse_postings_text"
            | "autoresponse_request_text" => {
                let text = value
                    .as_str()
                    .filter(|text| text.len() <= SETTING_TEXT_BYTES)
                    .ok_or_else(invalid)?;
                let responses = &mut list.automatic_responses;
                match key {
                    "autoresponse_owner_text" => responses.autoresponse_owner_text = text.into(),
                    "autoresponse_postings_text" => {
                        responses.autoresponse_postings_text = text.into();
                    }
                    _ => responses.autoresponse_request_text = text.into(),
                }
            }
            "autoresponse_grace_period" => {
                list.automatic_responses.autoresponse_grace_period = value
                    .as_i64()
                    .filter(|days| (0..=3650).contains(days))
                    .and_then(|days| i32::try_from(days).ok())
                    .ok_or_else(invalid)?;
            }
            "dmarc_addresses" => list.dmarc.dmarc_addresses = parse_address_list(key, value)?,
            "topics_bodylines_limit" => {
                list.topics_bodylines_limit = value
                    .as_i64()
                    .filter(|limit| (-1..=10_000).contains(limit))
                    .and_then(|limit| i32::try_from(limit).ok())
                    .ok_or_else(invalid)?;
            }
            "topics" => list.topics = parse_topics(value)?,
            "dmarc_moderation_notice" | "dmarc_wrapped_message_text" => {
                let text = value
                    .as_str()
                    .filter(|text| text.len() <= SETTING_TEXT_BYTES)
                    .ok_or_else(invalid)?;
                if key == "dmarc_moderation_notice" {
                    list.dmarc.dmarc_moderation_notice = text.into();
                } else {
                    list.dmarc.dmarc_wrapped_message_text = text.into();
                }
            }
            _ => return Err(invalid()),
        }
        Ok(())
    }

    fn patch_address_list(
        list: &mut MailingList,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<()> {
        let parsed = parse_address_list(key, value)?;
        let target = match key {
            "acceptable_aliases" => &mut list.acceptable_aliases,
            "accept_these_nonmembers" => &mut list.accept_these_nonmembers,
            "hold_these_nonmembers" => &mut list.hold_these_nonmembers,
            "reject_these_nonmembers" => &mut list.reject_these_nonmembers,
            "discard_these_nonmembers" => &mut list.discard_these_nonmembers,
            _ => return Err(Error::Validation(key.into())),
        };
        *target = parsed;
        Ok(())
    }

    /// Hash a new `Approved:` posting key, or clear it with an empty string.
    fn moderator_password_change(self, value: &serde_json::Value) -> Result<PasswordChange> {
        let invalid = || Error::Validation("moderator_password".into());
        let plaintext = value.as_str().ok_or_else(invalid)?;
        if plaintext.is_empty() {
            return Ok(PasswordChange::Clear);
        }
        if plaintext.len() > 1024 {
            return Err(invalid());
        }
        let hasher = self.db.users().password_hasher()?;
        let salt = SaltString::generate(&mut OsRng);
        Ok(PasswordChange::Set(
            hasher
                .hash_password(plaintext.as_bytes(), &salt)
                .map_err(db_error)?
                .to_string(),
        ))
    }

    fn patch_posting_pipeline(list: &mut MailingList, value: &serde_json::Value) -> Result<()> {
        let name = value
            .as_str()
            .ok_or_else(|| Error::Validation("posting_pipeline".into()))?;
        let registry = listmngr_mail::handlers::builtin_registry();
        let pipeline = registry
            .pipeline(name)
            .ok_or_else(|| Error::Validation(format!("unknown posting pipeline: {name}")))?;
        if !registry.is_executable(pipeline) || !pipeline.delivers_posts() {
            return Err(Error::Validation(format!(
                "{name} cannot deliver list posts"
            )));
        }
        list.posting_pipeline = name.into();
        Ok(())
    }

    fn patch_text(list: &mut MailingList, key: &str, value: &serde_json::Value) -> Result<()> {
        let target = match key {
            "display_name" => &mut list.display_name,
            "description" => &mut list.description,
            "info" => &mut list.info,
            _ => &mut list.subject_prefix,
        };
        *target = value
            .as_str()
            .filter(|text| key != "subject_prefix" || !text.contains(['\r', '\n']))
            .ok_or_else(|| Error::Validation(key.into()))?
            .into();
        Ok(())
    }

    /// Apply one patch key to the in-memory list. `moderator_password` is
    /// only checked for presence here: the caller hashed it before the
    /// transaction began.
    fn apply_patch_key(
        list: &mut MailingList,
        key: &str,
        value: &serde_json::Value,
        password: &PasswordChange,
    ) -> Result<()> {
        match key {
            "moderator_password" => {
                if matches!(password, PasswordChange::Unchanged) {
                    return Err(Error::Validation(key.into()));
                }
            }
            "acceptable_aliases"
            | "accept_these_nonmembers"
            | "hold_these_nonmembers"
            | "reject_these_nonmembers"
            | "discard_these_nonmembers" => Self::patch_address_list(list, key, value)?,
            "posting_pipeline" => Self::patch_posting_pipeline(list, value)?,
            "display_name" | "description" | "info" | "subject_prefix" => {
                Self::patch_text(list, key, value)?;
            }
            "default_member_action" | "default_nonmember_action" => {
                let action = serde_json::from_value(value.clone())
                    .map_err(|_| Error::Validation(key.into()))?;
                if key == "default_member_action" {
                    list.default_member_action = action;
                } else {
                    list.default_nonmember_action = action;
                }
            }
            "advertised"
            | "anonymous_list"
            | "send_welcome_message"
            | "send_goodbye_message"
            | "bounce_notify_owner_on_removal"
            | "process_bounces"
            | "bounce_notify_owner_on_disable"
            | "bounce_notify_owner_on_bounce_increment"
            | "emergency"
            | "administrivia"
            | "require_explicit_destination"
            | "respond_to_post_requests"
            | "admin_immed_notify"
            | "admin_notify_mchanges"
            | "digests_enabled"
            | "digest_send_periodic"
            | "filter_content"
            | "collapse_alternatives"
            | "convert_html_to_plaintext"
            | "include_rfc2369_headers"
            | "allow_list_posts"
            | "first_strip_reply_to"
            | "include_sender_header"
            | "topics_enabled" => Self::patch_boolean(list, key, value)?,
            "filter_types"
            | "pass_types"
            | "filter_extensions"
            | "pass_extensions"
            | "filter_action"
            | "reply_goes_to_list"
            | "reply_to_address"
            | "personalize"
            | "subscription_policy"
            | "unsubscription_policy"
            | "member_roster_visibility"
            | "forward_unrecognized_bounces_to"
            | "dmarc_addresses"
            | "dmarc_moderation_notice"
            | "dmarc_wrapped_message_text"
            | "topics_bodylines_limit"
            | "autorespond_owner"
            | "autorespond_postings"
            | "autorespond_requests"
            | "autoresponse_owner_text"
            | "autoresponse_postings_text"
            | "autoresponse_request_text"
            | "autoresponse_grace_period"
            | "topics" => Self::patch_alter_messages(list, key, value)?,
            "preferred_language" => {
                let language = value
                    .as_str()
                    .filter(|language| !language.trim().is_empty())
                    .ok_or_else(|| Error::Validation(key.into()))?;
                list.preferred_language = language.into();
            }
            "dmarc_mitigate_action" | "dmarc_mitigate_unconditionally" => {}
            "next_digest_number" | "digest_size_threshold" | "digest_volume_frequency" => {
                Self::patch_digest_setting(list, key, value)?;
            }
            "bounce_you_are_disabled_warnings"
            | "bounce_you_are_disabled_warnings_interval"
            | "bounce_score_threshold"
            | "bounce_info_stale_after" => Self::patch_bounce_setting(list, key, value)?,
            "max_message_size" => list.max_message_size = Self::posting_limit(value, key)?,
            "max_num_recipients" => list.max_num_recipients = Self::posting_limit(value, key)?,
            "archive_policy" => list.archive_policy = parse_enum(key, value)?,
            "archive_rendering_mode" => list.archive_rendering_mode = parse_enum(key, value)?,
            _ => {
                return Err(Error::Validation(format!(
                    "read-only or unknown list setting: {key}"
                )));
            }
        }
        Ok(())
    }

    async fn persist_alter_messages(
        tx: &mut Transaction<'_, Any>,
        list: &MailingList,
    ) -> Result<()> {
        let encode = |entries: &Vec<String>| {
            serde_json::to_string(entries).expect("string vector serializes")
        };
        let messages = &list.alter_messages;
        sqlx::query("UPDATE mailing_lists SET filter_content=$1,filter_types=$2,pass_types=$3,filter_extensions=$4,pass_extensions=$5,collapse_alternatives=$6,convert_html_to_plaintext=$7,filter_action=$8,include_rfc2369_headers=$9,allow_list_posts=$10,reply_goes_to_list=$11,reply_to_address=$12,first_strip_reply_to=$13,personalize=$14,include_sender_header=$15,subscription_policy=$16,unsubscription_policy=$17,member_roster_visibility=$18,dmarc_addresses=$19,dmarc_moderation_notice=$20,dmarc_wrapped_message_text=$21,forward_unrecognized_bounces_to=$22 WHERE list_id=$23")
            .bind(i64::from(messages.filter_content))
            .bind(encode(&messages.filter_types))
            .bind(encode(&messages.pass_types))
            .bind(encode(&messages.filter_extensions))
            .bind(encode(&messages.pass_extensions))
            .bind(i64::from(messages.collapse_alternatives))
            .bind(i64::from(messages.convert_html_to_plaintext))
            .bind(messages.filter_action.as_str())
            .bind(i64::from(messages.include_rfc2369_headers))
            .bind(i64::from(messages.allow_list_posts))
            .bind(messages.reply_goes_to_list.as_str())
            .bind(&messages.reply_to_address)
            .bind(i64::from(messages.first_strip_reply_to))
            .bind(messages.personalize.as_str())
            .bind(i64::from(messages.include_sender_header))
            .bind(list.member_policy.subscription_policy.as_str())
            .bind(list.member_policy.unsubscription_policy.as_str())
            .bind(list.member_policy.member_roster_visibility.as_str())
            .bind(encode(&list.dmarc.dmarc_addresses))
            .bind(&list.dmarc.dmarc_moderation_notice)
            .bind(&list.dmarc.dmarc_wrapped_message_text)
            .bind(list.forward_unrecognized_bounces_to.as_str())
            .bind(list.id.as_str())
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
        let responses = &list.automatic_responses;
        sqlx::query("UPDATE mailing_lists SET autorespond_owner=$1,autoresponse_owner_text=$2,autorespond_postings=$3,autoresponse_postings_text=$4,autorespond_requests=$5,autoresponse_request_text=$6,autoresponse_grace_period=$7 WHERE list_id=$8")
            .bind(responses.autorespond_owner.as_str())
            .bind(&responses.autoresponse_owner_text)
            .bind(responses.autorespond_postings.as_str())
            .bind(&responses.autoresponse_postings_text)
            .bind(responses.autorespond_requests.as_str())
            .bind(&responses.autoresponse_request_text)
            .bind(i64::from(responses.autoresponse_grace_period))
            .bind(list.id.as_str())
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
        sqlx::query("UPDATE mailing_lists SET topics_enabled=$1,topics_bodylines_limit=$2,topics=$3 WHERE list_id=$4")
            .bind(i64::from(list.topics_enabled))
            .bind(i64::from(list.topics_bodylines_limit))
            .bind(serde_json::to_string(&list.topics).expect("topics serialize"))
            .bind(list.id.as_str())
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
        Ok(())
    }

    fn patch_digest_setting(
        list: &mut MailingList,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<()> {
        match key {
            "next_digest_number" => {
                list.next_digest_number = value
                    .as_i64()
                    .filter(|number| *number >= 1)
                    .ok_or_else(|| Error::Validation(key.into()))?;
            }
            "digest_size_threshold" => {
                list.digest_size_threshold = value
                    .as_f64()
                    .filter(|kib| kib.is_finite() && (0.0..=1_048_576.0).contains(kib))
                    .ok_or_else(|| Error::Validation(key.into()))?;
            }
            _ => list.digest_volume_frequency = parse_enum(key, value)?,
        }
        Ok(())
    }

    async fn persist_acceptance(tx: &mut Transaction<'_, Any>, list: &MailingList) -> Result<()> {
        let encode = |entries: &Vec<String>| {
            serde_json::to_string(entries).expect("string vector serializes")
        };
        sqlx::query("UPDATE mailing_lists SET digests_enabled=$1,digest_size_threshold=$2,digest_send_periodic=$3,digest_volume_frequency=$4 WHERE list_id=$5")
            .bind(i64::from(list.digests_enabled))
            .bind(list.digest_size_threshold)
            .bind(i64::from(list.digest_send_periodic))
            .bind(list.digest_volume_frequency.as_str())
            .bind(list.id.as_str())
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
        sqlx::query("UPDATE mailing_lists SET administrivia=$1,require_explicit_destination=$2,acceptable_aliases=$3,accept_these_nonmembers=$4,hold_these_nonmembers=$5,reject_these_nonmembers=$6,discard_these_nonmembers=$7,posting_pipeline=$9,respond_to_post_requests=$10,admin_immed_notify=$11,admin_notify_mchanges=$12 WHERE list_id=$8")
            .bind(i64::from(list.administrivia))
            .bind(i64::from(list.require_explicit_destination))
            .bind(encode(&list.acceptable_aliases))
            .bind(encode(&list.accept_these_nonmembers))
            .bind(encode(&list.hold_these_nonmembers))
            .bind(encode(&list.reject_these_nonmembers))
            .bind(encode(&list.discard_these_nonmembers))
            .bind(list.id.as_str())
            .bind(&list.posting_pipeline)
            .bind(i64::from(list.respond_to_post_requests))
            .bind(i64::from(list.admin_immed_notify))
            .bind(i64::from(list.admin_notify_mchanges))
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
        Ok(())
    }

    /// Whether `candidate` is the list's `Approved:` posting key. A list with
    /// no key never verifies. Comparison is Argon2's constant-time verify.
    /// # Errors
    /// Returns a database error or not-found for the list.
    pub async fn verify_moderator_password(&self, id: &ListId, candidate: &str) -> Result<bool> {
        let stored: Option<Option<String>> =
            sqlx::query_scalar("SELECT moderator_password FROM mailing_lists WHERE list_id=$1")
                .bind(id.as_str())
                .fetch_optional(&self.db.pool)
                .await
                .map_err(db_error)?;
        let Some(stored) = stored else {
            return Err(Error::NotFound(id.to_string()));
        };
        let Some(hash) = stored else {
            return Ok(false);
        };
        if candidate.is_empty() || candidate.len() > 1024 {
            return Ok(false);
        }
        let parsed = PasswordHash::new(&hash).map_err(db_error)?;
        Ok(Argon2::default()
            .verify_password(candidate.as_bytes(), &parsed)
            .is_ok())
    }

    async fn persist_lifecycle(tx: &mut Transaction<'_, Any>, list: &MailingList) -> Result<()> {
        sqlx::query("UPDATE mailing_lists SET send_welcome_message=$1, max_num_recipients=$3, send_goodbye_message=$4 WHERE list_id=$2")
            .bind(i64::from(list.send_welcome_message))
            .bind(list.id.as_str())
            .bind(i64::from(list.max_num_recipients))
            .bind(i64::from(list.send_goodbye_message))
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
        Ok(())
    }

    async fn persist_maintenance(tx: &mut Transaction<'_, Any>, list: &MailingList) -> Result<()> {
        sqlx::query("UPDATE mailing_lists SET bounce_you_are_disabled_warnings=$1,bounce_you_are_disabled_warnings_interval=$2,bounce_notify_owner_on_removal=$3 WHERE list_id=$4")
            .bind(i64::from(list.bounce_you_are_disabled_warnings)).bind(i64::from(list.bounce_you_are_disabled_warnings_interval)).bind(i64::from(list.bounce_notify_owner_on_removal)).bind(list.id.as_str()).execute(&mut **tx).await.map_err(db_error)?;
        Ok(())
    }

    fn patch_bounce_setting(
        list: &mut MailingList,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<()> {
        if matches!(
            key,
            "bounce_you_are_disabled_warnings" | "bounce_you_are_disabled_warnings_interval"
        ) {
            let (target, max) = if key == "bounce_you_are_disabled_warnings" {
                (&mut list.bounce_you_are_disabled_warnings, 100)
            } else {
                (&mut list.bounce_you_are_disabled_warnings_interval, 36500)
            };
            *target = value
                .as_u64()
                .filter(|n| *n <= max)
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| Error::Validation(key.into()))?;
        } else if key == "bounce_score_threshold" {
            list.bounce_score_threshold = value
                .as_f64()
                .filter(|n| n.is_finite() && *n > 0.0 && *n <= 1_000_000.0)
                .ok_or_else(|| Error::Validation(key.into()))?;
        } else {
            list.bounce_info_stale_after = value
                .as_u64()
                .filter(|n| (1..=3650).contains(n))
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| Error::Validation(key.into()))?;
        }
        Ok(())
    }

    /// The list `patch` would produce, validated exactly as a save is, with
    /// nothing written: what a preview shows.
    /// # Errors
    /// The same validation errors a save returns.
    pub fn validate_patch(current: &MailingList, patch: &serde_json::Value) -> Result<MailingList> {
        let mut list = current.clone();
        let object = patch
            .as_object()
            .ok_or_else(|| Error::Validation("config patch must be an object".into()))?;
        for (key, value) in object {
            Self::apply_patch_key(&mut list, key, value, &PasswordChange::Unchanged)?;
        }
        Self::patch_dmarc(&mut list.dmarc, object)?;
        Ok(list)
    }

    pub(crate) async fn update_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Any>,
        id: &ListId,
        patch: &serde_json::Value,
        context: &AuditContext,
    ) -> Result<MailingList> {
        Self::update_tx_with_password(tx, id, patch, context, PasswordChange::Unchanged).await
    }

    /// `password` is the pre-hashed `moderator_password` change when the
    /// patch carried one; the patch itself is audited with that key redacted.
    pub(crate) async fn update_tx_with_password(
        tx: &mut sqlx::Transaction<'_, sqlx::Any>,
        id: &ListId,
        patch: &serde_json::Value,
        context: &AuditContext,
        password: PasswordChange,
    ) -> Result<MailingList> {
        let mut list = lock_list_for_patch(tx, id).await?;
        let object = patch
            .as_object()
            .ok_or_else(|| Error::Validation("config patch must be an object".into()))?;
        for (key, value) in object {
            Self::apply_patch_key(&mut list, key, value, &password)?;
        }
        Self::patch_dmarc(&mut list.dmarc, object)?;
        Self::persist_maintenance(tx, &list).await?;
        Self::persist_acceptance(tx, &list).await?;
        Self::persist_alter_messages(tx, &list).await?;
        if !matches!(password, PasswordChange::Unchanged) {
            let column = match password {
                PasswordChange::Set(hash) => Some(hash),
                PasswordChange::Clear | PasswordChange::Unchanged => None,
            };
            sqlx::query("UPDATE mailing_lists SET moderator_password=$1 WHERE list_id=$2")
                .bind(column)
                .bind(id.as_str())
                .execute(&mut **tx)
                .await
                .map_err(db_error)?;
        }
        sqlx::query("UPDATE mailing_lists SET process_bounces=$1,bounce_info_stale_after=$2,bounce_score_threshold=$4,bounce_notify_owner_on_disable=$5,bounce_notify_owner_on_bounce_increment=$6 WHERE list_id=$3")
            .bind(i64::from(list.process_bounces)).bind(i64::from(list.bounce_info_stale_after)).bind(id.as_str()).bind(list.bounce_score_threshold).bind(i64::from(list.bounce_notify_owner_on_disable)).bind(i64::from(list.bounce_notify_owner_on_bounce_increment)).execute(&mut **tx).await.map_err(db_error)?;
        Self::persist_lifecycle(tx, &list).await?;
        sqlx::query("UPDATE mailing_lists SET display_name=$1,description=$2,info=$3,subject_prefix=$4,advertised=$5,preferred_language=$6,anonymous_list=$7,next_digest_number=$8,emergency=$9,archive_policy=$10,archive_rendering_mode=$11,default_member_action=$13,default_nonmember_action=$14,max_message_size=$15,dmarc_mitigate_action=$16,dmarc_mitigate_unconditionally=$17 WHERE list_id=$12")
            .bind(&list.display_name).bind(&list.description).bind(&list.info).bind(&list.subject_prefix).bind(i64::from(list.advertised)).bind(&list.preferred_language).bind(i64::from(list.anonymous_list)).bind(list.next_digest_number).bind(i64::from(list.emergency)).bind(list.archive_policy.to_string()).bind(list.archive_rendering_mode.to_string()).bind(id.as_str()).bind(list.default_member_action.map(|value| value.to_string())).bind(list.default_nonmember_action.map(|value| value.to_string())).bind(i64::from(list.max_message_size)).bind(serde_json::to_value(list.dmarc.action).expect("serialize action").as_str().expect("action string")).bind(i64::from(list.dmarc.unconditional)).execute(&mut **tx).await.map_err(db_error)?;
        let mut audited = patch.clone();
        if let Some(object) = audited.as_object_mut()
            && object.contains_key("moderator_password")
        {
            // Never persist the plaintext or the hash in the audit trail.
            object.insert(
                "moderator_password".into(),
                serde_json::Value::String("[redacted]".into()),
            );
        }
        Database::record_tx_with_context(tx, context, "list.config", "list", id.as_str(), audited)
            .await?;
        Ok(list)
    }
    /// # Errors
    ///
    /// Returns an error for invalid list data, missing/conflicting records, or database/audit transaction failure.
    pub async fn delete(&self, id: &ListId) -> Result<()> {
        self.delete_with_context(id, &AuditContext::system()).await
    }
    /// Deletes a list-owned graph attributed to the supplied audit context.
    ///
    /// # Errors
    ///
    /// Returns an error if the list is missing or any database/audit transaction step fails.
    pub async fn delete_with_context(&self, id: &ListId, context: &AuditContext) -> Result<()> {
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        Self::delete_tx(&mut tx, self.db, id, context).await?;
        tx.commit().await.map_err(db_error)
    }

    /// [`Self::delete_with_context`] inside the caller's transaction.
    pub(crate) async fn delete_tx(
        tx: &mut Transaction<'_, Any>,
        db: &Database,
        id: &ListId,
        context: &AuditContext,
    ) -> Result<()> {
        let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mailing_lists WHERE list_id=$1")
            .bind(id.as_str())
            .fetch_one(&mut **tx)
            .await
            .map_err(db_error)?;
        if exists == 0 {
            return Err(Error::NotFound(id.to_string()));
        }

        // Keep this explicit rather than relying on backend-specific cascades or
        // data-modifying CTEs: both SQLite and PostgreSQL execute every step in
        // the same SQLx transaction. Member preferences are list-owned, while
        // their users and addresses are shared identity records.
        let preference_rows = sqlx::query("SELECT preferences_id FROM members WHERE list_id=$1")
            .bind(id.as_str())
            .fetch_all(&mut **tx)
            .await
            .map_err(db_error)?;
        let preference_ids = preference_rows
            .iter()
            .map(|row| row.try_get::<String, _>("preferences_id"))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_error)?;

        digests::delete_list(tx, id).await?;
        for table in ["list_archivers", "header_matches", "bans"] {
            sqlx::query(&format!("DELETE FROM {table} WHERE list_id=$1"))
                .bind(id.as_str())
                .execute(&mut **tx)
                .await
                .map_err(db_error)?;
        }
        sqlx::query("DELETE FROM templates WHERE scope='list' AND scope_id=$1")
            .bind(id.as_str())
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
        let member_ids: Vec<String> = sqlx::query_scalar("SELECT id FROM members WHERE list_id=$1")
            .bind(id.as_str())
            .fetch_all(&mut **tx)
            .await
            .map_err(db_error)?;
        for member_id in member_ids {
            workflows::delete_member_with_goodbye(tx, db, &member_id).await?;
        }
        for preference_id in preference_ids {
            sqlx::query("DELETE FROM preferences WHERE id=$1")
                .bind(preference_id)
                .execute(&mut **tx)
                .await
                .map_err(db_error)?;
        }
        sqlx::query("DELETE FROM mailing_lists WHERE list_id=$1")
            .bind(id.as_str())
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
        Database::record_tx_with_context(
            tx,
            context,
            "list.delete",
            "list",
            id.as_str(),
            serde_json::json!({}),
        )
        .await
    }

    /// # Errors
    ///
    /// Returns an error for invalid list data, missing/conflicting records, or database/audit transaction failure.
    pub async fn set_archiver(&self, id: &ListId, name: &str, enabled: bool) -> Result<()> {
        self.set_archiver_with_context(id, name, enabled, &AuditContext::system())
            .await
    }

    /// Sets a list archiver attributed to the supplied audit context.
    ///
    /// # Errors
    ///
    /// Returns an error if the list is missing or the database/audit transaction fails.
    pub async fn set_archiver_with_context(
        &self,
        id: &ListId,
        name: &str,
        enabled: bool,
        context: &AuditContext,
    ) -> Result<()> {
        self.get(id).await?;
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        Self::set_archiver_tx(&mut tx, id, name, enabled, context).await?;
        tx.commit().await.map_err(db_error)
    }

    /// [`Self::set_archiver_with_context`] inside the caller's transaction.
    pub(crate) async fn set_archiver_tx(
        tx: &mut Transaction<'_, Any>,
        id: &ListId,
        name: &str,
        enabled: bool,
        context: &AuditContext,
    ) -> Result<()> {
        sqlx::query("INSERT INTO list_archivers(list_id,name,enabled) VALUES($1,$2,$3) ON CONFLICT(list_id,name) DO UPDATE SET enabled=excluded.enabled")
            .bind(id.as_str()).bind(name).bind(i64::from(enabled)).execute(&mut **tx).await.map_err(db_error)?;
        Database::record_tx_with_context(
            tx,
            context,
            "list.archiver.set",
            "list",
            id.as_str(),
            serde_json::json!({"name":name,"enabled":enabled}),
        )
        .await
    }

    /// # Errors
    ///
    /// Returns an error for invalid list data, missing/conflicting records, or database/audit transaction failure.
    pub async fn archivers(&self, id: &ListId) -> Result<Vec<(String, bool)>> {
        self.get(id).await?;
        let rows =
            sqlx::query("SELECT name,enabled FROM list_archivers WHERE list_id=$1 ORDER BY name")
                .bind(id.as_str())
                .fetch_all(&self.db.pool)
                .await
                .map_err(db_error)?;
        rows.iter()
            .map(|row| {
                Ok((
                    row.try_get("name").map_err(db_error)?,
                    row.try_get::<i64, _>("enabled").map_err(db_error)? != 0,
                ))
            })
            .collect()
    }

    /// # Errors
    ///
    /// Returns an error for invalid list data, missing/conflicting records, or database/audit transaction failure.
    /// Store an inline list-scoped template body. See
    /// [`crate::templates::TemplateRepo::set_body`].
    /// # Errors
    /// Returns validation errors, not-found, or a database/audit failure.
    pub async fn set_template(
        &self,
        id: &ListId,
        name: &str,
        language: &str,
        body: &str,
    ) -> Result<()> {
        self.set_template_with_context(id, name, language, body, &AuditContext::system())
            .await
    }
    /// See [`Self::set_template`].
    /// # Errors
    /// Returns validation errors, not-found, or a database/audit failure.
    pub async fn set_template_with_context(
        &self,
        id: &ListId,
        name: &str,
        language: &str,
        body: &str,
        context: &AuditContext,
    ) -> Result<()> {
        self.db
            .templates()
            .set_body_with_context(
                &crate::templates::Scope::List(id.clone()),
                name,
                language,
                body,
                context,
            )
            .await
    }
    /// # Errors
    /// Returns not-found or a database error.
    pub async fn templates(&self, id: &ListId) -> Result<Vec<Template>> {
        self.get(id).await?;
        let rows = sqlx::query("SELECT id,name,scope,scope_id,language,uri,body FROM templates WHERE scope='list' AND scope_id=$1 ORDER BY name,language")
            .bind(id.as_str()).fetch_all(&self.db.pool).await.map_err(db_error)?;
        rows.iter()
            .map(|row| {
                Ok(Template {
                    id: row.try_get("id").map_err(db_error)?,
                    name: row.try_get("name").map_err(db_error)?,
                    scope: row.try_get("scope").map_err(db_error)?,
                    scope_id: row.try_get("scope_id").map_err(db_error)?,
                    language: row.try_get("language").map_err(db_error)?,
                    uri: row.try_get("uri").map_err(db_error)?,
                    body: row.try_get("body").map_err(db_error)?,
                })
            })
            .collect()
    }
}
async fn lock_list_for_patch(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    id: &ListId,
) -> Result<MailingList> {
    // Writer reservation precedes the snapshot: a waiter must not restore
    // stale unrelated fields that committed while it was waiting for a lock.
    let locked = sqlx::query("UPDATE mailing_lists SET list_id=list_id WHERE list_id=$1")
        .bind(id.as_str())
        .execute(&mut **tx)
        .await
        .map_err(db_error)?
        .rows_affected();
    if locked != 1 {
        return Err(Error::NotFound(id.to_string()));
    }
    let row = sqlx::query("SELECT * FROM mailing_lists WHERE list_id=$1")
        .bind(id.as_str())
        .fetch_one(&mut **tx)
        .await
        .map_err(db_error)?;
    list_from_row(&row)
}

fn row_u32(row: &sqlx::any::AnyRow, key: &str) -> Result<u32> {
    u32::try_from(row.try_get::<i64, _>(key).map_err(db_error)?).map_err(db_error)
}

fn automatic_responses_from_row(
    row: &sqlx::any::AnyRow,
) -> Result<listmngr_core::AutomaticResponses> {
    Ok(listmngr_core::AutomaticResponses {
        autorespond_owner: enum_column(row, "autorespond_owner")?,
        autoresponse_owner_text: row.try_get("autoresponse_owner_text").map_err(db_error)?,
        autorespond_postings: enum_column(row, "autorespond_postings")?,
        autoresponse_postings_text: row
            .try_get("autoresponse_postings_text")
            .map_err(db_error)?,
        autorespond_requests: enum_column(row, "autorespond_requests")?,
        autoresponse_request_text: row.try_get("autoresponse_request_text").map_err(db_error)?,
        autoresponse_grace_period: i32::try_from(
            row.try_get::<i64, _>("autoresponse_grace_period")
                .map_err(db_error)?,
        )
        .unwrap_or(90),
    })
}

fn list_from_row(row: &sqlx::any::AnyRow) -> Result<MailingList> {
    let bounce_flags = bounce_flags_from_row(row)?;
    Ok(MailingList {
        id: row
            .try_get::<String, _>("list_id")
            .map_err(db_error)?
            .parse()?,
        display_name: row.try_get("display_name").map_err(db_error)?,
        description: row.try_get("description").map_err(db_error)?,
        info: row.try_get("info").map_err(db_error)?,
        subject_prefix: row.try_get("subject_prefix").map_err(db_error)?,
        advertised: row.try_get::<i64, _>("advertised").map_err(db_error)? != 0,
        preferred_language: row.try_get("preferred_language").map_err(db_error)?,
        anonymous_list: row.try_get::<i64, _>("anonymous_list").map_err(db_error)? != 0,
        send_welcome_message: row
            .try_get::<i64, _>("send_welcome_message")
            .map_err(db_error)?
            != 0,
        bounce_notify_owner_on_disable: bounce_flags.1,
        bounce_you_are_disabled_warnings: row_u32(row, "bounce_you_are_disabled_warnings")?,
        bounce_you_are_disabled_warnings_interval: row_u32(
            row,
            "bounce_you_are_disabled_warnings_interval",
        )?,
        bounce_notify_owner_on_removal: bounce_flags.3,
        process_bounces: bounce_flags.0,
        bounce_notify_owner_on_bounce_increment: bounce_flags.2,
        bounce_score_threshold: row.try_get("bounce_score_threshold").map_err(db_error)?,
        bounce_info_stale_after: u32::try_from(
            row.try_get::<i64, _>("bounce_info_stale_after")
                .map_err(db_error)?,
        )
        .map_err(|_| Error::Validation("bounce_info_stale_after".into()))?,
        send_goodbye_message: row
            .try_get::<i64, _>("send_goodbye_message")
            .map_err(db_error)?
            != 0,
        dmarc: dmarc_from_row(row)?,
        created_at: parse_time(&row.try_get::<String, _>("created_at").map_err(db_error)?)?,
        last_post_at: row
            .try_get::<Option<String>, _>("last_post_at")
            .map_err(db_error)?
            .map(|v| parse_time(&v))
            .transpose()?,
        post_id: row.try_get("post_id").map_err(db_error)?,
        volume: row.try_get("volume").map_err(db_error)?,
        next_digest_number: row.try_get("next_digest_number").map_err(db_error)?,
        digest_last_sent_at: row
            .try_get::<Option<String>, _>("digest_last_sent_at")
            .map_err(db_error)?
            .map(|value| parse_time(&value))
            .transpose()?,
        digests_enabled: flag_column(row, "digests_enabled")?,
        digest_size_threshold: row.try_get("digest_size_threshold").map_err(db_error)?,
        digest_send_periodic: flag_column(row, "digest_send_periodic")?,
        digest_volume_frequency: enum_column(row, "digest_volume_frequency")?,
        emergency: row.try_get::<i64, _>("emergency").map_err(db_error)? != 0,
        max_message_size: row
            .try_get::<i64, _>("max_message_size")
            .map_err(db_error)?
            .try_into()
            .map_err(db_error)?,
        max_num_recipients: row
            .try_get::<i64, _>("max_num_recipients")
            .map_err(db_error)?
            .try_into()
            .map_err(db_error)?,
        archive_policy: row
            .try_get::<String, _>("archive_policy")
            .map_err(db_error)?
            .parse()?,
        archive_rendering_mode: row
            .try_get::<String, _>("archive_rendering_mode")
            .map_err(db_error)?
            .parse()?,
        style_name: row.try_get("style_name").map_err(db_error)?,
        default_member_action: optional_enum_column(row, "default_member_action")?,
        default_nonmember_action: optional_enum_column(row, "default_nonmember_action")?,
        administrivia: flag_column(row, "administrivia")?,
        require_explicit_destination: flag_column(row, "require_explicit_destination")?,
        acceptable_aliases: address_list_column(row, "acceptable_aliases")?,
        accept_these_nonmembers: address_list_column(row, "accept_these_nonmembers")?,
        hold_these_nonmembers: address_list_column(row, "hold_these_nonmembers")?,
        reject_these_nonmembers: address_list_column(row, "reject_these_nonmembers")?,
        discard_these_nonmembers: address_list_column(row, "discard_these_nonmembers")?,
        posting_pipeline: row.try_get("posting_pipeline").map_err(db_error)?,
        respond_to_post_requests: flag_column(row, "respond_to_post_requests")?,
        admin_immed_notify: flag_column(row, "admin_immed_notify")?,
        admin_notify_mchanges: flag_column(row, "admin_notify_mchanges")?,
        automatic_responses: automatic_responses_from_row(row)?,
        alter_messages: alter_messages_from_row(row)?,
        member_policy: listmngr_core::MemberPolicy {
            subscription_policy: enum_column(row, "subscription_policy")?,
            unsubscription_policy: enum_column(row, "unsubscription_policy")?,
            member_roster_visibility: enum_column(row, "member_roster_visibility")?,
        },
        forward_unrecognized_bounces_to: enum_column(row, "forward_unrecognized_bounces_to")?,
        topics_enabled: flag_column(row, "topics_enabled")?,
        topics_bodylines_limit: topics_limit_column(row)?,
        topics: topics_column(row)?,
    })
}

fn optional_enum_column<T: std::str::FromStr<Err = Error>>(
    row: &sqlx::any::AnyRow,
    column: &str,
) -> Result<Option<T>> {
    row.try_get::<Option<String>, _>(column)
        .map_err(db_error)?
        .map(|value| value.parse())
        .transpose()
}

fn topics_limit_column(row: &sqlx::any::AnyRow) -> Result<i32> {
    row.try_get::<i64, _>("topics_bodylines_limit")
        .map_err(db_error)?
        .try_into()
        .map_err(|_| Error::Validation("corrupt topics_bodylines_limit".into()))
}

fn topics_column(row: &sqlx::any::AnyRow) -> Result<Vec<listmngr_core::Topic>> {
    let text: String = row.try_get("topics").map_err(db_error)?;
    serde_json::from_str(&text).map_err(|_| Error::Validation("corrupt topics".into()))
}

fn alter_messages_from_row(row: &sqlx::any::AnyRow) -> Result<listmngr_core::AlterMessages> {
    Ok(listmngr_core::AlterMessages {
        filter_content: flag_column(row, "filter_content")?,
        filter_types: address_list_column(row, "filter_types")?,
        pass_types: address_list_column(row, "pass_types")?,
        filter_extensions: address_list_column(row, "filter_extensions")?,
        pass_extensions: address_list_column(row, "pass_extensions")?,
        collapse_alternatives: flag_column(row, "collapse_alternatives")?,
        convert_html_to_plaintext: flag_column(row, "convert_html_to_plaintext")?,
        filter_action: enum_column(row, "filter_action")?,
        include_rfc2369_headers: flag_column(row, "include_rfc2369_headers")?,
        allow_list_posts: flag_column(row, "allow_list_posts")?,
        reply_goes_to_list: enum_column(row, "reply_goes_to_list")?,
        reply_to_address: row.try_get("reply_to_address").map_err(db_error)?,
        first_strip_reply_to: flag_column(row, "first_strip_reply_to")?,
        personalize: enum_column(row, "personalize")?,
        include_sender_header: flag_column(row, "include_sender_header")?,
    })
}

/// A TEXT column holding one of a `string_enum`'s wire values.
fn enum_column<T: std::str::FromStr<Err = Error>>(
    row: &sqlx::any::AnyRow,
    column: &str,
) -> Result<T> {
    row.try_get::<String, _>(column)
        .map_err(db_error)?
        .parse()
        .map_err(|_| Error::Validation(format!("corrupt {column}")))
}

fn flag_column(row: &sqlx::any::AnyRow, column: &str) -> Result<bool> {
    Ok(row.try_get::<i64, _>(column).map_err(db_error)? != 0)
}

fn bounce_flags_from_row(row: &sqlx::any::AnyRow) -> Result<(bool, bool, bool, bool)> {
    let flag = |column: &str| -> Result<bool> {
        Ok(row.try_get::<i64, _>(column).map_err(db_error)? != 0)
    };
    Ok((
        flag("process_bounces")?,
        flag("bounce_notify_owner_on_disable")?,
        flag("bounce_notify_owner_on_bounce_increment")?,
        flag("bounce_notify_owner_on_removal")?,
    ))
}

fn dmarc_from_row(row: &sqlx::any::AnyRow) -> Result<listmngr_core::DmarcSettings> {
    Ok(listmngr_core::DmarcSettings {
        action: serde_json::from_value(serde_json::Value::String(
            row.try_get("dmarc_mitigate_action").map_err(db_error)?,
        ))
        .map_err(db_error)?,
        unconditional: row
            .try_get::<i64, _>("dmarc_mitigate_unconditionally")
            .map_err(db_error)?
            != 0,
        dmarc_addresses: address_list_column(row, "dmarc_addresses")?,
        dmarc_moderation_notice: row.try_get("dmarc_moderation_notice").map_err(db_error)?,
        dmarc_wrapped_message_text: row
            .try_get("dmarc_wrapped_message_text")
            .map_err(db_error)?,
    })
}

/// A JSON-array column of exact addresses or `^`-anchored regexes.
fn address_list_column(row: &sqlx::any::AnyRow, column: &str) -> Result<Vec<String>> {
    let text: String = row.try_get(column).map_err(db_error)?;
    serde_json::from_str(&text).map_err(|_| Error::Validation(format!("corrupt {column}")))
}

/// Longest single entry and longest list accepted for an address list.
const ADDRESS_LIST_ENTRY_BYTES: usize = 1024;
const ADDRESS_LIST_MAX_ENTRIES: usize = 10_000;
/// Longest free-text setting (DMARC notice and wrapper text).
const SETTING_TEXT_BYTES: usize = 65_536;
/// Longest MIME type or extension token and longest token list.
const TOKEN_BYTES: usize = 255;
const TOKEN_LIST_MAX_ENTRIES: usize = 1000;

const TOPIC_MAX_ENTRIES: usize = 1000;
const TOPIC_NAME_BYTES: usize = 64;
const TOPIC_PATTERN_BYTES: usize = 4096;
const TOPIC_DESCRIPTION_BYTES: usize = 1024;

/// A JSON array of `{name, pattern, description}` topics: unique
/// single-line names, patterns whose every line compiles as the matcher
/// will run it, bounded description text.
fn parse_topics(value: &serde_json::Value) -> Result<Vec<listmngr_core::Topic>> {
    let invalid = |detail: &str| Error::Validation(format!("topics: {detail}"));
    let topics: Vec<listmngr_core::Topic> =
        serde_json::from_value(value.clone()).map_err(|_| invalid("expected a list of topics"))?;
    if topics.len() > TOPIC_MAX_ENTRIES {
        return Err(invalid("too many topics"));
    }
    let mut names = std::collections::BTreeSet::new();
    for topic in &topics {
        let name = topic.name.trim();
        if name.is_empty()
            || name.len() > TOPIC_NAME_BYTES
            || name.chars().any(char::is_control)
            || !names.insert(name.to_ascii_lowercase())
        {
            return Err(invalid(
                "topic names must be unique, single-line and at most 64 bytes",
            ));
        }
        if topic.pattern.trim().is_empty() || topic.pattern.len() > TOPIC_PATTERN_BYTES {
            return Err(invalid(
                "topic patterns must be non-empty and at most 4096 bytes",
            ));
        }
        listmngr_pipeline::topics::compile_topic_pattern(&topic.pattern)
            .map_err(|error| invalid(&format!("topic {name}: {error}")))?;
        if topic.description.len() > TOPIC_DESCRIPTION_BYTES
            || topic.description.contains(['\r', '\n'])
        {
            return Err(invalid(
                "topic descriptions must be single-line and at most 1024 bytes",
            ));
        }
    }
    Ok(topics)
}

/// One of a `string_enum`'s wire values, as a JSON string.
fn parse_enum<T: std::str::FromStr<Err = Error>>(
    key: &str,
    value: &serde_json::Value,
) -> Result<T> {
    value
        .as_str()
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| Error::Validation(key.to_owned()))
}

/// A JSON array of MIME types (`type` or `type/subtype`) or of file-name
/// extensions: printable ASCII tokens without whitespace, stored lowercase
/// as Mailman compares them.
fn parse_token_list(key: &str, value: &serde_json::Value, mime: bool) -> Result<Vec<String>> {
    let invalid = || Error::Validation(key.to_owned());
    let entries = value.as_array().ok_or_else(invalid)?;
    if entries.len() > TOKEN_LIST_MAX_ENTRIES {
        return Err(invalid());
    }
    entries
        .iter()
        .map(|entry| {
            let text = entry.as_str().ok_or_else(invalid)?;
            let ok = !text.is_empty()
                && text.len() <= TOKEN_BYTES
                && text
                    .bytes()
                    .all(|byte| byte.is_ascii_graphic() && byte != b'"' && byte != b'\\')
                && if mime {
                    text.matches('/').count() <= 1 && !text.starts_with('/') && !text.ends_with('/')
                } else {
                    !text.contains(['/', '.'])
                };
            ok.then(|| text.to_ascii_lowercase()).ok_or_else(invalid)
        })
        .collect()
}

/// Validate one address-list setting: a JSON array of non-empty single-line
/// strings, each either an address or a `^`-anchored regex that compiles.
fn parse_address_list(key: &str, value: &serde_json::Value) -> Result<Vec<String>> {
    let invalid = || Error::Validation(key.to_owned());
    let entries = value.as_array().ok_or_else(invalid)?;
    if entries.len() > ADDRESS_LIST_MAX_ENTRIES {
        return Err(invalid());
    }
    entries
        .iter()
        .map(|entry| {
            let text = entry.as_str().ok_or_else(invalid)?.trim();
            if text.is_empty()
                || text.len() > ADDRESS_LIST_ENTRY_BYTES
                || text.chars().any(|c| c.is_control() || c.is_whitespace())
            {
                return Err(invalid());
            }
            if let Some(pattern) = text.strip_prefix('^') {
                listmngr_pipeline::compile_header_pattern(&format!("^{pattern}"))
                    .map_err(|_| invalid())?;
            }
            Ok(text.to_owned())
        })
        .collect()
}

type ExistingRoleMembers = HashMap<String, (String, String)>;

fn validate_mass_input(operation: &str, emails: &[String]) -> Result<Vec<Address>> {
    if !matches!(operation, "subscribe" | "unsubscribe" | "sync") {
        return Err(Error::Validation("invalid mass operation".into()));
    }
    let addresses = emails
        .iter()
        .map(|email| Address::new(email, String::new()))
        .collect::<Result<Vec<_>>>()?;
    let unique = addresses
        .iter()
        .map(|address| address.email.as_str())
        .collect::<HashSet<_>>();
    if unique.len() != addresses.len() {
        return Err(Error::Validation("duplicate mass member row".into()));
    }
    Ok(addresses)
}

async fn load_role_members(
    tx: &mut Transaction<'_, Any>,
    list: &ListId,
    role: MemberRole,
) -> Result<ExistingRoleMembers> {
    let rows = sqlx::query("SELECT m.id,m.preferences_id,a.email FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id=$1 AND m.role=$2 ORDER BY a.email")
        .bind(list.as_str())
        .bind(role.to_string())
        .fetch_all(&mut **tx)
        .await
        .map_err(db_error)?;
    rows.iter()
        .map(|row| {
            Ok((
                row.try_get::<String, _>("email").map_err(db_error)?,
                (
                    row.try_get::<String, _>("id").map_err(db_error)?,
                    row.try_get::<String, _>("preferences_id")
                        .map_err(db_error)?,
                ),
            ))
        })
        .collect()
}

fn preflight_mass_operation(
    operation: &str,
    addresses: &[Address],
    existing: &ExistingRoleMembers,
) -> Result<()> {
    if operation == "subscribe" {
        if let Some(address) = addresses
            .iter()
            .find(|address| existing.contains_key(&address.email))
        {
            return Err(Error::Conflict(address.email.clone()));
        }
    } else if operation == "unsubscribe" {
        if let Some(address) = addresses
            .iter()
            .find(|address| !existing.contains_key(&address.email))
        {
            return Err(Error::NotFound(address.email.clone()));
        }
    }
    Ok(())
}

fn mass_removals(
    operation: &str,
    addresses: &[Address],
    existing: &ExistingRoleMembers,
) -> Vec<(String, String)> {
    let desired = addresses
        .iter()
        .map(|address| address.email.as_str())
        .collect::<HashSet<_>>();
    match operation {
        "sync" => existing
            .iter()
            .filter(|(email, _)| !desired.contains(email.as_str()))
            .map(|(_, value)| value.clone())
            .collect(),
        "unsubscribe" => addresses
            .iter()
            .filter_map(|address| existing.get(&address.email).cloned())
            .collect(),
        _ => Vec::new(),
    }
}

async fn delete_mass_members(
    tx: &mut Transaction<'_, Any>,
    db: &Database,
    removed: &[(String, String)],
) -> Result<()> {
    for (member_id, preferences_id) in removed {
        workflows::delete_member_with_goodbye(tx, db, member_id).await?;
        sqlx::query("DELETE FROM preferences WHERE id=$1")
            .bind(preferences_id)
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
    }
    Ok(())
}

fn mass_additions<'a>(
    operation: &str,
    addresses: &'a [Address],
    existing: &ExistingRoleMembers,
) -> Vec<&'a Address> {
    match operation {
        "subscribe" => addresses.iter().collect(),
        "sync" => addresses
            .iter()
            .filter(|address| !existing.contains_key(&address.email))
            .collect(),
        _ => Vec::new(),
    }
}

async fn insert_mass_members(
    tx: &mut Transaction<'_, Any>,
    db: &Database,
    list: &ListId,
    role: MemberRole,
    mode_for_new: SubscriptionMode,
    additions: &[&Address],
) -> Result<()> {
    for address in additions {
        let row = sqlx::query("SELECT id,user_id FROM addresses WHERE email=$1")
            .bind(&address.email)
            .fetch_optional(&mut **tx)
            .await
            .map_err(db_error)?;
        let (address_id, user_id) = if let Some(row) = row {
            (
                row.try_get::<String, _>("id").map_err(db_error)?,
                row.try_get::<Option<String>, _>("user_id")
                    .map_err(db_error)?,
            )
        } else {
            sqlx::query("INSERT INTO addresses(id,email,original_email,display_name,registered_on) VALUES($1,$2,$3,$4,$5)")
                .bind(address.id.to_string()).bind(&address.email).bind(&address.original_email)
                .bind(&address.display_name).bind(address.registered_on.to_rfc3339())
                .execute(&mut **tx).await.map_err(db_error)?;
            (address.id.to_string(), None)
        };
        let member_id = MemberId::new();
        let preferences_id = PreferencesId::new();
        sqlx::query("INSERT INTO preferences(id) VALUES($1)")
            .bind(preferences_id.to_string())
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
        sqlx::query("INSERT INTO members(id,list_id,role,address_id,user_id,subscription_mode,display_name,preferences_id,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)")
            .bind(member_id.to_string()).bind(list.as_str()).bind(role.to_string())
            .bind(address_id).bind(user_id).bind(mode_for_new.to_string())
            // Mass operations build addresses without one; a workflow
            // carries the display name the operator supplied.
            .bind(&address.display_name).bind(preferences_id.to_string()).bind(now())
            .execute(&mut **tx).await.map_err(db_error)?;
        workflows::welcome_new_member(tx, db, member_id).await?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub struct MemberRepo<'a> {
    db: &'a Database,
}
impl MemberRepo<'_> {
    /// # Errors
    ///
    /// Returns an error for invalid member data, missing/conflicting records, or database/audit transaction failure.
    pub async fn mass(&self, list: &ListId, operation: &str, emails: &[String]) -> Result<usize> {
        Ok(self
            .mass_for_role(
                list,
                operation,
                emails,
                MemberRole::Member,
                SubscriptionMode::AsAddress,
            )
            .await?
            .processed())
    }
    /// Applies a bulk member operation attributed to the supplied audit context.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid input, missing/conflicting records, or database/audit transaction failure.
    pub async fn mass_with_context(
        &self,
        list: &ListId,
        operation: &str,
        emails: &[String],
        context: &AuditContext,
    ) -> Result<usize> {
        Ok(self
            .mass_for_role_with_context(
                list,
                operation,
                emails,
                MemberRole::Member,
                SubscriptionMode::AsAddress,
                context,
            )
            .await?
            .processed())
    }

    /// Applies a mass operation to exactly one role. Existing memberships retained
    /// by `sync` are not rewritten, preserving their role, mode, and metadata.
    /// All input is normalized and validated before the transaction starts.
    ///
    /// # Errors
    ///
    /// Returns a deterministic validation/conflict/not-found error without writes.
    pub async fn mass_for_role(
        &self,
        list: &ListId,
        operation: &str,
        emails: &[String],
        role: MemberRole,
        mode_for_new: SubscriptionMode,
    ) -> Result<MemberMassResult> {
        self.mass_for_role_with_context(
            list,
            operation,
            emails,
            role,
            mode_for_new,
            &AuditContext::system(),
        )
        .await
    }

    /// Context-attributed variant of [`Self::mass_for_role`].
    ///
    /// # Errors
    ///
    /// Returns a deterministic validation/conflict/not-found error, or a
    /// database/audit error. All writes and the one audit event share a transaction.
    pub async fn mass_for_role_with_context(
        &self,
        list: &ListId,
        operation: &str,
        emails: &[String],
        role: MemberRole,
        mode_for_new: SubscriptionMode,
        context: &AuditContext,
    ) -> Result<MemberMassResult> {
        self.db.lists().get(list).await?;
        let addresses = validate_mass_input(operation, emails)?;
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        let existing = load_role_members(&mut tx, list, role).await?;
        preflight_mass_operation(operation, &addresses, &existing)?;

        let removed = mass_removals(operation, &addresses, &existing);
        delete_mass_members(&mut tx, self.db, &removed).await?;
        let additions = mass_additions(operation, &addresses, &existing);
        insert_mass_members(&mut tx, self.db, list, role, mode_for_new, &additions).await?;
        let result = MemberMassResult {
            added: additions.len(),
            removed: removed.len(),
            retained: if operation == "sync" {
                addresses.len() - additions.len()
            } else {
                0
            },
        };
        Database::record_tx_with_context(
            &mut tx,
            context,
            "member.mass",
            "list",
            list.as_str(),
            serde_json::json!({
                "operation":operation,
                "role":role,
                "mode_for_new":mode_for_new,
                "added":result.added,
                "removed":result.removed,
                "retained":result.retained
            }),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(result)
    }

    /// # Errors
    ///
    /// Returns an error for invalid member data, missing/conflicting records, or database/audit transaction failure.
    pub async fn create(&self, new: NewMember) -> Result<Member> {
        self.subscribe_with_context(new, false, &AuditContext::system())
            .await
    }

    /// Atomically creates the optional address and preferences, optionally
    /// verifies the address, creates the member, and emits one audit event.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid member data, missing/conflicting records, or database/audit transaction failure.
    pub async fn subscribe_with_context(
        &self,
        new: NewMember,
        verified: bool,
        context: &AuditContext,
    ) -> Result<Member> {
        self.db.lists().get(&new.list_id).await?;
        let (address, insert_address) = match self.db.addresses().get(&new.email).await {
            Ok(value) => (value, false),
            Err(Error::NotFound(_)) => (Address::new(&new.email, new.display_name.clone())?, true),
            Err(error) => return Err(error),
        };
        let pref = PreferencesId::new();
        let member = Member {
            id: MemberId::new(),
            list_id: new.list_id,
            role: new.role,
            address_id: address.id,
            user_id: address.user_id,
            subscription_mode: new.subscription_mode,
            moderation_action: None,
            display_name: new.display_name,
            preferences_id: pref,
            bounce_score: 0.0,
            last_bounce_received: None,
            created_at: Utc::now(),
        };
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        if insert_address {
            sqlx::query("INSERT INTO addresses(id,email,original_email,display_name,registered_on) VALUES($1,$2,$3,$4,$5)").bind(address.id.to_string()).bind(&address.email).bind(&address.original_email).bind(&address.display_name).bind(address.registered_on.to_rfc3339()).execute(&mut *tx).await.map_err(db_error)?;
        }
        if verified {
            sqlx::query("UPDATE addresses SET verified_on=$1 WHERE id=$2")
                .bind(now())
                .bind(address.id.to_string())
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
        }
        sqlx::query("INSERT INTO preferences(id) VALUES($1)")
            .bind(pref.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        sqlx::query("INSERT INTO members(id,list_id,role,address_id,user_id,subscription_mode,display_name,preferences_id,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)").bind(member.id.to_string()).bind(member.list_id.as_str()).bind(member.role.to_string()).bind(member.address_id.to_string()).bind(member.user_id.map(|v|v.to_string())).bind(member.subscription_mode.to_string()).bind(&member.display_name).bind(pref.to_string()).bind(member.created_at.to_rfc3339()).execute(&mut *tx).await.map_err(db_error)?;
        Database::record_tx_with_context(
            &mut tx,
            context,
            "member.create",
            "member",
            &member.id.to_string(),
            serde_json::json!({"list_id":member.list_id,"role":member.role,"verified":verified}),
        )
        .await?;
        workflows::welcome_new_member(&mut tx, self.db, member.id).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(member)
    }
    /// # Errors
    ///
    /// Returns an error for invalid member data, missing/conflicting records, or database/audit transaction failure.
    pub async fn get(&self, id: MemberId) -> Result<Member> {
        let row = sqlx::query("SELECT * FROM members WHERE id=$1")
            .bind(id.to_string())
            .fetch_optional(&self.db.pool)
            .await
            .map_err(db_error)?
            .ok_or_else(|| Error::NotFound(id.to_string()))?;
        member_from_row(&row)
    }
    /// # Errors
    ///
    /// Returns an error for invalid member data, missing/conflicting records, or database/audit transaction failure.
    pub async fn roster(&self, id: &ListId, role: MemberRole) -> Result<Vec<Member>> {
        let rows =
            sqlx::query("SELECT m.* FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id=$1 AND m.role=$2 ORDER BY a.email,m.id")
                .bind(id.as_str())
                .bind(role.to_string())
                .fetch_all(&self.db.pool)
                .await
                .map_err(db_error)?;
        rows.iter().map(member_from_row).collect()
    }
    /// # Errors
    ///
    /// Returns an error for invalid member data, missing/conflicting records, or database/audit transaction failure.
    pub async fn find(&self, email: &str) -> Result<Vec<Member>> {
        let rows=sqlx::query("SELECT m.* FROM members m JOIN addresses a ON a.id=m.address_id WHERE a.email=$1 ORDER BY m.list_id,m.role,m.id").bind(email.to_ascii_lowercase()).fetch_all(&self.db.pool).await.map_err(db_error)?;
        rows.iter().map(member_from_row).collect()
    }
    /// Finds members whose normalized address contains a literal substring.
    ///
    /// # Errors
    ///
    /// Returns an error when memberships cannot be queried or decoded.
    pub async fn find_substring(&self, substring: &str) -> Result<Vec<Member>> {
        let escaped = substring
            .to_ascii_lowercase()
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let rows = sqlx::query("SELECT m.* FROM members m JOIN addresses a ON a.id=m.address_id WHERE a.email LIKE $1 ESCAPE '\\' ORDER BY m.list_id,m.role,m.id")
            .bind(format!("%{escaped}%"))
            .fetch_all(&self.db.pool)
            .await
            .map_err(db_error)?;
        rows.iter().map(member_from_row).collect()
    }
    /// # Errors
    ///
    /// Returns an error for invalid member data, missing/conflicting records, or database/audit transaction failure.
    pub async fn update(&self, id: MemberId, patch: &serde_json::Value) -> Result<Member> {
        self.update_with_context(id, patch, &AuditContext::system())
            .await
    }
    /// Updates a member attributed to the supplied audit context.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid fields, a missing member, or database/audit transaction failure.
    pub async fn update_with_context(
        &self,
        id: MemberId,
        patch: &serde_json::Value,
        context: &AuditContext,
    ) -> Result<Member> {
        let current = self.get(id).await?;
        let object = patch
            .as_object()
            .ok_or_else(|| Error::Validation("member patch must be an object".into()))?;
        if object.keys().any(|key| {
            !matches!(
                key.as_str(),
                "display_name" | "role" | "subscription_mode" | "delivery_mode" | "delivery_status"
            )
        }) {
            return Err(Error::Validation(
                "read-only or unknown member field".into(),
            ));
        }
        let display_name = object.get("display_name").map_or_else(
            || Ok(current.display_name.clone()),
            |v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| Error::Validation("display_name".into()))
            },
        )?;
        let role = object
            .get("role")
            .map(|v| {
                v.as_str()
                    .ok_or_else(|| Error::Validation("role".into()))?
                    .parse()
            })
            .transpose()?
            .unwrap_or(current.role);
        let mode = object
            .get("subscription_mode")
            .map(|v| {
                v.as_str()
                    .ok_or_else(|| Error::Validation("subscription_mode".into()))?
                    .parse()
            })
            .transpose()?
            .unwrap_or(current.subscription_mode);
        // PATCH omission must not replay a pre-transaction preference snapshot.
        let mut preferences = Preferences::default();
        if let Some(value) = object.get("delivery_mode") {
            preferences.delivery_mode = Some(
                value
                    .as_str()
                    .ok_or_else(|| Error::Validation("delivery_mode".into()))?
                    .parse()?,
            );
        }
        if let Some(value) = object.get("delivery_status") {
            preferences.delivery_status = Some(
                value
                    .as_str()
                    .ok_or_else(|| Error::Validation("delivery_status".into()))?
                    .parse()?,
            );
        }
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        sqlx::query("UPDATE members SET display_name=$1,role=$2,subscription_mode=$3 WHERE id=$4")
            .bind(display_name)
            .bind(role.to_string())
            .bind(mode.to_string())
            .bind(id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        if preferences.delivery_mode.is_some() || preferences.delivery_status.is_some() {
            // Member UPDATE above is the shared scorer boundary. Only requested
            // fields are changed, using the current row after any preference wait.
            sqlx::query("UPDATE preferences SET delivery_mode=COALESCE($1,delivery_mode),delivery_status=COALESCE($2,delivery_status),delivery_generation=delivery_generation+1 WHERE id=$3")
                .bind(preferences.delivery_mode.map(|v| v.to_string()))
                .bind(preferences.delivery_status.map(|v| v.to_string()))
                .bind(current.preferences_id.to_string())
                .execute(&mut *tx).await.map_err(db_error)?;
        }
        Database::record_tx_with_context(
            &mut tx,
            context,
            "member.update",
            "member",
            &id.to_string(),
            patch.clone(),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        self.get(id).await
    }
    /// # Errors
    ///
    /// Returns an error for invalid member data, missing/conflicting records, or database/audit transaction failure.
    pub async fn delete(&self, id: MemberId) -> Result<()> {
        self.delete_with_context(id, &AuditContext::system()).await
    }
    /// Deletes a member attributed to the supplied audit context.
    ///
    /// # Errors
    ///
    /// Returns an error if the member is missing or the database/audit transaction fails.
    pub async fn delete_with_context(&self, id: MemberId, context: &AuditContext) -> Result<()> {
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        let preferences_id: Option<String> =
            sqlx::query_scalar("SELECT preferences_id FROM members WHERE id=$1")
                .bind(id.to_string())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_error)?;
        let Some(preferences_id) = preferences_id else {
            return Err(Error::NotFound(id.to_string()));
        };
        if !workflows::delete_member_with_goodbye(&mut tx, self.db, &id.to_string()).await? {
            return Err(Error::NotFound(id.to_string()));
        }
        sqlx::query("DELETE FROM preferences WHERE id=$1")
            .bind(preferences_id)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        Database::record_tx_with_context(
            &mut tx,
            context,
            "member.delete",
            "member",
            &id.to_string(),
            serde_json::json!({}),
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }
}
fn member_from_row(row: &sqlx::any::AnyRow) -> Result<Member> {
    Ok(Member {
        bounce_score: row.try_get("bounce_score").map_err(db_error)?,
        last_bounce_received: row
            .try_get::<Option<String>, _>("last_bounce_received")
            .map_err(db_error)?
            .map(|v| parse_time(&v))
            .transpose()?,
        id: parse_uuid(row.try_get::<String, _>("id").map_err(db_error)?.as_str())?,
        list_id: row
            .try_get::<String, _>("list_id")
            .map_err(db_error)?
            .parse()?,
        role: row
            .try_get::<String, _>("role")
            .map_err(db_error)?
            .parse()?,
        address_id: parse_uuid(
            row.try_get::<String, _>("address_id")
                .map_err(db_error)?
                .as_str(),
        )?,
        user_id: row
            .try_get::<Option<String>, _>("user_id")
            .map_err(db_error)?
            .map(|v| parse_uuid(&v))
            .transpose()?,
        subscription_mode: row
            .try_get::<String, _>("subscription_mode")
            .map_err(db_error)?
            .parse()?,
        moderation_action: row
            .try_get::<Option<String>, _>("moderation_action")
            .map_err(db_error)?
            .map(|v| v.parse())
            .transpose()?,
        display_name: row.try_get("display_name").map_err(db_error)?,
        preferences_id: parse_uuid(
            row.try_get::<String, _>("preferences_id")
                .map_err(db_error)?
                .as_str(),
        )?,
        created_at: parse_time(&row.try_get::<String, _>("created_at").map_err(db_error)?)?,
    })
}

#[derive(Debug, Clone, Copy)]
pub struct PreferencesRepo<'a> {
    db: &'a Database,
}
impl PreferencesRepo<'_> {
    pub(crate) async fn set_tx(
        tx: &mut Transaction<'_, Any>,
        id: PreferencesId,
        p: &Preferences,
    ) -> Result<()> {
        sqlx::query("UPDATE preferences SET acknowledge_posts=$1,hide_address=$2,preferred_language=$3,receive_list_copy=$4,receive_own_postings=$5,delivery_mode=$6,delivery_status=$7,delivery_generation=delivery_generation+1 WHERE id=$8").bind(p.acknowledge_posts.map(i64::from)).bind(p.hide_address.map(i64::from)).bind(&p.preferred_language).bind(p.receive_list_copy.map(i64::from)).bind(p.receive_own_postings.map(i64::from)).bind(p.delivery_mode.map(|v|v.to_string())).bind(p.delivery_status.map(|v|v.to_string())).bind(id.to_string()).execute(&mut **tx).await.map_err(db_error)?;
        Ok(())
    }
    async fn set(&self, id: PreferencesId, p: &Preferences, context: &AuditContext) -> Result<()> {
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        Self::set_tx(&mut tx, id, p).await?;
        Database::record_tx_with_context(
            &mut tx,
            context,
            "preferences.update",
            "preferences",
            &id.to_string(),
            serde_json::to_value(p).map_err(db_error)?,
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }
    /// # Errors
    ///
    /// Returns an error when the preference owner is missing, values cannot be decoded, or a database/audit operation fails.
    pub async fn set_user(&self, id: UserId, p: Preferences) -> Result<()> {
        self.set_user_with_context(id, p, &AuditContext::system())
            .await
    }
    /// Sets user preferences attributed to the supplied audit context.
    ///
    /// # Errors
    ///
    /// Returns an error if the user is missing, values cannot be encoded, or the database/audit transaction fails.
    pub async fn set_user_with_context(
        &self,
        id: UserId,
        p: Preferences,
        context: &AuditContext,
    ) -> Result<()> {
        let row = sqlx::query("SELECT preferences_id FROM users WHERE id=$1")
            .bind(id.to_string())
            .fetch_one(&self.db.pool)
            .await
            .map_err(db_error)?;
        self.set(
            parse_uuid(
                row.try_get::<String, _>("preferences_id")
                    .map_err(db_error)?
                    .as_str(),
            )?,
            &p,
            context,
        )
        .await
    }
    /// # Errors
    ///
    /// Returns an error when the preference owner is missing, values cannot be decoded, or a database/audit operation fails.
    pub async fn get_user(&self, id: UserId) -> Result<Preferences> {
        let preference_id: String =
            sqlx::query_scalar("SELECT preferences_id FROM users WHERE id=$1")
                .bind(id.to_string())
                .fetch_optional(&self.db.pool)
                .await
                .map_err(db_error)?
                .ok_or_else(|| Error::NotFound(id.to_string()))?;
        self.get(parse_uuid(&preference_id)?).await
    }
    /// # Errors
    ///
    /// Returns an error when the preference owner is missing, values cannot be decoded, or a database/audit operation fails.
    pub async fn resolve_user(&self, id: UserId, language: &str) -> Result<Preferences> {
        let direct = self.get_user(id).await?;
        Ok(Preferences::resolve([
            &Preferences::system_defaults(language.to_owned()),
            &direct,
        ]))
    }
    /// # Errors
    ///
    /// Returns an error when the preference owner is missing, values cannot be decoded, or a database/audit operation fails.
    pub async fn set_member(&self, id: MemberId, p: Preferences) -> Result<()> {
        self.set_member_with_context(id, p, &AuditContext::system())
            .await
    }
    /// Sets member preferences attributed to the supplied audit context.
    ///
    /// # Errors
    ///
    /// Returns an error if the member is missing, values cannot be encoded, or the database/audit transaction fails.
    pub async fn set_member_with_context(
        &self,
        id: MemberId,
        p: Preferences,
        context: &AuditContext,
    ) -> Result<()> {
        let m = self.db.members().get(id).await?;
        self.set(m.preferences_id, &p, context).await
    }
    /// # Errors
    ///
    /// Returns an error when the preference owner is missing, values cannot be decoded, or a database/audit operation fails.
    pub async fn set_address(&self, email: &str, p: Preferences) -> Result<()> {
        self.set_address_with_context(email, p, &AuditContext::system())
            .await
    }
    /// Sets address preferences attributed to the supplied audit context.
    ///
    /// # Errors
    ///
    /// Returns an error if the address is missing, values cannot be encoded, or the database/audit transaction fails.
    pub async fn set_address_with_context(
        &self,
        email: &str,
        p: Preferences,
        context: &AuditContext,
    ) -> Result<()> {
        let address = self.db.addresses().get(email).await?;
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        let existing: Option<String> =
            sqlx::query_scalar("UPDATE addresses SET preferences_id=preferences_id WHERE id=$1 RETURNING preferences_id")
                .bind(address.id.to_string())
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?;
        let preference_id = if let Some(id) = existing {
            parse_uuid(&id)?
        } else {
            let id = PreferencesId::new();
            sqlx::query("INSERT INTO preferences(id) VALUES($1)")
                .bind(id.to_string())
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
            sqlx::query("UPDATE addresses SET preferences_id=$1 WHERE id=$2")
                .bind(id.to_string())
                .bind(address.id.to_string())
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
            id
        };
        Self::set_tx(&mut tx, preference_id, &p).await?;
        Database::record_tx_with_context(
            &mut tx,
            context,
            "preferences.update",
            "address",
            email,
            serde_json::to_value(&p).map_err(db_error)?,
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }
    /// # Errors
    ///
    /// Returns an error when the preference owner is missing, values cannot be decoded, or a database/audit operation fails.
    pub async fn get_address(&self, email: &str) -> Result<Preferences> {
        let address = self.db.addresses().get(email).await?;
        let id: Option<String> =
            sqlx::query_scalar("SELECT preferences_id FROM addresses WHERE id=$1")
                .bind(address.id.to_string())
                .fetch_one(&self.db.pool)
                .await
                .map_err(db_error)?;
        match id {
            Some(id) => self.get(parse_uuid(&id)?).await,
            None => Ok(Preferences::default()),
        }
    }
    /// # Errors
    ///
    /// Returns an error when the preference owner is missing, values cannot be decoded, or a database/audit operation fails.
    pub async fn resolve_address(&self, email: &str, language: &str) -> Result<Preferences> {
        let address = self.db.addresses().get(email).await?;
        let system = Preferences::system_defaults(language.to_owned());
        let user = match address.user_id {
            Some(id) => self.get_user(id).await?,
            None => Preferences::default(),
        };
        let direct = self.get_address(email).await?;
        Ok(Preferences::resolve([&system, &user, &direct]))
    }
    /// # Errors
    ///
    /// Returns an error when the preference owner is missing, values cannot be decoded, or a database/audit operation fails.
    pub async fn get(&self, id: PreferencesId) -> Result<Preferences> {
        let row = sqlx::query("SELECT * FROM preferences WHERE id=$1")
            .bind(id.to_string())
            .fetch_one(&self.db.pool)
            .await
            .map_err(db_error)?;
        Ok(Preferences {
            acknowledge_posts: row
                .try_get::<Option<i64>, _>("acknowledge_posts")
                .map_err(db_error)?
                .map(|v| v != 0),
            hide_address: row
                .try_get::<Option<i64>, _>("hide_address")
                .map_err(db_error)?
                .map(|v| v != 0),
            preferred_language: row.try_get("preferred_language").map_err(db_error)?,
            receive_list_copy: row
                .try_get::<Option<i64>, _>("receive_list_copy")
                .map_err(db_error)?
                .map(|v| v != 0),
            receive_own_postings: row
                .try_get::<Option<i64>, _>("receive_own_postings")
                .map_err(db_error)?
                .map(|v| v != 0),
            delivery_mode: row
                .try_get::<Option<String>, _>("delivery_mode")
                .map_err(db_error)?
                .map(|v| v.parse())
                .transpose()?,
            delivery_status: row
                .try_get::<Option<String>, _>("delivery_status")
                .map_err(db_error)?
                .map(|v| v.parse())
                .transpose()?,
        })
    }
    /// # Errors
    ///
    /// Returns an error when the preference owner is missing, values cannot be decoded, or a database/audit operation fails.
    pub async fn resolve_member(&self, id: MemberId, language: &str) -> Result<Preferences> {
        let member = self.db.members().get(id).await?;
        let mut layers = vec![Preferences::system_defaults(language.into())];
        if let Some(user_id) = member.user_id {
            layers.push(self.get_user(user_id).await?);
        }
        let address_preference: Option<String> =
            sqlx::query_scalar("SELECT preferences_id FROM addresses WHERE id=$1")
                .bind(member.address_id.to_string())
                .fetch_one(&self.db.pool)
                .await
                .map_err(db_error)?;
        if let Some(value) = address_preference {
            layers.push(self.get(parse_uuid(&value)?).await?);
        }
        layers.push(self.get(member.preferences_id).await?);
        Ok(Preferences::resolve(layers.iter()))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TokenRepo<'a> {
    db: &'a Database,
}
/// The random secret, its digest, the row and the `token.create` audit event,
/// in the caller's transaction. Validation is the caller's.
pub(crate) async fn insert_token_tx(
    tx: &mut Transaction<'_, Any>,
    input: &NewToken<'_>,
    context: &AuditContext,
) -> Result<IssuedToken> {
    let id = TokenId::new();
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    let secret = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let token = format!("lm_{id}_{secret}");
    let hash = format!("{:x}", Sha256::digest(secret.as_bytes()));
    sqlx::query("INSERT INTO api_tokens(id,user_id,name,token_hash,scopes,list_id,domain_id,expires_at,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)")
        .bind(id.to_string()).bind(input.user.to_string()).bind(input.name).bind(hash).bind(input.scopes.join(" "))
        .bind(input.list_id.map(ToString::to_string)).bind(input.domain_id.map(|value| value.to_string()))
        .bind(input.expires.map(|value| value.to_rfc3339())).bind(now()).execute(&mut **tx).await.map_err(db_error)?;
    Database::record_tx_with_context(tx, context, "token.create", "token", &id.to_string(), serde_json::json!({"scopes":input.scopes,"list_id":input.list_id,"domain_id":input.domain_id})).await?;
    Ok(IssuedToken { id, token })
}

impl TokenRepo<'_> {
    /// # Errors
    ///
    /// Returns an error for invalid token input, authentication failure, missing records, or database/audit transaction failure.
    pub async fn create(
        &self,
        user: UserId,
        name: &str,
        scopes: &[&str],
        expires: Option<DateTime<Utc>>,
    ) -> Result<IssuedToken> {
        self.create_with_context(user, name, scopes, expires, &AuditContext::system())
            .await
    }
    /// Issues an unscoped token attributed to the supplied audit context.
    ///
    /// # Errors
    /// Returns an error for invalid scope or database/audit failure.
    pub async fn create_with_context(
        &self,
        user: UserId,
        name: &str,
        scopes: &[&str],
        expires: Option<DateTime<Utc>>,
        context: &AuditContext,
    ) -> Result<IssuedToken> {
        self.create_new_with_context(
            NewToken {
                user,
                name,
                scopes,
                list_id: None,
                domain_id: None,
                expires,
            },
            context,
        )
        .await
    }
    /// # Errors
    ///
    /// Returns an error for invalid token input, authentication failure, missing records, or database/audit transaction failure.
    pub async fn create_scoped(
        &self,
        user: UserId,
        name: &str,
        scopes: &[&str],
        list_id: Option<&ListId>,
        domain_id: Option<DomainId>,
        expires: Option<DateTime<Utc>>,
    ) -> Result<IssuedToken> {
        self.create_new_with_context(
            NewToken {
                user,
                name,
                scopes,
                list_id,
                domain_id,
                expires,
            },
            &AuditContext::system(),
        )
        .await
    }
    /// Issues a token attributed to the supplied audit context.
    ///
    /// # Errors
    /// Returns an error for invalid bounds, scope, or database/audit failure.
    pub async fn create_new_with_context(
        &self,
        input: NewToken<'_>,
        context: &AuditContext,
    ) -> Result<IssuedToken> {
        self.db.users().get(input.user).await?;
        let allowed = [
            "system:read",
            "lists:read",
            "lists:write",
            "members:read",
            "members:write",
            "moderation",
            "users:write",
            "archive:write",
            "admin",
        ];
        if input.scopes.contains(&"admin") && (input.list_id.is_some() || input.domain_id.is_some())
        {
            return Err(Error::Validation("admin tokens must be unbound".into()));
        }
        if input.scopes.iter().any(|scope| !allowed.contains(scope)) {
            return Err(Error::Validation("unknown token scope".into()));
        }
        if let Some(list) = input.list_id {
            let domain = self.db.domains().get(list.mail_host()).await?;
            self.db.lists().get(list).await?;
            if input.domain_id.is_some_and(|bound| bound != domain.id) {
                return Err(Error::Validation("list is outside token domain".into()));
            }
        }
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        let issued = insert_token_tx(&mut tx, &input, context).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(issued)
    }
    /// # Errors
    /// Returns an authentication or database error.
    pub async fn authenticate(&self, token: &str) -> Result<TokenAuth> {
        let auth = self.authenticate_without_usage(token).await?;
        self.mark_used(auth.id).await?;
        Ok(auth)
    }
    /// Validates a token without updating its usage timestamp.
    ///
    /// # Errors
    /// Returns an authentication or database error.
    pub async fn authenticate_without_usage(&self, token: &str) -> Result<TokenAuth> {
        let mut parts = token.splitn(3, '_');
        if parts.next() != Some("lm") {
            return Err(Error::Authentication);
        }
        let id: TokenId = parts
            .next()
            .ok_or(Error::Authentication)?
            .parse()
            .map_err(|_| Error::Authentication)?;
        let secret = parts
            .next()
            .filter(|value| !value.is_empty())
            .ok_or(Error::Authentication)?;
        let hash = format!("{:x}", Sha256::digest(secret.as_bytes()));
        let row=sqlx::query("SELECT user_id,token_hash,scopes,list_id,domain_id,expires_at,revoked_at FROM api_tokens WHERE id=$1").bind(id.to_string()).fetch_optional(&self.db.pool).await.map_err(db_error)?.ok_or(Error::Authentication)?;
        let stored: String = row.try_get("token_hash").map_err(db_error)?;
        if !bool::from(stored.as_bytes().ct_eq(hash.as_bytes()))
            || row
                .try_get::<Option<String>, _>("revoked_at")
                .map_err(db_error)?
                .is_some()
        {
            return Err(Error::Authentication);
        }
        if let Some(expiry) = row
            .try_get::<Option<String>, _>("expires_at")
            .map_err(db_error)?
        {
            if parse_time(&expiry)? <= Utc::now() {
                return Err(Error::Authentication);
            }
        }
        Ok(TokenAuth {
            id,
            user_id: parse_uuid(&row.try_get::<String, _>("user_id").map_err(db_error)?)?,
            scopes: row
                .try_get::<String, _>("scopes")
                .map_err(db_error)?
                .split_whitespace()
                .map(str::to_owned)
                .collect(),
            list_id: row
                .try_get::<Option<String>, _>("list_id")
                .map_err(db_error)?
                .map(|value| value.parse())
                .transpose()?,
            domain_id: row
                .try_get::<Option<String>, _>("domain_id")
                .map_err(db_error)?
                .map(|value| parse_uuid(&value))
                .transpose()?,
        })
    }
    /// Updates the successful-use timestamp after all authorization checks pass.
    ///
    /// # Errors
    /// Returns a database error.
    pub async fn mark_used(&self, id: TokenId) -> Result<()> {
        sqlx::query("UPDATE api_tokens SET last_used_at=$1 WHERE id=$2")
            .bind(now())
            .bind(id.to_string())
            .execute(&self.db.pool)
            .await
            .map_err(db_error)?;
        Ok(())
    }
    /// # Errors
    /// Returns a missing-token or database/audit error.
    pub async fn revoke(&self, id: TokenId) -> Result<()> {
        self.revoke_with_context(id, &AuditContext::system()).await
    }
    /// Revokes a token attributed to the supplied audit context.
    ///
    /// # Errors
    /// Returns a missing-token or database/audit error.
    pub async fn revoke_with_context(&self, id: TokenId, context: &AuditContext) -> Result<()> {
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        let changed =
            sqlx::query("UPDATE api_tokens SET revoked_at=$1 WHERE id=$2 AND revoked_at IS NULL")
                .bind(now())
                .bind(id.to_string())
                .execute(&mut *tx)
                .await
                .map_err(db_error)?
                .rows_affected();
        if changed == 0 {
            return Err(Error::NotFound(id.to_string()));
        }
        Database::record_tx_with_context(
            &mut tx,
            context,
            "token.revoke",
            "token",
            &id.to_string(),
            serde_json::json!({}),
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AuditRepo<'a> {
    db: &'a Database,
}
impl AuditRepo<'_> {
    /// # Errors
    /// Returns an error when audit records cannot be queried or decoded.
    pub async fn list(&self) -> Result<Vec<AuditEntry>> {
        let rows = sqlx::query("SELECT id,at,actor_user_id,actor_token_id,ip,action,target_type,target_id,diff FROM audit_log ORDER BY at")
            .fetch_all(&self.db.pool).await.map_err(db_error)?;
        rows.iter()
            .map(|row| {
                Ok(AuditEntry {
                    id: row.try_get("id").map_err(db_error)?,
                    at: parse_time(&row.try_get::<String, _>("at").map_err(db_error)?)?,
                    actor_user_id: row
                        .try_get::<Option<String>, _>("actor_user_id")
                        .map_err(db_error)?
                        .map(|value| parse_uuid(&value))
                        .transpose()?,
                    actor_token_id: row
                        .try_get::<Option<String>, _>("actor_token_id")
                        .map_err(db_error)?
                        .map(|value| parse_uuid(&value))
                        .transpose()?,
                    ip: row
                        .try_get::<Option<String>, _>("ip")
                        .map_err(db_error)?
                        .map(|value| value.parse().map_err(db_error))
                        .transpose()?,
                    action: row.try_get("action").map_err(db_error)?,
                    target_type: row.try_get("target_type").map_err(db_error)?,
                    target_id: row.try_get("target_id").map_err(db_error)?,
                    diff: serde_json::from_str(
                        &row.try_get::<String, _>("diff").map_err(db_error)?,
                    )
                    .map_err(db_error)?,
                })
            })
            .collect()
    }
}
