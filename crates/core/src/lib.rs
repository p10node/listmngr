#![forbid(unsafe_code)]

//! Shared configuration and Phase 0-1 domain model.

pub mod dsn_issuance;
pub mod metrics;
pub mod one_click;
pub mod verp;

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
    #[error("rate limit exceeded; retry after {retry_after} seconds")]
    RateLimited { retry_after: u64 },
    #[error("database error: {0}")]
    Database(String),
}

impl From<figment::Error> for Error {
    fn from(value: figment::Error) -> Self {
        Self::Config(Box::new(value))
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Remote SMTP command that produced a permanent reply; not a mailbox verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SmtpFailureStage {
    Ehlo,
    MailFrom,
    Rcpt,
    DataStart,
    DataFinal,
}

impl SmtpFailureStage {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ehlo => "ehlo",
            Self::MailFrom => "mail_from",
            Self::Rcpt => "rcpt",
            Self::DataStart => "data_start",
            Self::DataFinal => "data_final",
        }
    }
}

/// Exact remote reply metadata, supplied by the SMTP consumer, never parsed from diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmtpFailure {
    pub stage: SmtpFailureStage,
    pub code: u16,
}

/// Persisted inbound command; token case is significant.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EmailCommand {
    Join,
    Leave,
    Confirm(String),
    Help,
    /// Mailman's `echo`: answer with the text that followed the verb.
    Echo(String),
    /// Mailman's `end`/`stop`: nothing after this line is a command.
    End,
}

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
uuid_id!(WebhookId);
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

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, utoipa::ToSchema)]
#[serde(transparent)]
#[schema(value_type = String, example = "list.example.com")]
pub struct ListId(String);

impl<'de> Deserialize<'de> for ListId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

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
string_enum!(FilterAction { Discard => "discard", Reject => "reject", Forward => "forward", Preserve => "preserve" });
string_enum!(ReplyToMunging { NoMunging => "no_munging", PointToList => "point_to_list", ExplicitHeader => "explicit_header", ExplicitHeaderOnly => "explicit_header_only" });
string_enum!(Personalization { None => "none", Individual => "individual", Full => "full" });
// Mailman's `digest_volume_frequency`: how often the digest volume rolls.
string_enum!(DigestFrequency { Yearly => "yearly", Monthly => "monthly", Quarterly => "quarterly", Weekly => "weekly", Daily => "daily" });
// Mailman's `ResponseAction`: what the list does with mail it answers.
// Mailman's `ResponseAction` names: `respond_and_continue` is what Postorius
// and mailmanclient send; the pre-parity spelling `respond` is still read.
string_enum!(ResponseAction { None => "none", RespondAndContinue => "respond_and_continue", RespondAndDiscard => "respond_and_discard" });
string_enum!(SubscriptionPolicy { Open => "open", Confirm => "confirm", Moderate => "moderate", ConfirmThenModerate => "confirm_then_moderate" });
string_enum!(RosterVisibility { Public => "public", Members => "members", Moderators => "moderators" });
string_enum!(UnrecognizedBounceDisposition { Discard => "discard", SiteOwner => "site_owner", Administrators => "administrators" });
// Mailman's `NewsgroupModeration`: whether the linked newsgroup is moderated,
// and whether the list still posts to it openly.
string_enum!(NewsgroupModeration { None => "none", OpenModerated => "open_moderated", Moderated => "moderated" });
impl Default for NewsgroupModeration {
    fn default() -> Self {
        Self::None
    }
}

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
            // Mailman's system default: an address is hidden from the
            // roster unless the member says otherwise.
            hide_address: Some(true),
            preferred_language: Some(language),
            receive_list_copy: Some(true),
            receive_own_postings: Some(true),
            delivery_mode: Some(DeliveryMode::Regular),
            delivery_status: Some(DeliveryStatus::Enabled),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Domain {
    pub id: DomainId,
    pub mail_host: String,
    pub description: String,
    pub alias_domain: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct User {
    pub id: UserId,
    pub display_name: String,
    pub is_server_owner: bool,
    pub locale: String,
    pub timezone: String,
    pub preferred_address_id: Option<AddressId>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
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

/// Mailman's `dmarc_mitigate_action` vocabulary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DmarcMitigateAction {
    #[default]
    NoMitigation,
    MungeFrom,
    /// Mailman's `wrap_message`: deliver the post whole inside a
    /// list-addressed message, `dmarc_wrapped_message_text` above it.
    WrapMessage,
    /// Mailman's `reject`: refuse the post with `dmarc_moderation_notice`.
    Reject,
    /// Mailman's `discard`: drop the post silently.
    Discard,
}

/// Coupled delivery mitigation settings; serialized as flat compatibility keys.
#[derive(Debug, Clone, Default, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct DmarcSettings {
    #[serde(rename = "dmarc_mitigate_action")]
    pub action: DmarcMitigateAction,
    #[serde(rename = "dmarc_mitigate_unconditionally")]
    pub unconditional: bool,
    /// Exact addresses or `^`-anchored regexes always treated as if their
    /// domain published a restrictive DMARC policy.
    pub dmarc_addresses: Vec<String>,
    /// Text inserted into the hold notice when a post is held for DMARC.
    pub dmarc_moderation_notice: String,
    /// Text of the outer message when `wrap_message` mitigation applies.
    pub dmarc_wrapped_message_text: String,
}

/// Mailman's *Alter Messages* settings: content filtering, header munging
/// Mailman's Automatic Responses: the list answers its own addresses, at
/// most once per address per grace period.
///
/// Each `autorespond_*` says whether that address is answered and whether
/// the original is then discarded; the matching text is the reply body, and
/// an empty one uses the built-in template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct AutomaticResponses {
    #[schema(default = "none")]
    pub autorespond_owner: ResponseAction,
    pub autoresponse_owner_text: String,
    #[schema(default = "none")]
    pub autorespond_postings: ResponseAction,
    pub autoresponse_postings_text: String,
    #[schema(default = "none")]
    pub autorespond_requests: ResponseAction,
    pub autoresponse_request_text: String,
    /// Days before the same address is answered again; `0` answers always.
    #[schema(default = 90)]
    pub autoresponse_grace_period: i32,
}

impl Default for AutomaticResponses {
    fn default() -> Self {
        Self {
            autorespond_owner: ResponseAction::None,
            autoresponse_owner_text: String::new(),
            autorespond_postings: ResponseAction::None,
            autoresponse_postings_text: String::new(),
            autorespond_requests: ResponseAction::None,
            autoresponse_request_text: String::new(),
            autoresponse_grace_period: 90,
        }
    }
}

/// and personalization. Serialized as flat compatibility keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(default)]
// Independent persisted configuration switches, not mutually exclusive states.
#[allow(clippy::struct_excessive_bools)]
pub struct AlterMessages {
    pub filter_content: bool,
    /// MIME types (`type` or `type/subtype`) removed by content filtering.
    pub filter_types: Vec<String>,
    /// MIME types kept by content filtering; empty keeps everything not filtered.
    pub pass_types: Vec<String>,
    /// File-name extensions removed by content filtering.
    pub filter_extensions: Vec<String>,
    /// File-name extensions kept by content filtering.
    pub pass_extensions: Vec<String>,
    #[schema(default = true)]
    pub collapse_alternatives: bool,
    pub convert_html_to_plaintext: bool,
    #[schema(default = "discard")]
    pub filter_action: FilterAction,
    #[schema(default = true)]
    pub include_rfc2369_headers: bool,
    #[schema(default = true)]
    pub allow_list_posts: bool,
    #[schema(default = "no_munging")]
    pub reply_goes_to_list: ReplyToMunging,
    /// Mailbox used by the explicit `Reply-To` policies; empty when unset.
    pub reply_to_address: String,
    pub first_strip_reply_to: bool,
    #[schema(default = "none")]
    pub personalize: Personalization,
    #[schema(default = true)]
    pub include_sender_header: bool,
}

impl Default for AlterMessages {
    fn default() -> Self {
        Self {
            filter_content: false,
            filter_types: Vec::new(),
            pass_types: Vec::new(),
            filter_extensions: Vec::new(),
            pass_extensions: Vec::new(),
            collapse_alternatives: true,
            convert_html_to_plaintext: false,
            filter_action: FilterAction::Discard,
            include_rfc2369_headers: true,
            allow_list_posts: true,
            reply_goes_to_list: ReplyToMunging::NoMunging,
            reply_to_address: String::new(),
            first_strip_reply_to: false,
            personalize: Personalization::None,
            include_sender_header: true,
        }
    }
}

/// One of Mailman's topics: a named pattern whose lines are alternatives,
/// searched in subjects, keywords and the header-like lines opening a body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Topic {
    pub name: String,
    pub pattern: String,
    #[serde(default)]
    pub description: String,
}

/// Mailman's Usenet gateway settings, serialized as flat compatibility keys.
///
/// `gateway_to_mail`, `gateway_to_news`, `linked_newsgroup`,
/// `nntp_prefix_subject_too` and `newsgroup_moderation` are the settings;
/// `usenet_watermark` is what only the gateway writes: the last article
/// number gated from the newsgroup, `None` before the first poll.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct UsenetSettings {
    #[schema(default = false)]
    pub gateway_to_mail: bool,
    #[schema(default = false)]
    pub gateway_to_news: bool,
    /// The newsgroup the list gateways with; empty when there is none.
    #[schema(default = "")]
    pub linked_newsgroup: String,
    /// Keep the list's subject prefix on posts gated to the newsgroup.
    #[schema(default = true)]
    pub nntp_prefix_subject_too: bool,
    #[schema(default = "none")]
    pub newsgroup_moderation: NewsgroupModeration,
    /// Read-only: the last article number gated from the newsgroup.
    #[schema(read_only)]
    pub usenet_watermark: Option<i64>,
}

impl Default for UsenetSettings {
    fn default() -> Self {
        Self {
            gateway_to_mail: false,
            gateway_to_news: false,
            linked_newsgroup: String::new(),
            nntp_prefix_subject_too: true,
            newsgroup_moderation: NewsgroupModeration::None,
            usenet_watermark: None,
        }
    }
}

/// A newsgroup name as Usenet spells it: dot-separated components of
/// letters, digits, `+`, `-` and `_`, at most 255 bytes.
#[must_use]
pub fn is_newsgroup_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name.split('.').all(|component| {
            !component.is_empty()
                && component
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"+-_".contains(&b))
        })
}

/// Mailman's *Member Policy* settings. Serialized as flat compatibility keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct MemberPolicy {
    #[schema(default = "confirm")]
    pub subscription_policy: SubscriptionPolicy,
    #[schema(default = "confirm")]
    pub unsubscription_policy: SubscriptionPolicy,
    #[schema(default = "moderators")]
    pub member_roster_visibility: RosterVisibility,
}

impl Default for MemberPolicy {
    fn default() -> Self {
        Self {
            subscription_policy: SubscriptionPolicy::Confirm,
            unsubscription_policy: SubscriptionPolicy::Confirm,
            member_roster_visibility: RosterVisibility::Moderators,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
// Independent persisted configuration switches, not mutually exclusive states.
#[allow(clippy::struct_excessive_bools)]
pub struct MailingList {
    pub id: ListId,
    pub display_name: String,
    pub description: String,
    pub info: String,
    pub subject_prefix: String,
    pub advertised: bool,
    pub preferred_language: String,
    pub anonymous_list: bool,
    /// Send a private built-in notice only when a Member subscription is added.
    #[serde(default)]
    pub send_welcome_message: bool,
    /// Send a private built-in notice only on actual Member removal.
    #[serde(default)]
    pub send_goodbye_message: bool,
    #[serde(default)]
    pub process_bounces: bool,
    #[serde(default = "default_owner_disable_notice")]
    #[schema(default = true)]
    pub bounce_notify_owner_on_disable: bool,
    #[serde(default)]
    #[schema(default = false)]
    pub bounce_notify_owner_on_bounce_increment: bool,
    #[serde(default = "default_bounce_warnings")]
    #[schema(minimum = 0, maximum = 100, default = 3)]
    pub bounce_you_are_disabled_warnings: u32,
    #[serde(default = "default_bounce_stale_days")]
    #[schema(minimum = 0, maximum = 36500, default = 7)]
    pub bounce_you_are_disabled_warnings_interval: u32,
    #[serde(default = "default_owner_disable_notice")]
    #[schema(default = true)]
    pub bounce_notify_owner_on_removal: bool,
    /// Stale interval in days (1..=3650).
    #[serde(default = "default_bounce_stale_days")]
    #[schema(minimum = 1, maximum = 3650, default = 7)]
    pub bounce_info_stale_after: u32,
    #[serde(default = "default_bounce_threshold")]
    #[schema(exclusive_minimum = 0, maximum = 1_000_000, default = 5)]
    pub bounce_score_threshold: f64,
    #[serde(flatten)]
    pub dmarc: DmarcSettings,
    pub created_at: DateTime<Utc>,
    pub last_post_at: Option<DateTime<Utc>>,
    pub post_id: i64,
    pub volume: i32,
    pub next_digest_number: i64,
    pub digest_last_sent_at: Option<DateTime<Utc>>,
    /// Produce digests at all; off, digest members get nothing.
    #[serde(default = "default_true")]
    #[schema(default = true)]
    pub digests_enabled: bool,
    /// KiB of pending posts that trigger an issue; `0` never does.
    #[serde(default = "default_digest_size_threshold")]
    #[schema(default = 30.0)]
    pub digest_size_threshold: f64,
    /// Send what is pending on the daily periodic run.
    #[serde(default = "default_true")]
    #[schema(default = true)]
    pub digest_send_periodic: bool,
    /// How often the volume number advances and issues restart at 1.
    #[serde(default = "default_digest_volume_frequency")]
    #[schema(default = "monthly")]
    pub digest_volume_frequency: DigestFrequency,
    pub emergency: bool,
    /// Maximum original post size in KiB; zero disables this per-list limit.
    #[serde(default)]
    pub max_message_size: u32,
    /// Hold at or above this visible To/Cc mailbox count; zero disables the check.
    #[serde(default)]
    pub max_num_recipients: u32,
    pub archive_policy: ArchivePolicy,
    pub archive_rendering_mode: ArchiveRenderingMode,
    pub style_name: String,
    #[serde(default)]
    pub default_member_action: Option<ModerationAction>,
    #[serde(default)]
    pub default_nonmember_action: Option<ModerationAction>,
    /// Hold short posts that look like email commands.
    #[serde(default = "default_true")]
    #[schema(default = true)]
    pub administrivia: bool,
    /// Hold posts whose visible To/Cc names neither the list nor an alias.
    #[serde(default = "default_true")]
    #[schema(default = true)]
    pub require_explicit_destination: bool,
    /// Exact addresses or `^`-anchored regexes that count as explicit destinations.
    #[serde(default)]
    pub acceptable_aliases: Vec<String>,
    /// Legacy nonmember action lists; exact addresses or `^`-anchored regexes.
    #[serde(default)]
    pub accept_these_nonmembers: Vec<String>,
    #[serde(default)]
    pub hold_these_nonmembers: Vec<String>,
    #[serde(default)]
    pub reject_these_nonmembers: Vec<String>,
    #[serde(default)]
    pub discard_these_nonmembers: Vec<String>,
    /// Name of the handler pipeline an accepted post runs.
    #[serde(default = "default_posting_pipeline")]
    #[schema(default = "default-posting-pipeline")]
    pub posting_pipeline: String,
    /// Tell the poster when their post is held for moderation.
    #[serde(default = "default_true")]
    #[schema(default = true)]
    pub respond_to_post_requests: bool,
    /// Tell owners and moderators immediately when a post is held.
    #[serde(default = "default_true")]
    #[schema(default = true)]
    pub admin_immed_notify: bool,
    /// Tell owners and moderators when a member subscribes or unsubscribes.
    #[serde(default)]
    #[schema(default = false)]
    pub admin_notify_mchanges: bool,
    #[serde(flatten)]
    pub automatic_responses: AutomaticResponses,
    #[serde(flatten)]
    pub alter_messages: AlterMessages,
    #[serde(flatten)]
    pub member_policy: MemberPolicy,
    #[serde(flatten)]
    pub usenet: UsenetSettings,
    /// Where bounces the detectors cannot attribute to a member are forwarded.
    #[serde(default = "default_unrecognized_bounces")]
    #[schema(default = "administrators")]
    pub forward_unrecognized_bounces_to: UnrecognizedBounceDisposition,
    /// Run the topic matcher and add `X-Topics` to matching posts.
    #[serde(default)]
    pub topics_enabled: bool,
    /// Body lines scanned for `Subject:`/`Keywords:` pseudo-headers: negative
    /// scans every leading header-like line, zero scans none.
    #[serde(default = "default_topics_bodylines_limit")]
    #[schema(default = 5)]
    pub topics_bodylines_limit: i32,
    #[serde(default)]
    pub topics: Vec<Topic>,
}

const fn default_topics_bodylines_limit() -> i32 {
    5
}

const fn default_digest_size_threshold() -> f64 {
    30.0
}
const fn default_digest_volume_frequency() -> DigestFrequency {
    DigestFrequency::Monthly
}
const fn default_unrecognized_bounces() -> UnrecognizedBounceDisposition {
    UnrecognizedBounceDisposition::Administrators
}

fn default_posting_pipeline() -> String {
    "default-posting-pipeline".into()
}

const fn default_true() -> bool {
    true
}

const fn default_bounce_warnings() -> u32 {
    3
}

const fn default_owner_disable_notice() -> bool {
    true
}

const fn default_bounce_threshold() -> f64 {
    5.0
}

const fn default_bounce_stale_days() -> u32 {
    7
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
            send_welcome_message: false,
            send_goodbye_message: false,
            process_bounces: false,
            bounce_notify_owner_on_disable: true,
            bounce_notify_owner_on_bounce_increment: false,
            bounce_you_are_disabled_warnings: 3,
            bounce_you_are_disabled_warnings_interval: 7,
            bounce_notify_owner_on_removal: true,
            bounce_info_stale_after: 7,
            bounce_score_threshold: 5.0,
            dmarc: DmarcSettings::default(),
            created_at: Utc::now(),
            last_post_at: None,
            post_id: 1,
            volume: 1,
            next_digest_number: 1,
            digest_last_sent_at: None,
            digests_enabled: true,
            digest_size_threshold: 30.0,
            digest_send_periodic: true,
            digest_volume_frequency: DigestFrequency::Monthly,
            emergency: false,
            max_message_size: 0,
            max_num_recipients: 0,
            archive_policy: ArchivePolicy::Public,
            archive_rendering_mode: ArchiveRenderingMode::Text,
            style_name: "legacy-default".into(),
            default_member_action: None,
            default_nonmember_action: None,
            administrivia: true,
            require_explicit_destination: true,
            acceptable_aliases: Vec::new(),
            accept_these_nonmembers: Vec::new(),
            hold_these_nonmembers: Vec::new(),
            reject_these_nonmembers: Vec::new(),
            discard_these_nonmembers: Vec::new(),
            posting_pipeline: default_posting_pipeline(),
            respond_to_post_requests: true,
            automatic_responses: AutomaticResponses::default(),
            admin_immed_notify: true,
            admin_notify_mchanges: false,
            alter_messages: AlterMessages::default(),
            member_policy: MemberPolicy::default(),
            usenet: UsenetSettings::default(),
            forward_unrecognized_bounces_to: default_unrecognized_bounces(),
            topics_enabled: false,
            topics_bodylines_limit: default_topics_bodylines_limit(),
            topics: Vec::new(),
        }
    }

    #[must_use]
    pub fn fqdn_listname(&self) -> String {
        self.id.posting_address()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
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
    #[serde(default)]
    #[schema(read_only)]
    pub bounce_score: f64,
    #[serde(default)]
    #[schema(read_only)]
    pub last_bounce_received: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

pub trait ListStyle: fmt::Debug + Send + Sync {
    fn name(&self) -> &'static str;
    /// Mailman's one-line description of the style, as `/lists/styles`
    /// reports it.
    fn description(&self) -> &'static str;
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
    fn description(&self) -> &'static str {
        match self.name {
            "legacy-announce" => "Announce only mailing list style.",
            "private-default" => "Discussion mailing list style with private archives.",
            _ => "Ordinary discussion mailing list style.",
        }
    }
    fn apply(&self, list: &mut MailingList) {
        list.style_name = self.name.into();
        match self.name {
            "legacy-announce" => {
                list.subject_prefix = format!("[{}] ", list.id.list_name());
                list.default_member_action = Some(ModerationAction::Hold);
                list.default_nonmember_action = Some(ModerationAction::Hold);
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
    pub antispam: AntispamConfig,
    pub archive: ArchiveConfig,
    pub runners: RunnerConfig,
    pub nntp: NntpConfig,
    pub webhooks: WebhooksConfig,
    pub observability: ObservabilityConfig,
}

fn validate_rate_limit(key: &str, spec: &str) -> Result<()> {
    let Some((count, window)) = spec.split_once('/') else {
        return Err(Error::Validation(format!(
            "security.rate_limit.{key} must use COUNT/WINDOW"
        )));
    };
    if count
        .parse::<u32>()
        .ok()
        .filter(|count| *count > 0)
        .is_none()
    {
        return Err(Error::Validation(format!(
            "security.rate_limit.{key} count must be a positive integer"
        )));
    }
    if !matches!(
        window,
        "s" | "sec" | "second" | "m" | "min" | "minute" | "h" | "hour" | "d" | "day"
    ) {
        return Err(Error::Validation(format!(
            "security.rate_limit.{key} has an unsupported window"
        )));
    }
    Ok(())
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
            .extract()
            .map_err(|_| Error::Validation("invalid configuration (values redacted)".into()))?;
        if let Some(secret_file) = &config.database.url_file {
            config.database.url = read_secret_file(secret_file, "database.url_file")?;
        }
        for (index, check) in config.antispam.header_checks.iter().enumerate() {
            let header = check.header.trim();
            if header.is_empty() || !header.bytes().all(|b| b.is_ascii_graphic() && b != b':') {
                return Err(Error::Validation(format!(
                    "antispam.header_checks[{index}].header must be a single ASCII field name"
                )));
            }
            if check.pattern.is_empty()
                || regex::RegexBuilder::new(&check.pattern)
                    .case_insensitive(true)
                    .size_limit(1 << 20)
                    .build()
                    .is_err()
            {
                return Err(Error::Validation(format!(
                    "antispam.header_checks[{index}].pattern is not a valid regular expression"
                )));
            }
        }
        if !matches!(
            config.antispam.jump_chain.as_str(),
            "accept" | "hold" | "reject" | "discard"
        ) {
            return Err(Error::Validation(
                "antispam.jump_chain must be one of accept, hold, reject, discard".into(),
            ));
        }
        for role in &config.security.require_2fa_for {
            if role != "server_owner" {
                return Err(Error::Validation(format!(
                    "security.require_2fa_for: unknown role {role:?}; only \"server_owner\" is enforced"
                )));
            }
        }
        validate_oidc(&mut config.web.oidc)?;
        validate_rate_limit("login", &config.security.rate_limit.login)?;
        validate_rate_limit("subscribe", &config.security.rate_limit.subscribe)?;
        validate_rate_limit("api", &config.security.rate_limit.api)?;
        if let Some(spec) = &config.security.rate_limit.api_pre_auth {
            validate_rate_limit("api_pre_auth", spec)?;
        }
        config.mta.validate()?;
        config.mailman.validate()?;
        config.nntp.validate()?;
        config.webhooks.resolve()?;
        config.archive.archivers.resolve()?;
        config.web.tls.validate(&config.web.listen)?;
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
        if let Some(webhooks) = value
            .get_mut("webhooks")
            .and_then(serde_json::Value::as_object_mut)
        {
            webhooks.remove("signing_key_file");
        }
        if let Some(archivers) = value
            .get_mut("archive")
            .and_then(|archive| archive.get_mut("archivers"))
            .and_then(serde_json::Value::as_object_mut)
        {
            archivers.remove("hyperkitty_api_key_file");
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

/// A secret read from a file only the service user can read; the path and
/// the value never appear in an error.
fn read_secret_file(secret_file: &Path, setting: &str) -> Result<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(secret_file)
            .map_err(|_| Error::Validation(format!("cannot inspect {setting}")))?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            return Err(Error::Validation(format!(
                "{setting} must not be accessible by group or other users"
            )));
        }
    }
    let value = std::fs::read_to_string(secret_file)
        .map_err(|_| Error::Validation(format!("cannot read {setting}")))?;
    let value = value.trim();
    if value.is_empty() {
        return Err(Error::Validation(format!("{setting} is empty")));
    }
    Ok(value.to_owned())
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
// `enabled` is the opt-in switch for LMTP + processing + outbound (default off).
// Enabled roles admit only explicit trusted plaintext or verified required STARTTLS.
// CA file and server identity are validated when constructing the runtime mail role;
// disabled web-only configs retain the unsupported opportunistic default without sending.
/// Operator-controlled identity and file reference, never private key bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DkimSigningConfig {
    pub domain: String,
    pub selector: String,
    pub private_key_file: PathBuf,
}

/// A credential read from configuration (SMTP or an OIDC client secret);
/// never exposes its value through Debug or serialization.
#[derive(Clone, Deserialize)]
#[serde(transparent)]
pub struct SmtpAuthSecret(String);
impl SmtpAuthSecret {
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl From<String> for SmtpAuthSecret {
    fn from(value: String) -> Self {
        Self(value)
    }
}
impl From<&str> for SmtpAuthSecret {
    fn from(value: &str) -> Self {
        Self(value.into())
    }
}
impl fmt::Debug for SmtpAuthSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}
impl Serialize for SmtpAuthSecret {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str("[REDACTED]")
    }
}

// Mailman's `[ARC]`: seal every delivered post with the site's key so a
// receiver can trust the authentication results this site recorded before
// the list changed the message. Off by default; needs
// `[mta] authenticity_checks`, which produces the results a seal carries.
config_struct!(ArcConfig {
    enabled: bool = false,
    // The sealing domain (`d=`) and selector (`s=`); the public key is
    // published at `<selector>._domainkey.<domain>` as for DKIM.
    domain: String = String::new(),
    selector: String = String::new(),
    private_key_file: Option<PathBuf> = None
});

config_struct!(MtaConfig {
    enabled: bool = false,
    arc: ArcConfig = ArcConfig::default(),
    bounce_maintenance_enabled: bool = false,
    bounce_maintenance_interval_secs: u32 = 60,
    bounce_maintenance_batch_size: u32 = 100,
    dkim_signing: Vec<DkimSigningConfig> = Vec::new(),
    local_hostname: String = "listmngr.invalid".into(),
    incoming: String = "none".into(),
    lmtp_listen: String = "127.0.0.1:8024".into(),
    // Experimental: a second listener that speaks SMTP straight from the
    // network for the lists' addresses — no relay, no AUTH, no STARTTLS —
    // for a host with no MTA in front. Off by default.
    inbound_smtp_listen: Option<String> = None,
    smtp_relay: String = "127.0.0.1:25".into(),
    smtp_single_recipient: bool = false,
    dsn_issuance_enabled: bool = false,
    dsn_key_file: Option<String> = None,
    dsn_key_id: String = String::new(),
    dsn_ttl_secs: u32 = 604_800,
    smtp_tls: String = "opportunistic".into(),
    smtp_tls_server_name: Option<String> = None,
    smtp_tls_ca_file: Option<String> = None,
    smtp_auth_username: Option<SmtpAuthSecret> = None,
    smtp_auth_password: Option<SmtpAuthSecret> = None,
    smtp_auth_password_file: Option<PathBuf> = None,
    max_recipients: u32 = 500,
    // Mailman's `max_recipients`: recipients per outgoing SMTP transaction.
    max_recipients_per_transaction: u32 = 500,
    // `validate-authenticity`: check SPF, DKIM and DMARC on every post with
    // the system resolver, write `Authentication-Results`, and let
    // `dmarc_mitigate_action` follow the From domain's published policy.
    authenticity_checks: bool = false,
    // Transient delivery failures back off exponentially between these
    // bounds (seconds), with jitter.
    retry_initial_secs: u32 = 10,
    retry_max_secs: u32 = 3600,
    max_message_bytes: u32 = 10_485_760,
    command_timeout_secs: u32 = 30,
    // Mailman's `incoming` MTA: "none", "postfix" or "exim". Lookup maps are
    // published under `map_directory` as `generation-*` directories behind a
    // `current` symlink, at startup and after every list creation/removal.
    map_directory: String = "data/mta".into(),
    // How the MTA reaches the LMTP listener (`host:port`); defaults to
    // `lmtp_listen`, which must then be a concrete address.
    lmtp_map_target: Option<String> = None,
    // Mailman's `transport_file_type`: "regex" (read directly) or "hash"
    // (compiled with `postmap_command`).
    transport_file_type: String = "regex".into(),
    postmap_command: String = "/usr/sbin/postmap".into(),
    // Who may read a generation: "owner", "group" or "world".
    map_permissions: String = "group".into(),
    map_generations_kept: u32 = 5,
    verp_delimiter: String = "+".into(),
    verp_format: String = verp::DEFAULT_FORMAT.into(),
    // Mailman's `verp_personalized_deliveries`: personalized copies use a
    // per-recipient VERP envelope sender.
    verp_personalized_deliveries: bool = false,
    // Mailman's `verp_delivery_interval`: every Nth post of a list is
    // delivered one recipient per transaction with VERP senders; 0 never.
    verp_delivery_interval: u32 = 0
});
impl MtaConfig {
    /// The `[mta]` invariants `Config::load` enforces.
    /// # Errors
    /// Returns the first violated invariant as a validation error.
    pub fn validate(&self) -> Result<()> {
        if let Some(address) = &self.inbound_smtp_listen {
            let parsed: std::net::SocketAddr = address.parse().map_err(|_| {
                Error::Validation("mta.inbound_smtp_listen must be an address:port".into())
            })?;
            if self.lmtp_listen.parse::<std::net::SocketAddr>().ok() == Some(parsed) {
                return Err(Error::Validation(
                    "mta.inbound_smtp_listen must differ from mta.lmtp_listen".into(),
                ));
            }
        }
        if self.arc.enabled {
            if !self.authenticity_checks {
                return Err(Error::Validation(
                    "mta.arc.enabled needs mta.authenticity_checks: a seal carries the results of this site's checks".into(),
                ));
            }
            if self.arc.domain.is_empty()
                || self.arc.selector.is_empty()
                || self.arc.private_key_file.is_none()
            {
                return Err(Error::Validation(
                    "mta.arc needs domain, selector and private_key_file".into(),
                ));
            }
        }
        if self.enabled
            && !matches!(
                self.smtp_tls.as_str(),
                "plaintext_trusted_relay" | "required"
            )
        {
            return Err(Error::Validation(
                "mta.smtp_tls must be \"plaintext_trusted_relay\" or \"required\"; unsupported modes never downgrade".into(),
            ));
        }
        if !(1..=86_400).contains(&self.bounce_maintenance_interval_secs) {
            return Err(Error::Validation(
                "mta.bounce_maintenance_interval_secs must be 1..86400".into(),
            ));
        }
        if !(1..=1000).contains(&self.bounce_maintenance_batch_size) {
            return Err(Error::Validation(
                "mta.bounce_maintenance_batch_size must be 1..1000".into(),
            ));
        }
        if self.bounce_maintenance_enabled && !self.enabled {
            return Err(Error::Validation(
                "mta.bounce_maintenance_enabled requires mta.enabled".into(),
            ));
        }
        self.smtp_auth_credentials()?;
        verp::validate(&self.verp_format, &self.verp_delimiter)?;
        self.validate_maps()?;
        if !(1..=1000).contains(&self.max_recipients_per_transaction) {
            return Err(Error::Validation(
                "mta.max_recipients_per_transaction must be 1..1000".into(),
            ));
        }
        if !(1..=3600).contains(&self.retry_initial_secs)
            || self.retry_max_secs < self.retry_initial_secs
            || self.retry_max_secs > 86_400
        {
            return Err(Error::Validation(
                "mta.retry_initial_secs must be 1..3600 and mta.retry_max_secs between it and 86400".into(),
            ));
        }
        Ok(())
    }

    /// The map-generation vocabulary, whatever `incoming` says, plus the
    /// LMTP target and retention when maps are written at all.
    fn validate_maps(&self) -> Result<()> {
        if !matches!(self.incoming.as_str(), "none" | "postfix" | "exim") {
            return Err(Error::Validation(
                "mta.incoming must be \"none\", \"postfix\" or \"exim\"".into(),
            ));
        }
        if !matches!(self.transport_file_type.as_str(), "regex" | "hash") {
            return Err(Error::Validation(
                "mta.transport_file_type must be \"regex\" or \"hash\"".into(),
            ));
        }
        if !matches!(self.map_permissions.as_str(), "owner" | "group" | "world") {
            return Err(Error::Validation(
                "mta.map_permissions must be \"owner\", \"group\" or \"world\"".into(),
            ));
        }
        if !(1..=100).contains(&self.map_generations_kept) {
            return Err(Error::Validation(
                "mta.map_generations_kept must be 1..100".into(),
            ));
        }
        if self.incoming != "none" && self.map_directory.trim().is_empty() {
            return Err(Error::Validation(
                "mta.map_directory is required when mta.incoming is set".into(),
            ));
        }
        Ok(())
    }

    /// Validate bounded AUTH PLAIN inputs, reading a private regular password file if set.
    /// File bytes are exact except for one optional terminal LF/CRLF. No whitespace trimming.
    /// # Errors
    /// Rejects incomplete credentials, controls, values over 255 UTF-8 bytes, insecure
    /// files and any authentication without REQUIRED TLS, even when the MTA is disabled.
    pub fn smtp_auth_credentials(&self) -> Result<Option<(SmtpAuthSecret, SmtpAuthSecret)>> {
        use std::io::Read as _;
        let invalid = || {
            Error::Validation("invalid SMTP AUTH credentials or password file; both bounded credentials and required TLS are mandatory".into())
        };
        let mut password = self.smtp_auth_password.clone();
        if let Some(path) = &self.smtp_auth_password_file {
            if password.is_some() {
                return Err(invalid());
            }
            let mut options = std::fs::OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
            }
            let file = options.open(path).map_err(|_| invalid())?;
            let metadata = file.metadata().map_err(|_| invalid())?;
            if !metadata.is_file() {
                return Err(invalid());
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                if metadata.permissions().mode() & 0o077 != 0 {
                    return Err(invalid());
                }
            }
            let mut value = String::new();
            file.take(258)
                .read_to_string(&mut value)
                .map_err(|_| invalid())?;
            if value.ends_with('\n') {
                value.pop();
                if value.ends_with('\r') {
                    value.pop();
                }
            }
            password = Some(value.into());
        }
        match (&self.smtp_auth_username, password) {
            (None, None) => Ok(None),
            (Some(user), Some(password))
                if self.smtp_tls == "required"
                    && [user.expose(), password.expose()].iter().all(|value| {
                        !value.is_empty()
                            && value.len() <= 255
                            && !value.chars().any(char::is_control)
                    }) =>
            {
                Ok(Some((user.clone(), password)))
            }
            _ => Err(invalid()),
        }
    }
}

// `[web] tls`: a second listener that speaks TLS, beside the plain one on
// `listen` (which stays for health probes and a reverse proxy on the host).
// The three settings go together; the key file must be the owner's alone.
config_struct!(WebTlsConfig {
    listen: Option<String> = None,
    cert_file: Option<PathBuf> = None,
    key_file: Option<PathBuf> = None
});

impl WebTlsConfig {
    /// Whether a TLS listener is configured at all.
    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.listen.is_some()
    }

    /// The `[web] tls` invariants `Config::load` enforces.
    fn validate(&self, plain_listen: &str) -> Result<()> {
        let set = [
            self.listen.is_some(),
            self.cert_file.is_some(),
            self.key_file.is_some(),
        ];
        if set.iter().all(|value| !value) {
            return Ok(());
        }
        if !set.iter().all(|value| *value) {
            return Err(Error::Validation(
                "web.tls needs listen, cert_file and key_file together".into(),
            ));
        }
        let listen: std::net::SocketAddr = self
            .listen
            .as_deref()
            .unwrap_or_default()
            .parse()
            .map_err(|_| Error::Validation("web.tls.listen must be an address:port".into()))?;
        if plain_listen.parse::<std::net::SocketAddr>().ok() == Some(listen) {
            return Err(Error::Validation(
                "web.tls.listen must differ from web.listen".into(),
            ));
        }
        for (setting, path) in [
            ("web.tls.cert_file", &self.cert_file),
            ("web.tls.key_file", &self.key_file),
        ] {
            let path = path.as_deref().unwrap_or_else(|| Path::new(""));
            let metadata = std::fs::metadata(path)
                .map_err(|_| Error::Validation(format!("cannot read {setting}")))?;
            if !metadata.is_file() {
                return Err(Error::Validation(format!("{setting} is not a file")));
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(self.key_file.as_deref().unwrap_or_else(|| Path::new("")))
                .map_err(|_| Error::Validation("cannot read web.tls.key_file".into()))?
                .permissions()
                .mode();
            if mode & 0o077 != 0 {
                return Err(Error::Validation(
                    "web.tls.key_file must not be accessible by group or other users".into(),
                ));
            }
        }
        Ok(())
    }
}

config_struct!(WebConfig { listen: String = "127.0.0.1:8000".into(), trusted_proxies: Vec<IpNet> = vec!["127.0.0.1/32".parse().expect("valid network")], session_idle: String = "12h".into(), session_absolute: String = "7d".into(), signup: bool = true, oidc: Vec<OidcProviderConfig> = Vec::new(), tls: WebTlsConfig = WebTlsConfig::default() });

/// `[[web.oidc]]`: unique slugs, an https issuer (http only on loopback),
/// exactly one way to the client secret, and `openid` among the scopes.
fn validate_oidc(providers: &mut [OidcProviderConfig]) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    for provider in providers.iter_mut() {
        let name = provider.name.as_str();
        if name.is_empty()
            || name.len() > 32
            || !name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || !seen.insert(name.to_owned())
        {
            return Err(Error::Validation(
                "web.oidc[].name must be a unique lowercase slug".into(),
            ));
        }
        if provider.display_name.trim().is_empty() {
            return Err(Error::Validation(format!(
                "web.oidc.{name}.display_name is empty"
            )));
        }
        let issuer = provider.issuer.trim_end_matches('/');
        let loopback = issuer.starts_with("http://localhost")
            || issuer.starts_with("http://127.0.0.1")
            || issuer.starts_with("http://[::1]");
        if !(issuer.starts_with("https://") || loopback) {
            return Err(Error::Validation(format!(
                "web.oidc.{name}.issuer must be an https URL (http only for loopback development)"
            )));
        }
        provider.issuer = issuer.to_owned();
        if provider.client_id.trim().is_empty() {
            return Err(Error::Validation(format!(
                "web.oidc.{name}.client_id is empty"
            )));
        }
        match (&provider.client_secret, &provider.client_secret_file) {
            (Some(secret), None) if !secret.expose().trim().is_empty() => {}
            (None, Some(file)) => {
                let value = read_secret_file(file, &format!("web.oidc.{name}.client_secret_file"))?;
                provider.client_secret = Some(value.into());
            }
            _ => {
                return Err(Error::Validation(format!(
                    "web.oidc.{name} needs exactly one of client_secret or client_secret_file"
                )));
            }
        }
        if !provider.scopes.iter().any(|scope| scope == "openid") {
            provider.scopes.insert(0, "openid".into());
        }
    }
    Ok(())
}

/// One `OpenID` Connect provider a reader may sign in with (`[[web.oidc]]`).
///
/// Discovery, Authorization Code with PKCE, `state` and `nonce`, and an ID
/// token whose `email_verified` claim is required before an address is
/// trusted. Any conforming provider works; Google is one (`issuer =
/// "https://accounts.google.com"`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OidcProviderConfig {
    /// Slug in URLs: lowercase letters, digits and hyphens.
    pub name: String,
    /// What the login page calls it.
    pub display_name: String,
    /// The issuer URL discovery is fetched from and ID tokens must name.
    pub issuer: String,
    /// The client id registered with the provider.
    pub client_id: String,
    /// The client secret inline (development only); prefer `client_secret_file`.
    /// Redacted in `Debug` and when serialized.
    pub client_secret: Option<SmtpAuthSecret>,
    /// A file holding the client secret, readable by the service user only.
    pub client_secret_file: Option<PathBuf>,
    /// Scopes to request; `openid` is always included.
    pub scopes: Vec<String>,
}

impl Default for OidcProviderConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            display_name: String::new(),
            issuer: String::new(),
            client_id: String::new(),
            client_secret: None,
            client_secret_file: None,
            scopes: vec!["openid".into(), "email".into(), "profile".into()],
        }
    }
}
config_struct!(ApiConfig { listen: String = "127.0.0.1:8001".into(), compat_basic_auth: bool = false, compat_basic_auth_allow: Vec<IpNet> = vec!["127.0.0.1/32".parse().expect("valid network")] });
config_struct!(Argon2Config {
    memory_kib: u32 = 65_536,
    iterations: u32 = 3,
    parallelism: u32 = 1
});
config_struct!(RateLimitConfig {
    login: String = "5/min".into(),
    subscribe: String = "10/hour".into(),
    api: String = "600/min".into(),
    api_pre_auth: Option<String> = None
});
config_struct!(SecurityConfig { argon2: Argon2Config = Argon2Config::default(), password_min_score: u8 = 3, require_2fa_for: Vec<String> = vec!["server_owner".into()], pending_request_life: String = "3d".into(), rate_limit: RateLimitConfig = RateLimitConfig::default() });
config_struct!(MailmanConfig {
    default_member_action: ModerationAction = ModerationAction::Defer,
    default_nonmember_action: ModerationAction = ModerationAction::Hold,
    noreply_address: String = "noreply".into(),
    site_owner_notify: bool = true,
    // Mailman's `filtered_messages_are_preservable`: whether a list's
    // `filter_action = preserve` keeps a copy in the shunt store (else it
    // behaves as discard).
    filtered_messages_are_preservable: bool = false,
    // Mailman's probe step: a member at the bounce threshold is sent a probe
    // from a one-time bounce address and disabled only when it bounces.
    // Off, the threshold disables delivery at once.
    bounce_probes: bool = true,
    // How long a probe's bounce is honoured (seconds; Mailman keeps them
    // with its other pended requests).
    bounce_probe_lifetime_secs: u32 = 604_800,
    // Mailman's `run_tasks_every`: how often the mail role runs the task
    // sweep (expired confirmations and probes, finished queue jobs, stale
    // bounce scores).
    run_tasks_every_secs: u32 = 3600,
    // How long a finished queue job and its message stay for inspection
    // before the sweep collects them.
    finished_job_retention_secs: u32 = 604_800
});
impl MailmanConfig {
    fn validate(&self) -> Result<()> {
        if !(60..=604_800).contains(&self.run_tasks_every_secs) {
            return Err(Error::Validation(
                "mailman.run_tasks_every_secs must be 60..604800".into(),
            ));
        }
        if !(3600..=31_536_000).contains(&self.finished_job_retention_secs) {
            return Err(Error::Validation(
                "mailman.finished_job_retention_secs must be 3600..31536000".into(),
            ));
        }
        Ok(())
    }
}
config_struct!(HeaderCheck {
    header: String = String::new(),
    pattern: String = String::new()
});
// Site-wide header rules, evaluated by the `suspicious-header` posting rule,
// and the default chain for per-list header rules that name none.
config_struct!(AntispamConfig {
    header_checks: Vec<HeaderCheck> = Vec::new(),
    jump_chain: String = "hold".into()
});
// The remote archivers a list can switch on (`list_archivers`). Each is
// off until the operator configures it here: an empty address, command or
// path leaves that archiver off however a list is configured. The command
// is an argument vector, never a shell line, so nothing in a message can
// become a shell word.
config_struct!(ArchiversConfig {
    mail_archive_address: String = String::new(),
    mhonarc_command: Vec<String> = Vec::new(),
    prototype_path: String = String::new(),
    // A HyperKitty to post each archived post to, as `mailman-hyperkitty`
    // does: its base URL (`https://lists.example.com/hyperkitty`) and its
    // `MAILMAN_ARCHIVER_KEY`. Empty leaves the `hyperkitty` archiver off.
    hyperkitty_url: String = String::new(),
    hyperkitty_api_key: Option<SmtpAuthSecret> = None,
    hyperkitty_api_key_file: Option<PathBuf> = None
});

impl ArchiversConfig {
    /// Read `hyperkitty_api_key_file` into `hyperkitty_api_key` and check
    /// the pair.
    fn resolve(&mut self) -> Result<()> {
        if let Some(path) = &self.hyperkitty_api_key_file {
            if self.hyperkitty_api_key.is_some() {
                return Err(Error::Validation(
                    "archive.archivers.hyperkitty_api_key and hyperkitty_api_key_file are exclusive".into(),
                ));
            }
            self.hyperkitty_api_key =
                Some(read_secret_file(path, "archive.archivers.hyperkitty_api_key_file")?.into());
        }
        let url = self.hyperkitty_url.trim();
        if url.is_empty() {
            if self.hyperkitty_api_key.is_some() {
                return Err(Error::Validation(
                    "archive.archivers.hyperkitty_api_key needs hyperkitty_url".into(),
                ));
            }
            return Ok(());
        }
        if !(url.starts_with("https://") || url.starts_with("http://"))
            || url.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(Error::Validation(
                "archive.archivers.hyperkitty_url must be an http:// or https:// URL".into(),
            ));
        }
        if self.hyperkitty_api_key.is_none() {
            return Err(Error::Validation(
                "archive.archivers.hyperkitty_url needs hyperkitty_api_key or hyperkitty_api_key_file".into(),
            ));
        }
        Ok(())
    }

    /// The `HyperKitty` archiver key, when one is configured.
    #[must_use]
    pub fn hyperkitty_api_key(&self) -> Option<&str> {
        self.hyperkitty_api_key.as_ref().map(SmtpAuthSecret::expose)
    }
}
config_struct!(ArchiveConfig {
    enabled: bool = true,
    archivers: ArchiversConfig = ArchiversConfig::default(),
    index_path: String = "data/index".into(),
    default_policy: ArchivePolicy = ArchivePolicy::Public,
    // Opt-in avatars: the archive shows a Gravatar for each sender, fetched
    // through this server (`/web/gravatar/<sha256>`), never by the browser.
    gravatar: bool = false,
    gravatar_url: String = "https://www.gravatar.com/avatar/".into()
});
config_struct!(RunnerConfig {
    lock_timeout: String = "10m".into()
});
// Mailman's `[nntp]`: the news server the `nntp` runner posts gated lists'
// traffic to, and how a post is trimmed for it. Empty `host` leaves the
// gateway off: `to-usenet` still queues nothing for a list that does not
// gateway, and a queued post waits for a host.
config_struct!(NntpConfig {
    host: String = String::new(),
    port: u16 = 119,
    // Mailman's `gatenews_every`: how often the `nntp` runner polls the
    // newsgroups of lists that gateway to mail; 0 leaves polling to
    // `listmngr nntp gate`.
    gatenews_every_secs: u32 = 300,
    user: Option<String> = None,
    password: Option<SmtpAuthSecret> = None,
    password_file: Option<PathBuf> = None,
    // Headers removed from every post before it is offered to the news
    // server (Mailman's `remove_headers`).
    remove_headers: Vec<String> = vec![
        "nntp-posting-host".into(),
        "nntp-posting-date".into(),
        "x-trace".into(),
        "x-complaints-to".into(),
        "xref".into(),
        "date-received".into(),
        "posted".into(),
        "posting-version".into(),
        "relay-version".into(),
        "received".into()
    ],
    // `"Source Target"` pairs: a header a news server accepts once keeps its
    // first value and hands the rest to the target header (Mailman's
    // `rewrite_duplicate_headers`).
    rewrite_duplicate_headers: Vec<String> = vec![
        "To X-Original-To".into(),
        "CC X-Original-CC".into(),
        "Content-Transfer-Encoding X-Original-Content-Transfer-Encoding".into(),
        "MIME-Version X-MIME-Version".into()
    ]
});
// Webhooks (`P6-WEBHOOKS-*`): where the site posts what its audit log
// records. Off by default; on, a signing key is required, since each
// webhook's secret is derived from it and the webhook's own salt, so the
// database holds no secret to leak.
config_struct!(WebhooksConfig {
    enabled: bool = false,
    signing_key: Option<SmtpAuthSecret> = None,
    signing_key_file: Option<PathBuf> = None,
    // Accept `http://` targets as well as `https://` (a lab, never a site).
    allow_http: bool = false,
    // Deliver to loopback, link-local and private addresses (a lab).
    allow_private_targets: bool = false,
    // Attempts before a delivery is given up as failed.
    max_attempts: u32 = 12,
    // Seconds a target has to answer one delivery.
    timeout_secs: u32 = 10
});

impl WebhooksConfig {
    /// Read `signing_key_file` into `signing_key` and check the section.
    fn resolve(&mut self) -> Result<()> {
        if let Some(path) = &self.signing_key_file {
            if self.signing_key.is_some() {
                return Err(Error::Validation(
                    "webhooks.signing_key and webhooks.signing_key_file are exclusive".into(),
                ));
            }
            self.signing_key = Some(read_secret_file(path, "webhooks.signing_key_file")?.into());
        }
        if let Some(key) = &self.signing_key
            && key.expose().len() < 32
        {
            return Err(Error::Validation(
                "webhooks.signing_key must be at least 32 characters".into(),
            ));
        }
        if self.enabled && self.signing_key.is_none() {
            return Err(Error::Validation(
                "webhooks.enabled needs webhooks.signing_key or webhooks.signing_key_file".into(),
            ));
        }
        if self.max_attempts == 0 || self.timeout_secs == 0 {
            return Err(Error::Validation(
                "webhooks.max_attempts and webhooks.timeout_secs must be positive".into(),
            ));
        }
        Ok(())
    }

    /// The signing key, when one is configured.
    #[must_use]
    pub fn signing_key(&self) -> Option<&str> {
        self.signing_key.as_ref().map(SmtpAuthSecret::expose)
    }
}

impl NntpConfig {
    /// Whether a news server is configured at all.
    #[must_use]
    pub fn enabled(&self) -> bool {
        !self.host.trim().is_empty()
    }

    /// The `[nntp]` invariants `Config::load` enforces.
    /// # Errors
    /// Returns the first violated invariant as a validation error.
    pub fn validate(&self) -> Result<()> {
        if self.port == 0 {
            return Err(Error::Validation("nntp.port must be 1..65535".into()));
        }
        if self.password.is_some() && self.password_file.is_some() {
            return Err(Error::Validation(
                "nntp.password and nntp.password_file are exclusive".into(),
            ));
        }
        if (self.password.is_some() || self.password_file.is_some()) && self.user.is_none() {
            return Err(Error::Validation("nntp.password needs nntp.user".into()));
        }
        for pair in &self.rewrite_duplicate_headers {
            let mut words = pair.split_whitespace();
            match (words.next(), words.next(), words.next()) {
                (Some(source), Some(target), None)
                    if is_header_name(source) && is_header_name(target) => {}
                _ => {
                    return Err(Error::Validation(format!(
                        "nntp.rewrite_duplicate_headers: {pair:?} is not \"Source Target\""
                    )));
                }
            }
        }
        if let Some(name) = self
            .remove_headers
            .iter()
            .find(|name| !is_header_name(name))
        {
            return Err(Error::Validation(format!(
                "nntp.remove_headers: {name:?} is not a header name"
            )));
        }
        Ok(())
    }

    /// The credentials for `AUTHINFO`, the password read from its file when
    /// configured that way (never at configuration display).
    /// # Errors
    /// Returns a validation error for an unreadable, empty or exposed file.
    pub fn credentials(&self) -> Result<Option<(String, SmtpAuthSecret)>> {
        let Some(user) = &self.user else {
            return Ok(None);
        };
        let password = match (&self.password, &self.password_file) {
            (Some(password), _) => password.clone(),
            (None, Some(path)) => read_secret_file(path, "nntp.password_file")?.into(),
            (None, None) => return Err(Error::Validation("nntp.user needs a password".into())),
        };
        Ok(Some((user.clone(), password)))
    }

    /// The header rewrites as (source, target) pairs: the source lowercased
    /// for matching, the target as configured for writing.
    #[must_use]
    pub fn duplicate_rewrites(&self) -> Vec<(String, String)> {
        self.rewrite_duplicate_headers
            .iter()
            .filter_map(|pair| {
                let mut words = pair.split_whitespace();
                Some((words.next()?.to_ascii_lowercase(), words.next()?.to_owned()))
            })
            .collect()
    }
}

/// RFC 5322 `field-name`: printable US-ASCII without the colon.
#[must_use]
pub fn is_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 78
        && name.bytes().all(|b| (33..=126).contains(&b) && b != b':')
}
config_struct!(ObservabilityConfig {
    log: String = "info".into(),
    metrics: bool = true
});
