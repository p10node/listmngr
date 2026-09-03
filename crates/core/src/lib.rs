#![forbid(unsafe_code)]

//! Shared configuration and Phase 0-1 domain model.

use std::{
    fmt,
    path::{Path, PathBuf},
    str::FromStr,
};

use chrono::{DateTime, Utc};
use figment::{
    Figment,
    providers::{Env, Format, Serialized, Toml},
};
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum Error {
    #[error("configuration error: {0}")]
    Config(Box<figment::Error>),
    #[error("invalid list id: {0}")]
    InvalidListId(String),
    #[error("validation failed: {0}")]
    Validation(String),
    #[error("resource not found: {0}")]
    NotFound(String),
    #[error("resource already exists: {0}")]
    Conflict(String),
    #[error("authentication failed")]
    Authentication,
    #[error("permission denied: scope {0} is required")]
    Forbidden(String),
    #[error("rate limit exceeded")]
    RateLimited,
    #[error("database error: {0}")]
    Database(String),
}

impl From<figment::Error> for Error {
    fn from(value: figment::Error) -> Self {
        Self::Config(Box::new(value))
    }
}

pub type Result<T> = std::result::Result<T, Error>;

macro_rules! uuid_id {
    ($name:ident) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema,
        )]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;
            fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
                Uuid::parse_str(value).map(Self)
            }
        }
    };
}

uuid_id!(DomainId);
uuid_id!(UserId);
uuid_id!(AddressId);
uuid_id!(MemberId);
uuid_id!(PreferencesId);
uuid_id!(TokenId);

/// Normalizes a DNS domain using IDNA2008 and enforces DNS wire limits.
///
/// # Errors
/// Returns [`Error::Validation`] for control characters, invalid IDNA, or invalid labels.
pub fn normalize_domain(value: &str) -> Result<String> {
    let value = value.trim().trim_end_matches('.');
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(Error::Validation("invalid domain".into()));
    }
    let ascii = idna::domain_to_ascii(value)
        .map_err(|_| Error::Validation("invalid IDNA domain".into()))?
        .to_ascii_lowercase();
    if ascii.len() > 253
        || !ascii.contains('.')
        || ascii.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
    {
        return Err(Error::Validation("invalid domain".into()));
    }
    Ok(ascii)
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(transparent)]
pub struct ListId(String);

impl ListId {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn parts(&self) -> (&str, &str) {
        self.0.split_once('.').expect("validated list id")
    }

    #[must_use]
    pub fn list_name(&self) -> &str {
        self.parts().0
    }

    #[must_use]
    pub fn mail_host(&self) -> &str {
        self.parts().1
    }

    #[must_use]
    pub fn posting_address(&self) -> String {
        format!("{}@{}", self.list_name(), self.mail_host())
    }

    #[must_use]
    pub fn address_with_suffix(&self, suffix: &str) -> String {
        format!("{}-{suffix}@{}", self.list_name(), self.mail_host())
    }

    #[must_use]
    pub fn bounces_address(&self) -> String {
        self.address_with_suffix("bounces")
    }
    #[must_use]
    pub fn join_address(&self) -> String {
        self.address_with_suffix("join")
    }
    #[must_use]
    pub fn leave_address(&self) -> String {
        self.address_with_suffix("leave")
    }
    #[must_use]
    pub fn owner_address(&self) -> String {
        self.address_with_suffix("owner")
    }
    #[must_use]
    pub fn request_address(&self) -> String {
        self.address_with_suffix("request")
    }
}

impl FromStr for ListId {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self> {
        if value.chars().any(char::is_control) || value.len() > 318 {
            return Err(Error::InvalidListId(value.into()));
        }
        let normalized = value.trim().to_lowercase();
        let Some((name, host)) = normalized.split_once('.') else {
            return Err(Error::InvalidListId(value.into()));
        };
        if name.is_empty()
            || name.len() > 64
            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(Error::InvalidListId(value.into()));
        }
        let host = normalize_domain(host).map_err(|_| Error::InvalidListId(value.into()))?;
        Ok(Self(format!("{name}.{host}")))
    }
}

impl fmt::Display for ListId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

macro_rules! string_enum {
    ($name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }
        impl $name {
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $wire),+ }
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.as_str()) }
        }
        impl FromStr for $name {
            type Err = Error;
            fn from_str(value: &str) -> Result<Self> {
                match value { $($wire => Ok(Self::$variant),)+ _ => Err(Error::Validation(format!("invalid {}: {value}", stringify!($name)))) }
            }
        }
    };
}

string_enum!(MemberRole { Owner => "owner", Moderator => "moderator", Member => "member", Nonmember => "nonmember" });
string_enum!(SubscriptionMode { AsAddress => "as_address", AsUser => "as_user" });
string_enum!(DeliveryMode { Regular => "regular", PlaintextDigests => "plaintext_digests", MimeDigests => "mime_digests", SummaryDigests => "summary_digests" });
string_enum!(DeliveryStatus { Enabled => "enabled", ByUser => "by_user", ByBounces => "by_bounces", ByModerator => "by_moderator", Unknown => "unknown" });
string_enum!(ModerationAction { Defer => "defer", Accept => "accept", Hold => "hold", Reject => "reject", Discard => "discard" });
string_enum!(ArchivePolicy { Public => "public", Private => "private", Never => "never" });
string_enum!(ArchiveRenderingMode { Text => "text", Markdown => "markdown" });

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Preferences {
    pub acknowledge_posts: Option<bool>,
    pub hide_address: Option<bool>,
    pub preferred_language: Option<String>,
    pub receive_list_copy: Option<bool>,
    pub receive_own_postings: Option<bool>,
    pub delivery_mode: Option<DeliveryMode>,
    pub delivery_status: Option<DeliveryStatus>,
}

impl Preferences {
    #[must_use]
    pub fn resolve<'a>(layers: impl IntoIterator<Item = &'a Self>) -> Self {
        let mut value = Self::default();
        for layer in layers {
            if layer.acknowledge_posts.is_some() {
                value.acknowledge_posts = layer.acknowledge_posts;
            }
            if layer.hide_address.is_some() {
                value.hide_address = layer.hide_address;
            }
            if layer.preferred_language.is_some() {
                value
                    .preferred_language
                    .clone_from(&layer.preferred_language);
            }
            if layer.receive_list_copy.is_some() {
                value.receive_list_copy = layer.receive_list_copy;
            }
            if layer.receive_own_postings.is_some() {
                value.receive_own_postings = layer.receive_own_postings;
            }
            if layer.delivery_mode.is_some() {
                value.delivery_mode = layer.delivery_mode;
            }
            if layer.delivery_status.is_some() {
                value.delivery_status = layer.delivery_status;
            }
        }
        value
    }

    #[must_use]
    pub const fn system_defaults(language: String) -> Self {
        Self {
            acknowledge_posts: Some(false),
            hide_address: Some(false),
            preferred_language: Some(language),
            receive_list_copy: Some(true),
            receive_own_postings: Some(true),
            delivery_mode: Some(DeliveryMode::Regular),
            delivery_status: Some(DeliveryStatus::Enabled),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Domain {
    pub id: DomainId,
    pub mail_host: String,
    pub description: String,
    pub alias_domain: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: UserId,
    pub display_name: String,
    pub is_server_owner: bool,
    pub locale: String,
    pub timezone: String,
    pub preferred_address_id: Option<AddressId>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Address {
    pub id: AddressId,
    pub email: String,
    pub original_email: String,
    pub display_name: String,
    pub user_id: Option<UserId>,
    pub verified_on: Option<DateTime<Utc>>,
    pub registered_on: DateTime<Utc>,
}

impl Address {
    /// Builds a normalized address while preserving the original spelling.
    ///
    /// # Errors
    /// Returns [`Error::Validation`] for malformed addresses or control characters.
    pub fn new(email: &str, display_name: String) -> Result<Self> {
        let original_email = email.trim().to_owned();
        if email != original_email
            || original_email.len() > 254
            || original_email.chars().any(char::is_control)
            || original_email.matches('@').count() != 1
        {
            return Err(Error::Validation("invalid email address".into()));
        }
        let (local, domain) = original_email
            .split_once('@')
            .ok_or_else(|| Error::Validation("email must contain @".into()))?;
        if local.is_empty()
            || local.len() > 64
            || local.starts_with('.')
            || local.ends_with('.')
            || local.contains("..")
            || local.chars().any(char::is_whitespace)
        {
            return Err(Error::Validation("invalid email local part".into()));
        }
        let domain = normalize_domain(domain)?;
        let normalized = format!("{}@{domain}", local.to_lowercase());
        if normalized.len() > 254 {
            return Err(Error::Validation("email exceeds 254 bytes".into()));
        }
        Ok(Self {
            id: AddressId::new(),
            email: normalized,
            original_email,
            display_name,
            user_id: None,
            verified_on: None,
            registered_on: Utc::now(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailingList {
    pub id: ListId,
    pub display_name: String,
    pub description: String,
    pub info: String,
    pub subject_prefix: String,
    pub advertised: bool,
    pub preferred_language: String,
    pub anonymous_list: bool,
    pub created_at: DateTime<Utc>,
    pub last_post_at: Option<DateTime<Utc>>,
    pub post_id: i64,
    pub volume: i32,
    pub archive_policy: ArchivePolicy,
    pub archive_rendering_mode: ArchiveRenderingMode,
    pub style_name: String,
}

impl MailingList {
    #[must_use]
    pub fn new(id: ListId, display_name: String) -> Self {
        Self {
            subject_prefix: format!("[{}] ", id.list_name()),
            id,
            display_name,
            description: String::new(),
            info: String::new(),
            advertised: true,
            preferred_language: "en".into(),
            anonymous_list: false,
            created_at: Utc::now(),
            last_post_at: None,
            post_id: 1,
            volume: 1,
            archive_policy: ArchivePolicy::Public,
            archive_rendering_mode: ArchiveRenderingMode::Text,
            style_name: "legacy-default".into(),
        }
    }

    #[must_use]
    pub fn fqdn_listname(&self) -> String {
        self.id.posting_address()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Member {
    pub id: MemberId,
    pub list_id: ListId,
    pub role: MemberRole,
    pub address_id: AddressId,
    pub user_id: Option<UserId>,
    pub subscription_mode: SubscriptionMode,
    pub moderation_action: Option<ModerationAction>,
    pub display_name: String,
    pub preferences_id: PreferencesId,
    pub created_at: DateTime<Utc>,
}

pub trait ListStyle: fmt::Debug + Send + Sync {
    fn name(&self) -> &'static str;
    fn apply(&self, list: &mut MailingList);
}

#[derive(Debug)]
struct BuiltinStyle {
    name: &'static str,
}
impl ListStyle for BuiltinStyle {
    fn name(&self) -> &'static str {
        self.name
    }
    fn apply(&self, list: &mut MailingList) {
        list.style_name = self.name.into();
        match self.name {
            "legacy-announce" => {
                list.subject_prefix = format!("[{}] ", list.id.list_name());
            }
            "private-default" => {
                list.advertised = false;
                list.archive_policy = ArchivePolicy::Private;
            }
            _ => {}
        }
    }
}

#[must_use]
pub fn builtin_styles() -> Vec<Box<dyn ListStyle>> {
    vec![
        Box::new(BuiltinStyle {
            name: "legacy-default",
        }),
        Box::new(BuiltinStyle {
            name: "legacy-announce",
        }),
        Box::new(BuiltinStyle {
            name: "private-default",
        }),
    ]
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub site: SiteConfig,
    pub database: DatabaseConfig,
    pub message_store: MessageStoreConfig,
    pub mta: MtaConfig,
    pub web: WebConfig,
    pub api: ApiConfig,
    pub security: SecurityConfig,
    pub mailman: MailmanConfig,
    pub archive: ArchiveConfig,
    pub runners: RunnerConfig,
    pub observability: ObservabilityConfig,
}

impl Config {
    /// Loads defaults, TOML and environment overrides, in that order.
    /// Secret-file values are applied last and trimmed.
    ///
    /// # Errors
    /// Returns an error for invalid configuration or an unreadable/empty secret file.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let mut figment = Figment::from(Serialized::defaults(Self::default()));
        if let Some(path) = path {
            figment = figment.merge(Toml::file(path));
        }
        let mut config: Self = figment
            .merge(Env::prefixed("LISTMNGR__").split("__"))
            .extract()?;
        if let Some(secret_file) = &config.database.url_file {
            let value = std::fs::read_to_string(secret_file).map_err(|error| {
                Error::Validation(format!("cannot read database.url_file: {error}"))
            })?;
            let value = value.trim();
            if value.is_empty() {
                return Err(Error::Validation("database.url_file is empty".into()));
            }
            value.clone_into(&mut config.database.url);
        }
        Ok(config)
    }

    /// Produces a representation safe for CLI, HTTP and structured logs.
    #[must_use]
    pub fn redacted_json(&self) -> serde_json::Value {
        let Ok(mut value) = serde_json::to_value(self) else {
            return serde_json::Value::Null;
        };
        if let Some(database) = value
            .get_mut("database")
            .and_then(serde_json::Value::as_object_mut)
        {
            database.insert("url".into(), serde_json::Value::String("[REDACTED]".into()));
            database.remove("url_file");
        }
        value
    }

    /// Returns a credential-free database backend name.
    #[must_use]
    pub fn database_backend(&self) -> &'static str {
        if self.database.url.starts_with("postgres:")
            || self.database.url.starts_with("postgresql:")
        {
            "postgresql"
        } else if self.database.url.starts_with("sqlite:") {
            "sqlite"
        } else {
            "unknown"
        }
    }
}

macro_rules! config_struct {
    ($name:ident { $($field:ident : $ty:ty = $value:expr),+ $(,)? }) => {
        #[derive(Debug, Clone, Serialize, Deserialize)]
        #[serde(default)]
        pub struct $name { $(pub $field: $ty),+ }
        impl Default for $name { fn default() -> Self { Self { $($field: $value),+ } } }
    };
}

config_struct!(SiteConfig {
    name: String = "Example Lists".into(),
    site_owner: String = "postmaster@example.com".into(),
    default_language: String = "en".into(),
    base_url: String = "http://127.0.0.1:8000".into()
});
config_struct!(DatabaseConfig {
    url: String = "sqlite://data/listmngr.db?mode=rwc".into(),
    url_file: Option<PathBuf> = None,
    max_connections: u32 = 20
});
config_struct!(MessageStoreConfig {
    backend: String = "fs".into(),
    path: String = "data/messages".into()
});
config_struct!(MtaConfig {
    incoming: String = "none".into(),
    lmtp_listen: String = "127.0.0.1:8024".into(),
    smtp_relay: String = "127.0.0.1:25".into(),
    smtp_tls: String = "opportunistic".into(),
    max_recipients: u32 = 500,
    postfix_map_dir: String = "data/postfix".into(),
    verp_delimiter: String = "+".into(),
    verp_format: String = "{bounces}+{local}={domain}".into()
});
config_struct!(WebConfig { listen: String = "127.0.0.1:8000".into(), trusted_proxies: Vec<IpNet> = vec!["127.0.0.1/32".parse().expect("valid network")], session_idle: String = "12h".into(), session_absolute: String = "7d".into() });
config_struct!(ApiConfig { listen: String = "127.0.0.1:8001".into(), compat_basic_auth: bool = false, compat_basic_auth_allow: Vec<IpNet> = vec!["127.0.0.1/32".parse().expect("valid network")] });
config_struct!(Argon2Config {
    memory_kib: u32 = 65_536,
    iterations: u32 = 3,
    parallelism: u32 = 1
});
config_struct!(RateLimitConfig {
    login: String = "5/min".into(),
    subscribe: String = "10/hour".into(),
    api: String = "600/min".into()
});
config_struct!(SecurityConfig { argon2: Argon2Config = Argon2Config::default(), password_min_score: u8 = 3, require_2fa_for: Vec<String> = vec!["server_owner".into()], pending_request_life: String = "3d".into(), rate_limit: RateLimitConfig = RateLimitConfig::default() });
config_struct!(MailmanConfig {
    default_member_action: ModerationAction = ModerationAction::Defer,
    default_nonmember_action: ModerationAction = ModerationAction::Hold,
    noreply_address: String = "noreply".into(),
    site_owner_notify: bool = true
});
config_struct!(ArchiveConfig {
    enabled: bool = true,
    index_path: String = "data/index".into(),
    default_policy: ArchivePolicy = ArchivePolicy::Public
});
config_struct!(RunnerConfig {
    lock_timeout: String = "10m".into()
});
config_struct!(ObservabilityConfig {
    log: String = "info".into(),
    metrics: bool = true
});
