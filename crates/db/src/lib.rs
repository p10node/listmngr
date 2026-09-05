#![forbid(unsafe_code)]

//! Portable PostgreSQL/SQLite repositories for the Phase 1 model.

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
        })
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

    async fn record_tx_with_context(
        tx: &mut Transaction<'_, Any>,
        context: &AuditContext,
        action: &str,
        target_type: &str,
        target_id: &str,
        diff: serde_json::Value,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO audit_log(id,at,actor_user_id,actor_token_id,ip,action,target_type,target_id,diff) VALUES(?,?,?,?,?,?,?,?,?)",
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
        self.scopes.contains("admin") || self.domain_id.is_none_or(|bound| bound == domain)
    }

    #[must_use]
    pub fn allows_list(&self, list: &ListId, domain: DomainId) -> bool {
        self.scopes.contains("admin")
            || (self.list_id.as_ref().is_none_or(|bound| bound == list)
                && self.domain_id.is_none_or(|bound| bound == domain))
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
        sqlx::query("INSERT INTO domain_owners(domain_id,user_id) VALUES(?,?)")
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

    /// # Errors
    ///
    /// Returns an error for invalid domain data, missing/conflicting records, or database/audit transaction failure.
    pub async fn owners(&self, host: &str) -> Result<Vec<User>> {
        let domain = self.get(host).await?;
        let rows = sqlx::query("SELECT u.id,u.display_name,u.is_server_owner,u.locale,u.timezone,u.preferred_address_id,u.created_at FROM users u JOIN domain_owners o ON o.user_id=u.id WHERE o.domain_id=? ORDER BY u.created_at")
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
        sqlx::query("INSERT INTO domains(id,mail_host,description,alias_domain,created_at) VALUES(?,?,?,?,?)")
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
        let row = sqlx::query("SELECT id,mail_host,description,alias_domain,created_at FROM domains WHERE mail_host=?")
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
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mailing_lists WHERE mail_host=?")
            .bind(host.to_ascii_lowercase())
            .fetch_one(&self.db.pool)
            .await
            .map_err(db_error)?;
        if count != 0 {
            return Err(Error::Conflict("domain owns lists".into()));
        }
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        let changed = sqlx::query("DELETE FROM domains WHERE mail_host=?")
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
    fn password_hasher(self) -> Result<Argon2<'static>> {
        let params = Params::new(
            self.db.argon2.memory_kib,
            self.db.argon2.iterations,
            self.db.argon2.parallelism,
            None,
        )
        .map_err(db_error)?;
        Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
    }

    fn validate_password(self, password: &str) -> Result<()> {
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
        sqlx::query("INSERT INTO preferences(id) VALUES(?)")
            .bind(pref.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        sqlx::query("INSERT INTO users(id,display_name,is_server_owner,preferences_id,locale,timezone,preferred_address_id,created_at) VALUES(?,?,?,?,?,?,?,?)")
            .bind(user.id.to_string()).bind(&user.display_name).bind(i64::from(user.is_server_owner)).bind(pref.to_string()).bind(&user.locale).bind(&user.timezone).bind(Option::<String>::None).bind(user.created_at.to_rfc3339()).execute(&mut *tx).await.map_err(db_error)?;
        sqlx::query(
            "INSERT INTO user_credentials(user_id,password_hash,password_updated_at) VALUES(?,?,?)",
        )
        .bind(user.id.to_string())
        .bind(hash)
        .bind(now())
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        sqlx::query("INSERT INTO addresses(id,email,original_email,display_name,user_id,registered_on) VALUES(?,?,?,?,?,?)")
            .bind(address.id.to_string()).bind(&address.email).bind(&address.original_email).bind(&address.display_name).bind(user.id.to_string()).bind(address.registered_on.to_rfc3339()).execute(&mut *tx).await.map_err(db_error)?;
        sqlx::query("UPDATE users SET preferred_address_id=? WHERE id=?")
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
        let row = sqlx::query("SELECT user_id FROM addresses WHERE email=?")
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
            "SELECT id,display_name,is_server_owner,locale,timezone,preferred_address_id,created_at FROM users WHERE {column}=?"
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
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        sqlx::query("UPDATE users SET display_name=?,locale=?,timezone=? WHERE id=?")
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
            "UPDATE user_credentials SET password_hash=?,password_updated_at=? WHERE user_id=?",
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
        let row = sqlx::query("SELECT password_hash FROM user_credentials WHERE user_id=?")
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
        sqlx::query("DELETE FROM user_credentials WHERE user_id=?")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        sqlx::query("UPDATE addresses SET user_id=NULL WHERE user_id=?")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        let changed = sqlx::query("DELETE FROM users WHERE id=?")
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
        let row = sqlx::query("SELECT id,email,original_email,display_name,user_id,verified_on,registered_on FROM addresses WHERE id=?")
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
        let row = sqlx::query("SELECT id,email,original_email,display_name,user_id,verified_on,registered_on FROM addresses WHERE email=?").bind(email.to_ascii_lowercase()).fetch_optional(&self.db.pool).await.map_err(db_error)?.ok_or_else(|| Error::NotFound(email.into()))?;
        address_from_row(&row)
    }
    /// Lists every address linked to a user in stable email order.
    ///
    /// # Errors
    ///
    /// Returns an error when addresses cannot be queried or decoded.
    pub async fn by_user(&self, user: UserId) -> Result<Vec<Address>> {
        let rows = sqlx::query("SELECT id,email,original_email,display_name,user_id,verified_on,registered_on FROM addresses WHERE user_id=? ORDER BY email")
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
        let changed = sqlx::query("UPDATE addresses SET verified_on=? WHERE email=?")
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
        let changed = sqlx::query("UPDATE addresses SET user_id=? WHERE email=?")
            .bind(user.map(|v| v.to_string()))
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
        self.db.domains().get(new.list_id.mail_host()).await?;
        let mut list = MailingList::new(new.list_id, new.display_name);
        let style = builtin_styles()
            .into_iter()
            .find(|s| s.name() == new.style)
            .ok_or_else(|| Error::Validation(format!("unknown style: {}", new.style)))?;
        style.apply(&mut list);
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        sqlx::query("INSERT INTO mailing_lists(list_id,list_name,mail_host,display_name,description,info,subject_prefix,advertised,preferred_language,anonymous_list,created_at,post_id,volume,next_digest_number,digest_last_sent_at,emergency,archive_policy,archive_rendering_mode,style_name) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
            .bind(list.id.to_string()).bind(list.id.list_name()).bind(list.id.mail_host()).bind(&list.display_name).bind(&list.description).bind(&list.info).bind(&list.subject_prefix).bind(i64::from(list.advertised)).bind(&list.preferred_language).bind(i64::from(list.anonymous_list)).bind(list.created_at.to_rfc3339()).bind(list.post_id).bind(list.volume).bind(list.next_digest_number).bind(list.digest_last_sent_at.map(|value| value.to_rfc3339())).bind(i64::from(list.emergency)).bind(list.archive_policy.to_string()).bind(list.archive_rendering_mode.to_string()).bind(&list.style_name).execute(&mut *tx).await.map_err(db_error)?;
        Database::record_tx_with_context(
            &mut tx,
            context,
            "list.create",
            "list",
            list.id.as_str(),
            serde_json::json!({"style":list.style_name}),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(list)
    }
    /// # Errors
    ///
    /// Returns an error for invalid list data, missing/conflicting records, or database/audit transaction failure.
    pub async fn get(&self, id: &ListId) -> Result<MailingList> {
        let row = sqlx::query("SELECT * FROM mailing_lists WHERE list_id=?")
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
            sqlx::query("SELECT * FROM mailing_lists WHERE advertised=? ORDER BY list_id")
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
        let rows = sqlx::query("SELECT * FROM mailing_lists WHERE mail_host=? ORDER BY list_id")
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
        let mut list = self.get(id).await?;
        let object = patch
            .as_object()
            .ok_or_else(|| Error::Validation("config patch must be an object".into()))?;
        for (key, value) in object {
            match key.as_str() {
                "display_name" => {
                    list.display_name = value
                        .as_str()
                        .ok_or_else(|| Error::Validation(key.clone()))?
                        .into();
                }
                "description" => {
                    list.description = value
                        .as_str()
                        .ok_or_else(|| Error::Validation(key.clone()))?
                        .into();
                }
                "info" => {
                    list.info = value
                        .as_str()
                        .ok_or_else(|| Error::Validation(key.clone()))?
                        .into();
                }
                "subject_prefix" => {
                    list.subject_prefix = value
                        .as_str()
                        .ok_or_else(|| Error::Validation(key.clone()))?
                        .into();
                }
                "advertised" => {
                    list.advertised = value
                        .as_bool()
                        .ok_or_else(|| Error::Validation(key.clone()))?;
                }
                "preferred_language" => {
                    let language = value
                        .as_str()
                        .filter(|language| !language.trim().is_empty())
                        .ok_or_else(|| Error::Validation(key.clone()))?;
                    list.preferred_language = language.into();
                }
                "anonymous_list" => {
                    list.anonymous_list = value
                        .as_bool()
                        .ok_or_else(|| Error::Validation(key.clone()))?;
                }
                "next_digest_number" => {
                    list.next_digest_number = value
                        .as_i64()
                        .filter(|number| *number >= 1)
                        .ok_or_else(|| Error::Validation(key.clone()))?;
                }
                "emergency" => {
                    list.emergency = value
                        .as_bool()
                        .ok_or_else(|| Error::Validation(key.clone()))?;
                }
                "archive_policy" => {
                    list.archive_policy = value
                        .as_str()
                        .ok_or_else(|| Error::Validation(key.clone()))?
                        .parse()?;
                }
                "archive_rendering_mode" => {
                    list.archive_rendering_mode = value
                        .as_str()
                        .ok_or_else(|| Error::Validation(key.clone()))?
                        .parse()?;
                }
                _ => {
                    return Err(Error::Validation(format!(
                        "read-only or unknown list setting: {key}"
                    )));
                }
            }
        }
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        sqlx::query("UPDATE mailing_lists SET display_name=?,description=?,info=?,subject_prefix=?,advertised=?,preferred_language=?,anonymous_list=?,next_digest_number=?,emergency=?,archive_policy=?,archive_rendering_mode=? WHERE list_id=?")
            .bind(&list.display_name).bind(&list.description).bind(&list.info).bind(&list.subject_prefix).bind(i64::from(list.advertised)).bind(&list.preferred_language).bind(i64::from(list.anonymous_list)).bind(list.next_digest_number).bind(i64::from(list.emergency)).bind(list.archive_policy.to_string()).bind(list.archive_rendering_mode.to_string()).bind(id.as_str()).execute(&mut *tx).await.map_err(db_error)?;
        Database::record_tx_with_context(
            &mut tx,
            context,
            "list.config",
            "list",
            id.as_str(),
            patch.clone(),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
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
        let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mailing_lists WHERE list_id=?")
            .bind(id.as_str())
            .fetch_one(&mut *tx)
            .await
            .map_err(db_error)?;
        if exists == 0 {
            return Err(Error::NotFound(id.to_string()));
        }

        // Keep this explicit rather than relying on backend-specific cascades or
        // data-modifying CTEs: both SQLite and PostgreSQL execute every step in
        // the same SQLx transaction. Member preferences are list-owned, while
        // their users and addresses are shared identity records.
        let preference_rows = sqlx::query("SELECT preferences_id FROM members WHERE list_id=?")
            .bind(id.as_str())
            .fetch_all(&mut *tx)
            .await
            .map_err(db_error)?;
        let preference_ids = preference_rows
            .iter()
            .map(|row| row.try_get::<String, _>("preferences_id"))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_error)?;

        for table in ["list_archivers", "header_matches", "bans"] {
            sqlx::query(&format!("DELETE FROM {table} WHERE list_id=?"))
                .bind(id.as_str())
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
        }
        sqlx::query("DELETE FROM templates WHERE scope='list' AND scope_id=?")
            .bind(id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        sqlx::query("DELETE FROM members WHERE list_id=?")
            .bind(id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        for preference_id in preference_ids {
            sqlx::query("DELETE FROM preferences WHERE id=?")
                .bind(preference_id)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
        }
        sqlx::query("DELETE FROM mailing_lists WHERE list_id=?")
            .bind(id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        Database::record_tx_with_context(
            &mut tx,
            context,
            "list.delete",
            "list",
            id.as_str(),
            serde_json::json!({}),
        )
        .await?;
        tx.commit().await.map_err(db_error)
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
        sqlx::query("INSERT INTO list_archivers(list_id,name,enabled) VALUES(?,?,?) ON CONFLICT(list_id,name) DO UPDATE SET enabled=excluded.enabled")
            .bind(id.as_str()).bind(name).bind(i64::from(enabled)).execute(&mut *tx).await.map_err(db_error)?;
        Database::record_tx_with_context(
            &mut tx,
            context,
            "list.archiver.set",
            "list",
            id.as_str(),
            serde_json::json!({"name":name,"enabled":enabled}),
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }

    /// # Errors
    ///
    /// Returns an error for invalid list data, missing/conflicting records, or database/audit transaction failure.
    pub async fn archivers(&self, id: &ListId) -> Result<Vec<(String, bool)>> {
        self.get(id).await?;
        let rows =
            sqlx::query("SELECT name,enabled FROM list_archivers WHERE list_id=? ORDER BY name")
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

    /// Sets a list template attributed to the supplied audit context.
    ///
    /// # Errors
    ///
    /// Returns an error if the list is missing or the database/audit transaction fails.
    pub async fn set_template_with_context(
        &self,
        id: &ListId,
        name: &str,
        language: &str,
        body: &str,
        context: &AuditContext,
    ) -> Result<()> {
        self.get(id).await?;
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        sqlx::query("INSERT INTO templates(id,name,scope,scope_id,language,body) VALUES(?,?,?,?,?,?) ON CONFLICT(name,scope,scope_id,language) DO UPDATE SET body=excluded.body")
            .bind(Uuid::now_v7().to_string()).bind(name).bind("list").bind(id.as_str()).bind(language).bind(body).execute(&mut *tx).await.map_err(db_error)?;
        Database::record_tx_with_context(
            &mut tx,
            context,
            "list.template.set",
            "list",
            id.as_str(),
            serde_json::json!({"name":name,"language":language}),
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }

    /// # Errors
    ///
    /// Returns an error for invalid list data, missing/conflicting records, or database/audit transaction failure.
    pub async fn templates(&self, id: &ListId) -> Result<Vec<Template>> {
        self.get(id).await?;
        let rows = sqlx::query("SELECT id,name,scope,scope_id,language,uri,body FROM templates WHERE scope='list' AND scope_id=? ORDER BY name,language")
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
fn list_from_row(row: &sqlx::any::AnyRow) -> Result<MailingList> {
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
        emergency: row.try_get::<i64, _>("emergency").map_err(db_error)? != 0,
        archive_policy: row
            .try_get::<String, _>("archive_policy")
            .map_err(db_error)?
            .parse()?,
        archive_rendering_mode: row
            .try_get::<String, _>("archive_rendering_mode")
            .map_err(db_error)?
            .parse()?,
        style_name: row.try_get("style_name").map_err(db_error)?,
    })
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
    let rows = sqlx::query("SELECT m.id,m.preferences_id,a.email FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id=? AND m.role=? ORDER BY a.email")
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
    removed: &[(String, String)],
) -> Result<()> {
    for (member_id, preferences_id) in removed {
        sqlx::query("DELETE FROM members WHERE id=?")
            .bind(member_id)
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
        sqlx::query("DELETE FROM preferences WHERE id=?")
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
    list: &ListId,
    role: MemberRole,
    mode_for_new: SubscriptionMode,
    additions: &[&Address],
) -> Result<()> {
    for address in additions {
        let row = sqlx::query("SELECT id,user_id FROM addresses WHERE email=?")
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
            sqlx::query("INSERT INTO addresses(id,email,original_email,display_name,registered_on) VALUES(?,?,?,?,?)")
                .bind(address.id.to_string()).bind(&address.email).bind(&address.original_email)
                .bind(&address.display_name).bind(address.registered_on.to_rfc3339())
                .execute(&mut **tx).await.map_err(db_error)?;
            (address.id.to_string(), None)
        };
        let member_id = MemberId::new();
        let preferences_id = PreferencesId::new();
        sqlx::query("INSERT INTO preferences(id) VALUES(?)")
            .bind(preferences_id.to_string())
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
        sqlx::query("INSERT INTO members(id,list_id,role,address_id,user_id,subscription_mode,display_name,preferences_id,created_at) VALUES(?,?,?,?,?,?,?,?,?)")
            .bind(member_id.to_string()).bind(list.as_str()).bind(role.to_string())
            .bind(address_id).bind(user_id).bind(mode_for_new.to_string())
            .bind("").bind(preferences_id.to_string()).bind(now())
            .execute(&mut **tx).await.map_err(db_error)?;
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
        delete_mass_members(&mut tx, &removed).await?;
        let additions = mass_additions(operation, &addresses, &existing);
        insert_mass_members(&mut tx, list, role, mode_for_new, &additions).await?;
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
            created_at: Utc::now(),
        };
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        if insert_address {
            sqlx::query("INSERT INTO addresses(id,email,original_email,display_name,registered_on) VALUES(?,?,?,?,?)").bind(address.id.to_string()).bind(&address.email).bind(&address.original_email).bind(&address.display_name).bind(address.registered_on.to_rfc3339()).execute(&mut *tx).await.map_err(db_error)?;
        }
        if verified {
            sqlx::query("UPDATE addresses SET verified_on=? WHERE id=?")
                .bind(now())
                .bind(address.id.to_string())
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
        }
        sqlx::query("INSERT INTO preferences(id) VALUES(?)")
            .bind(pref.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        sqlx::query("INSERT INTO members(id,list_id,role,address_id,user_id,subscription_mode,display_name,preferences_id,created_at) VALUES(?,?,?,?,?,?,?,?,?)").bind(member.id.to_string()).bind(member.list_id.as_str()).bind(member.role.to_string()).bind(member.address_id.to_string()).bind(member.user_id.map(|v|v.to_string())).bind(member.subscription_mode.to_string()).bind(&member.display_name).bind(pref.to_string()).bind(member.created_at.to_rfc3339()).execute(&mut *tx).await.map_err(db_error)?;
        Database::record_tx_with_context(
            &mut tx,
            context,
            "member.create",
            "member",
            &member.id.to_string(),
            serde_json::json!({"list_id":member.list_id,"role":member.role,"verified":verified}),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(member)
    }
    /// # Errors
    ///
    /// Returns an error for invalid member data, missing/conflicting records, or database/audit transaction failure.
    pub async fn get(&self, id: MemberId) -> Result<Member> {
        let row = sqlx::query("SELECT * FROM members WHERE id=?")
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
            sqlx::query("SELECT m.* FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id=? AND m.role=? ORDER BY a.email,m.id")
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
        let rows=sqlx::query("SELECT m.* FROM members m JOIN addresses a ON a.id=m.address_id WHERE a.email=? ORDER BY m.list_id,m.role,m.id").bind(email.to_ascii_lowercase()).fetch_all(&self.db.pool).await.map_err(db_error)?;
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
        let rows = sqlx::query("SELECT m.* FROM members m JOIN addresses a ON a.id=m.address_id WHERE a.email LIKE ? ESCAPE '\\' ORDER BY m.list_id,m.role,m.id")
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
        let mut preferences = self.db.preferences().get(current.preferences_id).await?;
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
        sqlx::query("UPDATE members SET display_name=?,role=?,subscription_mode=? WHERE id=?")
            .bind(display_name)
            .bind(role.to_string())
            .bind(mode.to_string())
            .bind(id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        PreferencesRepo::set_tx(&mut tx, current.preferences_id, &preferences).await?;
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
        let changed = sqlx::query("DELETE FROM members WHERE id=?")
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
    async fn set_tx(
        tx: &mut Transaction<'_, Any>,
        id: PreferencesId,
        p: &Preferences,
    ) -> Result<()> {
        sqlx::query("UPDATE preferences SET acknowledge_posts=?,hide_address=?,preferred_language=?,receive_list_copy=?,receive_own_postings=?,delivery_mode=?,delivery_status=? WHERE id=?").bind(p.acknowledge_posts.map(i64::from)).bind(p.hide_address.map(i64::from)).bind(&p.preferred_language).bind(p.receive_list_copy.map(i64::from)).bind(p.receive_own_postings.map(i64::from)).bind(p.delivery_mode.map(|v|v.to_string())).bind(p.delivery_status.map(|v|v.to_string())).bind(id.to_string()).execute(&mut **tx).await.map_err(db_error)?;
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
        let row = sqlx::query("SELECT preferences_id FROM users WHERE id=?")
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
            sqlx::query_scalar("SELECT preferences_id FROM users WHERE id=?")
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
        let existing: Option<String> =
            sqlx::query_scalar("SELECT preferences_id FROM addresses WHERE id=?")
                .bind(address.id.to_string())
                .fetch_one(&self.db.pool)
                .await
                .map_err(db_error)?;
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        let preference_id = if let Some(id) = existing {
            parse_uuid(&id)?
        } else {
            let id = PreferencesId::new();
            sqlx::query("INSERT INTO preferences(id) VALUES(?)")
                .bind(id.to_string())
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
            sqlx::query("UPDATE addresses SET preferences_id=? WHERE id=?")
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
            sqlx::query_scalar("SELECT preferences_id FROM addresses WHERE id=?")
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
        let row = sqlx::query("SELECT * FROM preferences WHERE id=?")
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
            sqlx::query_scalar("SELECT preferences_id FROM addresses WHERE id=?")
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
        let id = TokenId::new();
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        let secret = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        let token = format!("lm_{id}_{secret}");
        let hash = format!("{:x}", Sha256::digest(secret.as_bytes()));
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        sqlx::query("INSERT INTO api_tokens(id,user_id,name,token_hash,scopes,list_id,domain_id,expires_at,created_at) VALUES(?,?,?,?,?,?,?,?,?)")
            .bind(id.to_string()).bind(input.user.to_string()).bind(input.name).bind(hash).bind(input.scopes.join(" "))
            .bind(input.list_id.map(ToString::to_string)).bind(input.domain_id.map(|value| value.to_string()))
            .bind(input.expires.map(|value| value.to_rfc3339())).bind(now()).execute(&mut *tx).await.map_err(db_error)?;
        Database::record_tx_with_context(&mut tx, context, "token.create", "token", &id.to_string(), serde_json::json!({"scopes":input.scopes,"list_id":input.list_id,"domain_id":input.domain_id})).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(IssuedToken { id, token })
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
        let row=sqlx::query("SELECT user_id,token_hash,scopes,list_id,domain_id,expires_at,revoked_at FROM api_tokens WHERE id=?").bind(id.to_string()).fetch_optional(&self.db.pool).await.map_err(db_error)?.ok_or(Error::Authentication)?;
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
        sqlx::query("UPDATE api_tokens SET last_used_at=? WHERE id=?")
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
            sqlx::query("UPDATE api_tokens SET revoked_at=? WHERE id=? AND revoked_at IS NULL")
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
