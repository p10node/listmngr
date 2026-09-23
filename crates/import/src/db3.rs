//! A Mailman 3 site read straight from the core's own database and
//! message store, with no core running.
//!
//! The result is the same `Site` the REST client gives, so the plan and
//! the apply are shared. What the REST layer spells out, the tables keep
//! as Mailman's storage: enums as the integers of `mailman.interfaces`,
//! the three intervals as a datetime counted from the epoch (`SQLite`) or
//! a real interval (`PostgreSQL`) — both asked for in seconds — the nonmember and DMARC lists as pickled
//! `MutableList`s, the pending subscriptions and the held messages'
//! metadata as JSON key/values under a token, and each held message as a
//! pickled `email.message.Message` under `var/messages`.
use crate::import3::{
    Address3, Domain3, Hold3, List3, Member3, Request3, Site, User3, milliseconds_of,
};
use crate::pickle::{self, Item};
use crate::{Error, Result};
use listmngr_core::{ListId, MemberRole, ModerationAction, Preferences};
use listmngr_db::header_matches::HeaderMatchRow;
use serde_json::{Map, Value as Json, json};
use sqlx::any::{AnyPoolOptions, AnyRow};
use sqlx::{AnyPool, Row};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Once;

static DRIVERS: Once = Once::new();

/// The languages this site's catalog has.
const LANGUAGES: [&str; 2] = ["en", "vi"];

/// Which SQL the core's database speaks.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Backend {
    Sqlite,
    Postgres,
}

/// Read the whole site from `url` (`sqlite:///path/to/mailman.db` or
/// `postgres://…`, Mailman's `postgresql://` accepted), the held messages
/// from `var_dir/messages` when the directory is given.
///
/// # Errors
/// `Database` when the database cannot be opened or a query fails, and
/// `Core` for a list identifier this site cannot parse.
pub async fn fetch_db(url: &str, var_dir: Option<&Path>) -> Result<Site> {
    DRIVERS.call_once(sqlx::any::install_default_drivers);
    let url = url.replacen("postgresql://", "postgres://", 1);
    let backend = if url.starts_with("sqlite") {
        Backend::Sqlite
    } else {
        Backend::Postgres
    };
    let pool = AnyPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .map_err(|error| Error::Database(scrub(&error.to_string())))?;
    let reader = Reader {
        pool,
        backend,
        var_dir,
    };
    let site = reader.site().await;
    reader.pool.close().await;
    site
}

/// A message from the store (a pickled `email.message.Message`) as the
/// bytes Python's generator would write for it.
///
/// # Errors
/// `Pickle` when the bytes are not such a pickle.
pub fn render_message(pickled: &[u8]) -> Result<Vec<u8>> {
    let item = pickle::read(pickled)?;
    let mut out = Vec::new();
    render_into(&item, &mut out)?;
    Ok(out)
}

fn render_into(item: &Item, out: &mut Vec<u8>) -> Result<()> {
    let Item::Instance(state) = item else {
        return Err(Error::Pickle("not a pickled message".into()));
    };
    let Item::Dict(state) = state.as_ref() else {
        return Err(Error::Pickle("a message without its state".into()));
    };
    let field = |name: &str| {
        state
            .iter()
            .find(|(key, _)| matches!(key, Item::Text(text) if text == name))
            .map(|(_, value)| value)
    };
    let mut boundary = None;
    if let Some(Item::List(headers)) = field("_headers") {
        for header in headers {
            let (Item::Tuple(pair) | Item::List(pair)) = header else {
                continue;
            };
            let (Some(Item::Text(name)), Some(value)) = (pair.first(), pair.get(1)) else {
                continue;
            };
            let value = header_text(value);
            if name.eq_ignore_ascii_case("content-type") {
                boundary = boundary_of(&value);
            }
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(b": ");
            out.extend_from_slice(value.as_bytes());
            out.push(b'\n');
        }
    }
    out.push(b'\n');
    match field("_payload") {
        Some(Item::Text(text)) => out.extend_from_slice(text.as_bytes()),
        Some(Item::Bytes(bytes)) => out.extend_from_slice(bytes),
        Some(Item::List(parts)) => {
            let boundary = boundary.unwrap_or_default();
            if let Some(Item::Text(preamble)) = field("preamble") {
                out.extend_from_slice(preamble.as_bytes());
                out.push(b'\n');
            }
            for part in parts {
                out.extend_from_slice(b"--");
                out.extend_from_slice(boundary.as_bytes());
                out.push(b'\n');
                render_into(part, out)?;
                if out.last() != Some(&b'\n') {
                    out.push(b'\n');
                }
            }
            out.extend_from_slice(b"--");
            out.extend_from_slice(boundary.as_bytes());
            out.extend_from_slice(b"--\n");
            if let Some(Item::Text(epilogue)) = field("epilogue") {
                out.extend_from_slice(epilogue.as_bytes());
            }
        }
        _ => {}
    }
    Ok(())
}

/// A header value: text, or an `email.header.Header` instance whose
/// chunks are joined.
fn header_text(value: &Item) -> String {
    match value {
        Item::Text(text) => text.clone(),
        Item::Instance(state) => {
            let Item::Dict(entries) = state.as_ref() else {
                return String::new();
            };
            entries
                .iter()
                .find(|(key, _)| matches!(key, Item::Text(text) if text == "_chunks"))
                .and_then(|(_, chunks)| match chunks {
                    Item::List(chunks) => Some(
                        chunks
                            .iter()
                            .filter_map(|chunk| match chunk {
                                Item::Tuple(pair) | Item::List(pair) => match pair.first() {
                                    Some(Item::Text(text)) => Some(text.as_str()),
                                    _ => None,
                                },
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join(" "),
                    ),
                    _ => None,
                })
                .unwrap_or_default()
        }
        _ => String::new(),
    }
}

/// The `boundary` parameter of a Content-Type value.
fn boundary_of(content_type: &str) -> Option<String> {
    content_type.split(';').skip(1).find_map(|parameter| {
        let (name, value) = parameter.trim().split_once('=')?;
        if !name.trim().eq_ignore_ascii_case("boundary") {
            return None;
        }
        Some(value.trim().trim_matches('"').to_owned())
    })
}

/// A database error message with any URL userinfo replaced.
pub(crate) fn scrub(message: &str) -> String {
    message
        .split_whitespace()
        .map(|word| {
            let userinfo = word
                .split_once("://")
                .and_then(|(_, rest)| rest.split('/').next())
                .is_some_and(|host| host.contains('@'));
            if userinfo { "<url>" } else { word }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

struct Reader<'a> {
    pool: AnyPool,
    backend: Backend,
    var_dir: Option<&'a Path>,
}

fn db_error(error: &sqlx::Error) -> Error {
    Error::Database(scrub(&error.to_string()))
}

/// A column that may be a real boolean (`PostgreSQL`) or an integer
/// (`SQLite`).
fn flag(row: &AnyRow, column: &str) -> bool {
    row.try_get::<bool, _>(column)
        .or_else(|_| row.try_get::<i64, _>(column).map(|value| value != 0))
        .or_else(|_| row.try_get::<i32, _>(column).map(|value| value != 0))
        .unwrap_or(false)
}

fn int(row: &AnyRow, column: &str) -> Option<i64> {
    row.try_get::<i64, _>(column)
        .or_else(|_| row.try_get::<i32, _>(column).map(i64::from))
        .or_else(|_| row.try_get::<i16, _>(column).map(i64::from))
        .ok()
}

fn float(row: &AnyRow, column: &str) -> Option<f64> {
    row.try_get::<f64, _>(column)
        .or_else(|_| row.try_get::<f32, _>(column).map(f64::from))
        .ok()
        .or_else(|| {
            // Mailman's thresholds are small integers when it wrote them so.
            #[allow(clippy::cast_precision_loss)]
            int(row, column).map(|value| value as f64)
        })
}

fn text(row: &AnyRow, column: &str) -> String {
    row.try_get::<String, _>(column).unwrap_or_default()
}

fn optional_text(row: &AnyRow, column: &str) -> Option<String> {
    row.try_get::<Option<String>, _>(column).ok().flatten()
}

fn bytes(row: &AnyRow, column: &str) -> Option<Vec<u8>> {
    row.try_get::<Option<Vec<u8>>, _>(column).ok().flatten()
}

/// The strings of a pickled `MutableList` column.
fn pickled_list(row: &AnyRow, column: &str) -> Vec<String> {
    let Some(blob) = bytes(row, column) else {
        return Vec::new();
    };
    match pickle::read(&blob) {
        Ok(Item::List(items) | Item::Tuple(items)) => items
            .into_iter()
            .filter_map(|item| match item {
                Item::Text(text) => Some(text),
                Item::Bytes(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// An enum column by Mailman's integer values.
fn named(row: &AnyRow, column: &str, names: &[&str]) -> Option<Json> {
    let value = int(row, column)?;
    usize::try_from(value)
        .ok()
        .and_then(|index| names.get(index))
        .map(|name| json!(name))
}

const ACTION: [&str; 5] = ["hold", "reject", "discard", "accept", "defer"];
const FILTER_ACTION: [&str; 7] = [
    "hold", "reject", "discard", "accept", "defer", "forward", "preserve",
];
const ARCHIVE_POLICY: [&str; 3] = ["never", "private", "public"];
const RENDERING: [&str; 3] = ["", "text", "markdown"];
const BOUNCES_TO: [&str; 3] = ["discard", "administrators", "site_owner"];
const DMARC: [&str; 5] = [
    "no_mitigation",
    "munge_from",
    "wrap_message",
    "reject",
    "discard",
];
const FREQUENCY: [&str; 5] = ["yearly", "monthly", "quarterly", "weekly", "daily"];
const PERSONALIZE: [&str; 3] = ["none", "individual", "full"];
const REPLY: [&str; 4] = [
    "no_munging",
    "point_to_list",
    "explicit_header",
    "explicit_header_only",
];
const POLICY: [&str; 4] = ["open", "confirm", "moderate", "confirm_then_moderate"];
const VISIBILITY: [&str; 3] = ["public", "members", "moderators"];
const NEWS: [&str; 3] = ["none", "open_moderated", "moderated"];
const RESPONSE: [&str; 3] = ["none", "respond_and_discard", "respond_and_continue"];
const ROLE: [&str; 5] = ["", "member", "owner", "moderator", "nonmember"];
const DELIVERY_MODE: [&str; 5] = [
    "",
    "regular",
    "plaintext_digests",
    "mime_digests",
    "summary_digests",
];
const DELIVERY_STATUS: [&str; 6] = [
    "",
    "enabled",
    "by_user",
    "by_bounces",
    "by_moderator",
    "unknown",
];

/// Booleans copied by name, and text copied by name.
const FLAGS: [&str; 26] = [
    "admin_immed_notify",
    "admin_notify_mchanges",
    "administrivia",
    "advertised",
    "allow_list_posts",
    "anonymous_list",
    "bounce_notify_owner_on_bounce_increment",
    "bounce_notify_owner_on_disable",
    "bounce_notify_owner_on_removal",
    "collapse_alternatives",
    "convert_html_to_plaintext",
    "digest_send_periodic",
    "digests_enabled",
    "dmarc_mitigate_unconditionally",
    "emergency",
    "filter_content",
    "first_strip_reply_to",
    "gateway_to_mail",
    "gateway_to_news",
    "include_rfc2369_headers",
    "nntp_prefix_subject_too",
    "process_bounces",
    "respond_to_post_requests",
    "require_explicit_destination",
    "send_goodbye_message",
    "send_welcome_message",
];
const TEXTS: [&str; 13] = [
    "autoresponse_owner_text",
    "autoresponse_postings_text",
    "autoresponse_request_text",
    "description",
    "display_name",
    "dmarc_moderation_notice",
    "dmarc_wrapped_message_text",
    "info",
    "linked_newsgroup",
    "posting_pipeline",
    "preferred_language",
    "reply_to_address",
    "subject_prefix",
];
const COUNTS: [&str; 6] = [
    "bounce_you_are_disabled_warnings",
    "max_days_to_hold",
    "max_message_size",
    "max_num_recipients",
    "next_digest_number",
    "bounce_score_threshold",
];
const ENUMS: [(&str, &[&str]); 15] = [
    ("archive_policy", &ARCHIVE_POLICY),
    ("archive_rendering_mode", &RENDERING),
    ("autorespond_owner", &RESPONSE),
    ("autorespond_postings", &RESPONSE),
    ("autorespond_requests", &RESPONSE),
    ("default_member_action", &ACTION),
    ("default_nonmember_action", &ACTION),
    ("digest_volume_frequency", &FREQUENCY),
    ("dmarc_mitigate_action", &DMARC),
    ("forward_unrecognized_bounces_to", &BOUNCES_TO),
    ("member_roster_visibility", &VISIBILITY),
    ("newsgroup_moderation", &NEWS),
    ("personalize", &PERSONALIZE),
    ("reply_goes_to_list", &REPLY),
    ("subscription_policy", &POLICY),
];
const PICKLED: [&str; 5] = [
    "accept_these_nonmembers",
    "hold_these_nonmembers",
    "reject_these_nonmembers",
    "discard_these_nonmembers",
    "dmarc_addresses",
];
const INTERVALS: [&str; 3] = [
    "autoresponse_grace_period",
    "bounce_info_stale_after",
    "bounce_you_are_disabled_warnings_interval",
];

impl Reader<'_> {
    /// A boolean column as the driver can carry it: `SQLite`'s `BOOLEAN`
    /// affinity is unknown to it, so the value is cast to an integer.
    fn boolean(&self, name: &str) -> String {
        match self.backend {
            Backend::Sqlite => format!("CAST({name} AS INTEGER) AS {name}"),
            Backend::Postgres => name.to_owned(),
        }
    }

    async fn rows(&self, sql: &str) -> Result<Vec<AnyRow>> {
        sqlx::query(sql)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| db_error(&error))
    }

    async fn site(&self) -> Result<Site> {
        let mut site = Site {
            domains: Vec::new(),
            site_bans: Vec::new(),
            users: Vec::new(),
            lists: Vec::new(),
            warnings: Vec::new(),
        };
        for row in self
            .rows("SELECT mail_host, description, alias_domain FROM domain ORDER BY id")
            .await?
        {
            site.domains.push(Domain3 {
                mail_host: text(&row, "mail_host"),
                description: text(&row, "description"),
                alias_domain: optional_text(&row, "alias_domain").filter(|alias| !alias.is_empty()),
            });
        }
        for row in self
            .rows("SELECT email FROM ban WHERE list_id IS NULL ORDER BY email")
            .await?
        {
            site.site_bans.push(text(&row, "email"));
        }
        let preferences = self.preferences().await?;
        let addresses = self.addresses().await?;
        let users = self.users(&preferences, &addresses).await?;
        site.users = users.values().cloned().collect();
        // PostgreSQL keeps the three intervals as intervals, which this
        // driver cannot hand over; ask for them in seconds instead.
        let mut columns: Vec<String> = [
            "id",
            "list_id",
            "mail_host",
            "digest_size_threshold",
            "moderator_password",
        ]
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
        columns.extend(FLAGS.iter().map(|name| self.boolean(name)));
        columns.extend(TEXTS.iter().map(|name| (*name).to_owned()));
        columns.extend(COUNTS.iter().map(|name| (*name).to_owned()));
        columns.extend(ENUMS.iter().map(|(name, _)| (*name).to_owned()));
        columns.extend(["unsubscription_policy", "filter_action"].map(str::to_owned));
        columns.extend(PICKLED.iter().map(|name| (*name).to_owned()));
        columns.extend(INTERVALS.iter().map(|name| match self.backend {
            // A datetime counted from the epoch: its seconds are the interval.
            Backend::Sqlite => format!("CAST(strftime('%s', {name}) AS INTEGER) AS {name}_seconds"),
            Backend::Postgres => format!("EXTRACT(EPOCH FROM {name})::bigint AS {name}_seconds"),
        }));
        let lists = format!(
            "SELECT {} FROM mailinglist ORDER BY list_id",
            columns.join(", ")
        );
        for row in self.rows(&lists).await? {
            let list = self
                .list(&row, &preferences, &addresses, &users, &mut site.warnings)
                .await?;
            site.lists.push(list);
        }
        Ok(site)
    }

    /// Every preferences row, by id.
    async fn preferences(&self) -> Result<BTreeMap<i64, Preferences>> {
        let mut out = BTreeMap::new();
        let query = format!(
            "SELECT id, {}, {}, preferred_language, {}, {}, delivery_mode, delivery_status FROM preferences",
            self.boolean("acknowledge_posts"),
            self.boolean("hide_address"),
            self.boolean("receive_list_copy"),
            self.boolean("receive_own_postings"),
        );
        for row in self.rows(&query).await? {
            let Some(id) = int(&row, "id") else {
                continue;
            };
            let language = optional_text(&row, "preferred_language")
                .filter(|code| LANGUAGES.contains(&code.as_str()));
            out.insert(
                id,
                Preferences {
                    acknowledge_posts: optional_flag(&row, "acknowledge_posts"),
                    hide_address: optional_flag(&row, "hide_address"),
                    preferred_language: language,
                    receive_list_copy: optional_flag(&row, "receive_list_copy"),
                    receive_own_postings: optional_flag(&row, "receive_own_postings"),
                    delivery_mode: named(&row, "delivery_mode", &DELIVERY_MODE)
                        .and_then(|name| name.as_str()?.parse().ok()),
                    delivery_status: named(&row, "delivery_status", &DELIVERY_STATUS)
                        .and_then(|name| name.as_str()?.parse().ok()),
                },
            );
        }
        Ok(out)
    }

    /// Every address row, by id.
    async fn addresses(&self) -> Result<BTreeMap<i64, AddressRow>> {
        let mut out = BTreeMap::new();
        for row in self
            .rows("SELECT id, email, display_name, CASE WHEN verified_on IS NULL THEN 0 ELSE 1 END AS verified, user_id FROM address ORDER BY id")
            .await?
        {
            let Some(id) = int(&row, "id") else {
                continue;
            };
            out.insert(
                id,
                AddressRow {
                    email: text(&row, "email").to_lowercase(),
                    display_name: text(&row, "display_name"),
                    verified: flag(&row, "verified"),
                    user_id: int(&row, "user_id"),
                },
            );
        }
        Ok(out)
    }

    /// Every account, by its row id.
    async fn users(
        &self,
        preferences: &BTreeMap<i64, Preferences>,
        addresses: &BTreeMap<i64, AddressRow>,
    ) -> Result<BTreeMap<i64, User3>> {
        let mut users = BTreeMap::new();
        for row in self
            .rows(&format!(
                "SELECT id, display_name, password, CAST(_user_id AS TEXT) AS _user_id, {}, _preferred_address_id, preferences_id FROM \"user\" ORDER BY id",
                self.boolean("is_server_owner")
            ))
            .await?
        {
            let Some(id) = int(&row, "id") else {
                continue;
            };
            let owned: Vec<Address3> = addresses
                .values()
                .filter(|address| address.user_id == Some(id))
                .map(|address| Address3 {
                    email: address.email.clone(),
                    display_name: address.display_name.clone(),
                    verified: address.verified,
                })
                .collect();
            users.insert(id, User3 {
                // PostgreSQL keeps a UUID and spells it with hyphens; the
                // REST API and SQLite give the 32 hex digits.
                user_id: text(&row, "_user_id").replace('-', ""),
                display_name: text(&row, "display_name"),
                is_server_owner: flag(&row, "is_server_owner"),
                has_password: optional_text(&row, "password").is_some_and(|hash| !hash.is_empty()),
                addresses: owned,
                preferred: int(&row, "_preferred_address_id")
                    .and_then(|address| addresses.get(&address))
                    .map(|address| address.email.clone()),
                preferences: int(&row, "preferences_id")
                    .and_then(|id| preferences.get(&id))
                    .cloned()
                    .unwrap_or_default(),
            });
        }
        Ok(users)
    }

    async fn list(
        &self,
        row: &AnyRow,
        preferences: &BTreeMap<i64, Preferences>,
        addresses: &BTreeMap<i64, AddressRow>,
        users: &BTreeMap<i64, User3>,
        warnings: &mut Vec<String>,
    ) -> Result<List3> {
        let id = int(row, "id").unwrap_or_default();
        let list_id: ListId = text(row, "list_id").parse()?;
        let config = self.config(row, id).await?;
        Ok(List3 {
            mail_host: text(row, "mail_host"),
            display_name: text(row, "display_name"),
            members: self
                .members(&list_id, preferences, addresses, users)
                .await?,
            bans: self.list_bans(&list_id).await?,
            header_matches: self.header_matches(id).await?,
            uris: self.uris(&list_id).await?,
            held: self.held(id, &list_id, warnings).await?,
            requests: self.requests(&list_id).await?,
            config,
            list_id,
        })
    }

    /// The configuration in the REST vocabulary, so the shared plan
    /// applies to it unchanged.
    async fn config(&self, row: &AnyRow, id: i64) -> Result<Map<String, Json>> {
        let mut config = Map::new();
        for name in FLAGS {
            config.insert(name.into(), json!(flag(row, name)));
        }
        for name in TEXTS {
            config.insert(
                name.into(),
                json!(optional_text(row, name).unwrap_or_default()),
            );
        }
        for name in COUNTS {
            if let Some(value) = int(row, name) {
                config.insert(name.into(), json!(value));
            }
        }
        if let Some(value) = float(row, "digest_size_threshold") {
            config.insert("digest_size_threshold".into(), json!(value));
        }
        for (name, names) in ENUMS {
            if let Some(value) = named(row, name, names) {
                config.insert(name.into(), value);
            }
        }
        if let Some(value) = named(row, "unsubscription_policy", &POLICY) {
            config.insert("unsubscription_policy".into(), value);
        }
        if let Some(action) = named(row, "filter_action", &FILTER_ACTION) {
            config.insert("filter_action".into(), action);
        }
        for name in INTERVALS {
            if let Some(days) = Self::interval_days(row, name) {
                config.insert(name.into(), json!(days));
            }
        }
        for name in PICKLED {
            config.insert(name.into(), json!(pickled_list(row, name)));
        }
        config.insert(
            "moderator_password".into(),
            if bytes(row, "moderator_password").is_some_and(|hash| !hash.is_empty()) {
                json!(true)
            } else {
                Json::Null
            },
        );
        let mut aliases = Vec::new();
        for found in self
            .rows(&format!(
                "SELECT alias FROM acceptablealias WHERE mailing_list_id={id} ORDER BY id"
            ))
            .await?
        {
            aliases.push(text(&found, "alias"));
        }
        config.insert("acceptable_aliases".into(), json!(aliases));
        let mut filters: [Vec<String>; 4] = Default::default();
        for found in self
            .rows(&format!(
                "SELECT filter_type, filter_pattern FROM contentfilter WHERE mailing_list_id={id} ORDER BY id"
            ))
            .await?
        {
            if let Some(kind) = int(&found, "filter_type").and_then(|kind| usize::try_from(kind).ok())
                && kind < 4
            {
                filters[kind].push(text(&found, "filter_pattern"));
            }
        }
        let [filter_types, pass_types, filter_extensions, pass_extensions] = filters;
        config.insert("filter_types".into(), json!(filter_types));
        config.insert("pass_types".into(), json!(pass_types));
        config.insert("filter_extensions".into(), json!(filter_extensions));
        config.insert("pass_extensions".into(), json!(pass_extensions));
        Ok(config)
    }

    /// One of the three interval columns as whole days, from the seconds
    /// the query asked for.
    fn interval_days(row: &AnyRow, column: &str) -> Option<i64> {
        int(row, &format!("{column}_seconds")).map(|seconds| seconds / 86_400)
    }

    async fn members(
        &self,
        list_id: &ListId,
        preferences: &BTreeMap<i64, Preferences>,
        addresses: &BTreeMap<i64, AddressRow>,
        users: &BTreeMap<i64, User3>,
    ) -> Result<Vec<Member3>> {
        let mut members = Vec::new();
        for row in self
            .rows(&format!(
                "SELECT role, moderation_action, address_id, preferences_id, user_id FROM member WHERE list_id='{}' ORDER BY id",
                list_id.as_str().replace('\'', "''")
            ))
            .await?
        {
            let Some(role) = named(&row, "role", &ROLE)
                .and_then(|name| name.as_str()?.parse::<MemberRole>().ok())
            else {
                continue;
            };
            let Some((email, display_name, mode)) = member_identity(&row, addresses, users) else {
                continue;
            };
            members.push(Member3 {
                email,
                display_name,
                role,
                moderation_action: named(&row, "moderation_action", &ACTION)
                    .and_then(|name| name.as_str()?.parse::<ModerationAction>().ok()),
                subscription_mode: mode.into(),
                preferences: int(&row, "preferences_id")
                    .and_then(|id| preferences.get(&id))
                    .cloned()
                    .unwrap_or_default(),
            });
        }
        Ok(members)
    }

    async fn list_bans(&self, list_id: &ListId) -> Result<Vec<String>> {
        Ok(self
            .rows(&format!(
                "SELECT email FROM ban WHERE list_id='{}' ORDER BY email",
                list_id.as_str().replace('\'', "''")
            ))
            .await?
            .iter()
            .map(|row| text(row, "email"))
            .collect())
    }

    async fn header_matches(&self, id: i64) -> Result<Vec<HeaderMatchRow>> {
        Ok(self
            .rows(&format!(
                "SELECT header, pattern, chain FROM headermatch WHERE mailing_list_id={id} ORDER BY position"
            ))
            .await?
            .iter()
            .map(|row| HeaderMatchRow {
                header: text(row, "header").to_ascii_lowercase(),
                pattern: text(row, "pattern"),
                chain: optional_text(row, "chain")
                    .filter(|chain| listmngr_db::header_matches::TARGET_CHAINS.contains(&chain.as_str())),
                tag: None,
            })
            .collect())
    }

    async fn uris(&self, list_id: &ListId) -> Result<Vec<(String, String)>> {
        Ok(self
            .rows(&format!(
                "SELECT name, uri FROM template WHERE context='{}' ORDER BY name",
                list_id.as_str().replace('\'', "''")
            ))
            .await?
            .iter()
            .map(|row| (text(row, "name"), text(row, "uri")))
            .collect())
    }

    /// The key/values pended under `token`, JSON-decoded as Mailman
    /// decodes them (`type` is kept as is).
    async fn pended(&self, token: &str) -> Result<BTreeMap<String, Json>> {
        let mut out = BTreeMap::new();
        for row in self
            .rows(&format!(
                "SELECT k.key, k.value FROM pendedkeyvalue k JOIN pended p ON p.id=k.pended_id WHERE p.token='{}'",
                token.replace('\'', "''")
            ))
            .await?
        {
            let key = text(&row, "key");
            let raw = text(&row, "value");
            let value = if key == "type" {
                json!(raw)
            } else {
                serde_json::from_str(&raw).unwrap_or(Json::String(raw))
            };
            out.insert(key, value);
        }
        Ok(out)
    }

    async fn requests(&self, list_id: &ListId) -> Result<Vec<Request3>> {
        let mut requests = Vec::new();
        for row in self
            .rows("SELECT p.token FROM pended p JOIN pendedkeyvalue t ON t.pended_id=p.id AND t.key='type' WHERE t.value IN ('subscription','unsubscription') ORDER BY p.id")
            .await?
        {
            let pended = self.pended(&text(&row, "token")).await?;
            if pended.get("list_id").and_then(Json::as_str) != Some(list_id.as_str()) {
                continue;
            }
            let field = |key: &str| {
                pended
                    .get(key)
                    .and_then(Json::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            requests.push(Request3 {
                email: field("email").to_lowercase(),
                display_name: field("display_name"),
                action: field("type"),
                token_owner: field("token_owner"),
                requested_at: milliseconds_of(&field("when")),
            });
        }
        Ok(requests)
    }

    /// The held messages: each `_request` of the held-message type, its
    /// metadata pended under its `data_hash`, its bytes in the store.
    async fn held(
        &self,
        id: i64,
        list_id: &ListId,
        warnings: &mut Vec<String>,
    ) -> Result<Vec<Hold3>> {
        let mut held = Vec::new();
        for row in self
            .rows(&format!(
                "SELECT r.key, r.data_hash, m.path FROM _request r LEFT JOIN message m ON m.message_id=r.key WHERE r.mailing_list_id={id} AND r.request_type=1 ORDER BY r.id"
            ))
            .await?
        {
            let message_id = text(&row, "key");
            let Some(token) = optional_text(&row, "data_hash") else {
                continue;
            };
            let pended = self.pended(&token).await?;
            let field = |key: &str| {
                pended
                    .get(key)
                    .and_then(Json::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            let reasons = pended
                .get("_pck_moderation_reasons")
                .and_then(Json::as_str)
                .map(|escaped| escaped.chars().map(|c| c as u8).collect::<Vec<u8>>())
                .and_then(|pickled| pickle::read(&pickled).ok())
                .and_then(|item| match item {
                    Item::List(items) => Some(
                        items
                            .into_iter()
                            .filter_map(|item| match item {
                                Item::Text(text) => Some(text),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("; "),
                    ),
                    _ => None,
                })
                .filter(|reason| !reason.is_empty())
                .unwrap_or_else(|| field("_mod_reason"));
            let Some(var_dir) = self.var_dir else {
                warnings.push(format!(
                    "{}: the held message {message_id} is in the core's message store, which --var-dir did not name; not imported",
                    list_id.as_str()
                ));
                continue;
            };
            let Some(path) = optional_text(&row, "path") else {
                warnings.push(format!(
                    "{}: the held message {message_id} is not in the core's message table; not imported",
                    list_id.as_str()
                ));
                continue;
            };
            let file = var_dir.join("messages").join(&path);
            let raw = match std::fs::read(&file).map_err(|error| error.to_string()).and_then(|pickled| {
                render_message(&pickled).map_err(|error| error.to_string())
            }) {
                Ok(raw) => raw,
                Err(reason) => {
                    warnings.push(format!(
                        "{}: the held message {message_id} could not be read from {}: {reason}; not imported",
                        list_id.as_str(),
                        file.display()
                    ));
                    continue;
                }
            };
            held.push(Hold3 {
                sender: field("_mod_sender").to_lowercase(),
                subject: field("_mod_subject"),
                reason: reasons,
                hold_date: milliseconds_of(&field("_mod_hold_date")),
                raw,
            });
        }
        Ok(held)
    }
}

/// Whose membership a row is: an address's, or — when the row names no
/// address — a user's, through the user's preferred address.
fn member_identity(
    row: &AnyRow,
    addresses: &BTreeMap<i64, AddressRow>,
    users: &BTreeMap<i64, User3>,
) -> Option<(String, String, &'static str)> {
    if let Some(address) = int(row, "address_id").and_then(|id| addresses.get(&id)) {
        return Some((
            address.email.clone(),
            address.display_name.clone(),
            "as_address",
        ));
    }
    let user = int(row, "user_id").and_then(|id| users.get(&id))?;
    let email = user.preferred.clone()?;
    Some((email, user.display_name.clone(), "as_user"))
}

fn optional_flag(row: &AnyRow, column: &str) -> Option<bool> {
    let known = row
        .try_get::<Option<bool>, _>(column)
        .ok()
        .flatten()
        .or_else(|| {
            row.try_get::<Option<i64>, _>(column)
                .ok()
                .flatten()
                .map(|value| value != 0)
        });
    known.or_else(|| {
        row.try_get::<Option<i32>, _>(column)
            .ok()
            .flatten()
            .map(|value| value != 0)
    })
}

/// An address row, as much of it as the site needs.
struct AddressRow {
    email: String,
    display_name: String,
    verified: bool,
    user_id: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::{boundary_of, scrub};

    #[test]
    fn the_boundary_is_taken_from_the_content_type() {
        assert_eq!(
            boundary_of(r#"multipart/mixed; boundary="=-=frontier=-=""#).as_deref(),
            Some("=-=frontier=-=")
        );
        assert_eq!(
            boundary_of("multipart/alternative; charset=utf-8; boundary=abc").as_deref(),
            Some("abc")
        );
        assert_eq!(boundary_of("text/plain"), None);
    }

    #[test]
    fn a_url_with_userinfo_never_reaches_a_message() {
        assert_eq!(
            scrub("could not open postgres://mailman:secret@db/mailman now"),
            "could not open <url> now"
        );
    }
}
