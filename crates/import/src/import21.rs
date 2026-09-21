//! Mailman 3's `import21` as a plan made from a `config.pck`.
//!
//! The plan follows `mailman/utilities/importer.py`: every 2.1 setting
//! with a Mailman 3 name and Mailman's conversion, the rosters with each
//! member's options, bans, header filter rules as header matches,
//! acceptable aliases, and the decoration templates with their
//! placeholders converted. `apply` writes it to a list here.
use crate::config21::{Config21, Value};
use listmngr_core::{
    DeliveryMode, DeliveryStatus, ListId, MemberRole, ModerationAction, SubscriptionMode,
};
use listmngr_db::{AuditContext, Database, NewMember, header_matches::HeaderMatchRow};
use serde_json::{Map, Value as Json, json};
use std::collections::BTreeMap;

/// One roster entry to subscribe, with the options Mailman 2.1 kept for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberImport {
    /// Lowercased, the roster key.
    pub email: String,
    /// The address as the member wrote it, when 2.1 kept it.
    pub original_email: String,
    pub role: MemberRole,
    pub display_name: String,
    pub delivery_mode: DeliveryMode,
    pub delivery_status: DeliveryStatus,
    /// A language this site knows, else nothing.
    pub preferred_language: Option<String>,
    /// The member's own moderation action; `None` leaves the list's default.
    pub moderation_action: Option<ModerationAction>,
    pub acknowledge_posts: Option<bool>,
    pub hide_address: Option<bool>,
    pub receive_own_postings: Option<bool>,
    pub receive_list_copy: Option<bool>,
}

/// What an import will do, before it touches the database.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    /// The list configuration patch, in the REST vocabulary.
    pub settings: Map<String, Json>,
    /// Addresses and `^` patterns to ban on the list.
    pub bans: Vec<String>,
    /// Header filter rules as header matches, in 2.1's order.
    pub header_matches: Vec<HeaderMatchRow>,
    /// `(template name, text)` for the decorations 2.1 kept on the list
    /// that differ from Mailman 3's defaults.
    pub templates: Vec<(String, String)>,
    /// Members, owners, moderators and nonmembers, in 2.1's order.
    pub members: Vec<MemberImport>,
    /// What was left out and why, for the operator.
    pub warnings: Vec<String>,
}

impl Plan {
    /// The plan as JSON, for `import21 --dry-run`.
    #[must_use]
    pub fn to_json(&self) -> Json {
        json!({
            "settings": self.settings,
            "bans": self.bans,
            "header_matches": self.header_matches,
            "templates": self.templates.iter().map(|(name, text)| json!({"name": name, "text": text})).collect::<Vec<_>>(),
            "members": self.members.iter().map(|member| json!({
                "email": member.email,
                "original_email": member.original_email,
                "role": member.role.as_str(),
                "display_name": member.display_name,
                "delivery_mode": member.delivery_mode.as_str(),
                "delivery_status": member.delivery_status.as_str(),
                "preferred_language": member.preferred_language,
                "moderation_action": member.moderation_action.map(ModerationAction::as_str),
                "acknowledge_posts": member.acknowledge_posts,
                "hide_address": member.hide_address,
                "receive_own_postings": member.receive_own_postings,
                "receive_list_copy": member.receive_list_copy,
            })).collect::<Vec<_>>(),
            "warnings": self.warnings,
        })
    }
}

/// What an import did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub settings: usize,
    pub members: usize,
    pub owners: usize,
    pub moderators: usize,
    pub nonmembers: usize,
    pub skipped: usize,
    pub bans: usize,
    pub header_matches: usize,
    pub templates: usize,
    pub warnings: Vec<String>,
}

impl ImportReport {
    /// The report as JSON, for the command line.
    #[must_use]
    pub fn to_json(&self) -> Json {
        json!({
            "settings": self.settings,
            "members": self.members,
            "owners": self.owners,
            "moderators": self.moderators,
            "nonmembers": self.nonmembers,
            "skipped": self.skipped,
            "bans": self.bans,
            "header_matches": self.header_matches,
            "templates": self.templates,
            "warnings": self.warnings,
        })
    }
}

/// The languages this site's catalog has.
const LANGUAGES: [&str; 2] = ["en", "vi"];

/// Mailman's `NAME_MAPPINGS`, and the settings copied by their own name:
/// (2.1 key, Mailman 3 setting, conversion).
#[derive(Clone, Copy)]
enum Kind {
    Text,
    Bool,
    Int,
    Float,
    TextList,
    /// Seconds in 2.1, days here.
    SecondsToDays,
    /// A 2.1 integer naming an enum value.
    Enum(&'static [&'static str]),
    /// `autorespond_owner`/`postings`: nonzero means respond and continue.
    Autorespond,
}

const SETTINGS: &[(&str, &str, Kind)] = &[
    ("real_name", "display_name", Kind::Text),
    ("description", "description", Kind::Text),
    ("info", "info", Kind::Text),
    ("advertised", "advertised", Kind::Bool),
    ("anonymous_list", "anonymous_list", Kind::Bool),
    ("admin_immed_notify", "admin_immed_notify", Kind::Bool),
    ("admin_notify_mchanges", "admin_notify_mchanges", Kind::Bool),
    ("administrivia", "administrivia", Kind::Bool),
    (
        "require_explicit_destination",
        "require_explicit_destination",
        Kind::Bool,
    ),
    (
        "respond_to_post_requests",
        "respond_to_post_requests",
        Kind::Bool,
    ),
    ("send_welcome_msg", "send_welcome_message", Kind::Bool),
    ("send_goodbye_msg", "send_goodbye_message", Kind::Bool),
    ("include_list_post_header", "allow_list_posts", Kind::Bool),
    (
        "include_rfc2369_headers",
        "include_rfc2369_headers",
        Kind::Bool,
    ),
    ("emergency", "emergency", Kind::Bool),
    ("autorespond_admin", "autorespond_owner", Kind::Autorespond),
    (
        "autoresponse_admin_text",
        "autoresponse_owner_text",
        Kind::Text,
    ),
    (
        "autorespond_postings",
        "autorespond_postings",
        Kind::Autorespond,
    ),
    (
        "autoresponse_postings_text",
        "autoresponse_postings_text",
        Kind::Text,
    ),
    (
        "autorespond_requests",
        "autorespond_requests",
        Kind::Enum(&["none", "respond_and_continue", "respond_and_discard"]),
    ),
    (
        "autoresponse_request_text",
        "autoresponse_request_text",
        Kind::Text,
    ),
    (
        "autoresponse_graceperiod",
        "autoresponse_grace_period",
        Kind::Int,
    ),
    ("bounce_processing", "process_bounces", Kind::Bool),
    (
        "bounce_score_threshold",
        "bounce_score_threshold",
        Kind::Float,
    ),
    (
        "bounce_info_stale_after",
        "bounce_info_stale_after",
        Kind::SecondsToDays,
    ),
    (
        "bounce_you_are_disabled_warnings",
        "bounce_you_are_disabled_warnings",
        Kind::Int,
    ),
    (
        "bounce_you_are_disabled_warnings_interval",
        "bounce_you_are_disabled_warnings_interval",
        Kind::SecondsToDays,
    ),
    (
        "bounce_notify_owner_on_disable",
        "bounce_notify_owner_on_disable",
        Kind::Bool,
    ),
    (
        "bounce_notify_owner_on_removal",
        "bounce_notify_owner_on_removal",
        Kind::Bool,
    ),
    (
        "bounce_unrecognized_goes_to_list_owner",
        "forward_unrecognized_bounces_to",
        Kind::Enum(&["discard", "site_owner", "administrators"]),
    ),
    ("collapse_alternatives", "collapse_alternatives", Kind::Bool),
    (
        "convert_html_to_plaintext",
        "convert_html_to_plaintext",
        Kind::Bool,
    ),
    (
        "filter_action",
        "filter_action",
        Kind::Enum(&["discard", "reject", "forward", "preserve"]),
    ),
    ("filter_content", "filter_content", Kind::Bool),
    (
        "filter_filename_extensions",
        "filter_extensions",
        Kind::TextList,
    ),
    ("filter_mime_types", "filter_types", Kind::TextList),
    (
        "pass_filename_extensions",
        "pass_extensions",
        Kind::TextList,
    ),
    ("pass_mime_types", "pass_types", Kind::TextList),
    (
        "generic_nonmember_action",
        "default_nonmember_action",
        Kind::Enum(&["defer", "hold", "reject", "discard"]),
    ),
    (
        "dmarc_moderation_addresses",
        "dmarc_addresses",
        Kind::TextList,
    ),
    (
        "dmarc_moderation_notice",
        "dmarc_moderation_notice",
        Kind::Text,
    ),
    (
        "dmarc_wrapped_message_text",
        "dmarc_wrapped_message_text",
        Kind::Text,
    ),
    ("digest_send_periodic", "digest_send_periodic", Kind::Bool),
    (
        "digest_size_threshhold",
        "digest_size_threshold",
        Kind::Float,
    ),
    (
        "digest_volume_frequency",
        "digest_volume_frequency",
        Kind::Enum(&["yearly", "monthly", "quarterly", "weekly", "daily"]),
    ),
    ("next_digest_number", "next_digest_number", Kind::Int),
    ("first_strip_reply_to", "first_strip_reply_to", Kind::Bool),
    (
        "reply_goes_to_list",
        "reply_goes_to_list",
        Kind::Enum(&["no_munging", "point_to_list", "explicit_header"]),
    ),
    ("reply_to_address", "reply_to_address", Kind::Text),
    (
        "personalize",
        "personalize",
        Kind::Enum(&["none", "individual", "full"]),
    ),
    (
        "subscribe_policy",
        "subscription_policy",
        Kind::Enum(&["open", "confirm", "moderate", "confirm_then_moderate"]),
    ),
    (
        "private_roster",
        "member_roster_visibility",
        Kind::Enum(&["public", "members", "moderators"]),
    ),
    ("max_message_size", "max_message_size", Kind::Int),
    ("max_num_recipients", "max_num_recipients", Kind::Int),
    ("gateway_to_mail", "gateway_to_mail", Kind::Bool),
    ("gateway_to_news", "gateway_to_news", Kind::Bool),
    ("linked_newsgroup", "linked_newsgroup", Kind::Text),
    (
        "news_moderation",
        "newsgroup_moderation",
        Kind::Enum(&["none", "open_moderated", "moderated"]),
    ),
    (
        "news_prefix_subject_too",
        "nntp_prefix_subject_too",
        Kind::Bool,
    ),
    ("topics_enabled", "topics_enabled", Kind::Bool),
    (
        "topics_bodylines_limit",
        "topics_bodylines_limit",
        Kind::Int,
    ),
];

/// Mailman's `convert_placeholders`, in its order, to this site's
/// placeholder names.
const PLACEHOLDERS: &[(&str, &str)] = &[
    ("\r\n", "\n"),
    (
        "%(real_name)s@%(host_name)s",
        "To unsubscribe send an email to ${short_listname}-leave@${domain}",
    ),
    (
        "%(real_name)s mailing list",
        "$display_name mailing list -- $listname",
    ),
    (
        "%(web_page_url)slistinfo%(cgiext)s/%(_internal_name)s\n",
        "",
    ),
    ("%(real_name)s", "$display_name"),
    ("%(list_name)s", "$listname"),
    ("%(description)s", "$description"),
    ("%(info)s", "$info"),
    ("%(cgiext)s", ""),
    ("%(user_address)s", "$user_email"),
    ("%(user_delivered_to)s", "$user_delivered_to"),
    ("%(user_password)s", ""),
    ("%(user_name)s", "$user_name"),
];

/// Mailman's placeholder conversion over one text.
fn convert_placeholders(text: &str) -> String {
    let mut text = text.to_owned();
    for (old, new) in PLACEHOLDERS {
        text = text.replace(old, new);
    }
    text
}

/// Mailman's `convert_to_uri`: 2.1 attribute → template name.
const TEMPLATES: &[(&str, &str)] = &[
    ("goodbye_msg", "list:user:notice:goodbye"),
    ("msg_header", "list:member:regular:header"),
    ("msg_footer", "list:member:regular:footer"),
    ("digest_header", "list:member:digest:header"),
    ("digest_footer", "list:member:digest:footer"),
];

fn moderation_action(value: Option<i64>) -> Option<ModerationAction> {
    match value? {
        0 => Some(ModerationAction::Hold),
        1 => Some(ModerationAction::Reject),
        2 => Some(ModerationAction::Discard),
        _ => None,
    }
}

fn dmarc_action(value: i64) -> Option<&'static str> {
    [
        "no_mitigation",
        "munge_from",
        "wrap_message",
        "reject",
        "discard",
    ]
    .get(usize::try_from(value).ok()?)
    .copied()
}

/// Mailman's `action_to_chain` for header filter rules: the chain a 2.1
/// action jumps to, `Chain::Default` for "defer to the list", `None` for
/// an action Mailman does not import.
enum Chain {
    Default,
    Named(&'static str),
}

const fn action_chain(value: i64) -> Option<Chain> {
    match value {
        0 => Some(Chain::Default),
        2 => Some(Chain::Named("reject")),
        3 => Some(Chain::Named("discard")),
        6 => Some(Chain::Named("accept")),
        7 => Some(Chain::Named("hold")),
        _ => None,
    }
}

/// The plan Mailman's importer would apply to `list` from `config`.
#[must_use]
pub fn plan(config: &Config21, list: &ListId) -> Plan {
    let mut plan = Plan::default();
    settings(config, &mut plan);
    if let Some(prefix) = config.text("subject_prefix") {
        // Mailman 3 prefixes with the value as it is, so 2.1's implied
        // space is written into it.
        let prefix = prefix.trim();
        plan.settings.insert(
            "subject_prefix".into(),
            json!(if prefix.is_empty() {
                String::new()
            } else {
                format!("{prefix} ")
            }),
        );
    }
    match config.text("preferred_language") {
        Some(code) if LANGUAGES.contains(&code.as_str()) => {
            plan.settings
                .insert("preferred_language".into(), json!(code));
        }
        Some(code) => plan.warnings.push(format!(
            "preferred_language {code:?} is not a language this site has; kept the list's"
        )),
        None => {}
    }
    if config.get("mod_password").is_some_and(Value::truthy) {
        plan.warnings.push(
            "the moderator password is a hash in Mailman 2.1 and cannot be imported; set a new one"
                .into(),
        );
    }
    // Member moderation and DMARC, as Mailman decides them.
    plan.settings.insert(
        "default_member_action".into(),
        json!(
            if config.bool("default_member_moderation").unwrap_or(false) {
                moderation_action(config.int("member_moderation_action"))
                    .unwrap_or(ModerationAction::Hold)
                    .as_str()
            } else {
                "defer"
            }
        ),
    );
    let from_is_list = config.int("from_is_list").unwrap_or(0);
    let dmarc_moderation = config.int("dmarc_moderation_action").unwrap_or(0);
    let (action, unconditional) = if from_is_list > dmarc_moderation {
        (dmarc_action(from_is_list), true)
    } else {
        (dmarc_action(dmarc_moderation), false)
    };
    if let Some(action) = action {
        plan.settings
            .insert("dmarc_mitigate_action".into(), json!(action));
        plan.settings.insert(
            "dmarc_mitigate_unconditionally".into(),
            json!(unconditional),
        );
    }
    plan.settings.insert(
        "archive_policy".into(),
        json!(if config.bool("archive").unwrap_or(false) {
            if config.bool("archive_private").unwrap_or(true) {
                "private"
            } else {
                "public"
            }
        } else {
            "never"
        }),
    );
    topics(config, &mut plan);
    aliases(config, list, &mut plan);
    bans(config, &mut plan);
    header_matches(config, &mut plan);
    templates(config, &mut plan);
    rosters(config, &mut plan);
    plan
}

fn settings(config: &Config21, plan: &mut Plan) {
    for (key, setting, kind) in SETTINGS {
        let Some(value) = config.get(key) else {
            continue;
        };
        let converted = match kind {
            // The autoresponse texts keep 2.1's `%(name)s` placeholders in
            // Mailman's import; here they are converted like the templates.
            Kind::Text if setting.starts_with("autoresponse_") => value
                .as_text()
                .map(|text| json!(convert_placeholders(text))),
            Kind::Text => value.as_text().map(|text| json!(text)),
            Kind::Bool => Some(json!(value.truthy())),
            Kind::Int => value.as_int().map(|number| json!(number)),
            Kind::Float => config.float(key).map(|number| json!(number)),
            Kind::TextList => Some(json!(config.text_list(key))),
            Kind::SecondsToDays => value.as_int().map(|seconds| json!(seconds / 86_400)),
            Kind::Enum(names) => value
                .as_int()
                .and_then(|number| usize::try_from(number).ok())
                .and_then(|index| names.get(index))
                .map(|name| json!(name)),
            Kind::Autorespond => Some(json!(if value.truthy() {
                "respond_and_continue"
            } else {
                "none"
            })),
        };
        match converted {
            Some(converted) => {
                plan.settings.insert((*setting).to_owned(), converted);
            }
            None => plan
                .warnings
                .push(format!("type conversion error for {key}: {value:?}")),
        }
    }
}

fn topics(config: &Config21, plan: &mut Plan) {
    let mut topics = Vec::new();
    for entry in config.list("topics") {
        let Value::List(fields) = entry else {
            continue;
        };
        let text = |index: usize| {
            fields
                .get(index)
                .and_then(Value::as_text)
                .unwrap_or("")
                .to_owned()
        };
        if text(0).is_empty() || text(1).is_empty() {
            plan.warnings
                .push(format!("topic without a name or pattern: {fields:?}"));
            continue;
        }
        topics.push(json!({"name": text(0), "pattern": text(1), "description": text(2)}));
    }
    if config.has("topics") {
        plan.settings.insert("topics".into(), json!(topics));
    }
}

/// Mailman's `acceptable_aliases`: one per line, each anchored with `^`,
/// plus the list's own name.
fn aliases(config: &Config21, list: &ListId, plan: &mut Plan) {
    let mut aliases: Vec<String> = config
        .text("acceptable_aliases")
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            if line.starts_with('^') {
                line.to_owned()
            } else {
                format!("^{line}")
            }
        })
        .collect();
    aliases.push(format!("^{}@", list.list_name()));
    plan.settings
        .insert("acceptable_aliases".into(), json!(aliases));
}

fn bans(config: &Config21, plan: &mut Plan) {
    for address in config.text_list("ban_list") {
        if address.starts_with('^') && regex::Regex::new(&address).is_err() {
            plan.warnings
                .push(format!("dropping invalid regexp {address} in ban_list"));
            continue;
        }
        plan.bans.push(address);
    }
}

/// Mailman's reading of `header_filter_rules`: `(line patterns, action,
/// unused)`, one rule per line, the header before the first of `: `,
/// `:.*`, `:.` or `:`.
fn header_matches(config: &Config21, plan: &mut Plan) {
    for rule in config.list("header_filter_rules") {
        let Value::List(fields) = rule else {
            continue;
        };
        let lines = fields.first().and_then(Value::as_text).unwrap_or("");
        let action = fields.get(1).and_then(Value::as_int).unwrap_or(-1);
        let Some(chain) = action_chain(action) else {
            plan.warnings
                .push(format!("unsupported header_filter_rules action: {action}"));
            continue;
        };
        for line in lines.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let Some((header, pattern)) = [": ", ":.*", ":.", ":"]
                .iter()
                .find_map(|sep| line.split_once(sep))
            else {
                plan.warnings
                    .push(format!("unsupported header_filter_rules pattern: {line:?}"));
                continue;
            };
            let header = header
                .trim()
                .trim_start_matches('^')
                .replace('\\', "")
                .to_ascii_lowercase();
            if header.is_empty() {
                plan.warnings.push(format!(
                    "cannot parse the header in header_filter_rule: {line:?}"
                ));
                continue;
            }
            let pattern = if pattern.is_empty() {
                ".*".to_owned()
            } else {
                pattern.to_owned()
            };
            if regex::Regex::new(&pattern).is_err() {
                plan.warnings.push(format!(
                    "skipping header_filter rule with an invalid regular expression: {line:?}"
                ));
                continue;
            }
            if plan
                .header_matches
                .iter()
                .any(|row| row.header == header && row.pattern == pattern)
            {
                plan.warnings
                    .push(format!("skipping duplicate header_filter rule: {line:?}"));
                continue;
            }
            plan.header_matches.push(HeaderMatchRow {
                header,
                pattern,
                chain: match chain {
                    Chain::Default => None,
                    Chain::Named(name) => Some(name.to_owned()),
                },
                tag: None,
            });
        }
    }
}

/// The decoration templates 2.1 kept, with Mailman's placeholder
/// conversion, when they differ from this site's defaults.
fn templates(config: &Config21, plan: &mut Plan) {
    for (attribute, name) in TEMPLATES {
        let Some(text) = config.text(attribute) else {
            continue;
        };
        let text = convert_placeholders(&text);
        // Mailman's loader falls back from the digest and regular
        // decorations to the generic ones; the comparison does the same.
        let generic = name
            .rsplit_once(':')
            .map(|(_, kind)| format!("list:member:generic:{kind}"));
        let default = listmngr_mail::templates::builtin(name)
            .filter(|text| !text.trim().is_empty())
            .or_else(|| {
                generic
                    .as_deref()
                    .and_then(listmngr_mail::templates::builtin)
            })
            .unwrap_or("");
        if text.trim().is_empty() || text.trim() == default.trim() {
            continue;
        }
        if text.contains('%') {
            plan.warnings.push(format!(
                "unable to convert every placeholder of {attribute}: {text:?}"
            ));
        }
        plan.templates.push(((*name).to_owned(), text));
    }
}

/// Mailman's `import_roster` for members, owners, moderators and the
/// nonmember lists, with each member's 2.1 options.
/// Every address 2.1 lists, with its role: members (regular then
/// digest), owners, moderators, then the nonmember addresses of the
/// `*_these_nonmembers` lists (their `^` patterns stay on the list; an
/// `@other-list` entry cannot be imported).
fn roster_entries(
    config: &Config21,
    plan: &mut Plan,
) -> Vec<(String, MemberRole, Option<ModerationAction>)> {
    let regulars = config.dict("members");
    let digesters = config.dict("digest_members");
    let mut members: Vec<String> = regulars.keys().cloned().collect();
    members.extend(
        digesters
            .keys()
            .filter(|k| !regulars.contains_key(*k))
            .cloned(),
    );
    let mut entries: Vec<(String, MemberRole, Option<ModerationAction>)> = members
        .into_iter()
        .map(|email| (email, MemberRole::Member, None))
        .collect();
    entries.extend(
        config
            .text_list("owner")
            .into_iter()
            .map(|email| (email, MemberRole::Owner, None)),
    );
    entries.extend(
        config
            .text_list("moderator")
            .into_iter()
            .map(|email| (email, MemberRole::Moderator, None)),
    );
    for (prop, action) in [
        ("accept_these_nonmembers", ModerationAction::Defer),
        ("hold_these_nonmembers", ModerationAction::Hold),
        ("reject_these_nonmembers", ModerationAction::Reject),
        ("discard_these_nonmembers", ModerationAction::Discard),
    ] {
        let mut patterns = Vec::new();
        for entry in config.text_list(prop) {
            if entry.starts_with('^') {
                patterns.push(entry);
            } else if entry.starts_with('@') {
                // Mailman 2.1's "members of another list": no such
                // membership test here.
                plan.warnings.push(format!(
                    "{prop}: {entry:?} names another list's members, which cannot be imported"
                ));
            } else {
                entries.push((entry, MemberRole::Nonmember, Some(action)));
            }
        }
        if config.has(prop) {
            plan.settings.insert(prop.to_owned(), json!(patterns));
        }
    }
    entries
}

/// Whether `email` matches one of the plan's bans.
fn banned(plan: &Plan, email: &str) -> bool {
    plan.bans.iter().any(|ban| {
        if ban.starts_with('^') {
            regex::Regex::new(ban).is_ok_and(|re| re.is_match(email))
        } else {
            ban.eq_ignore_ascii_case(email)
        }
    })
}

/// A 2.1 `delivery_status` entry `(code, when)` as a status.
fn delivery_status(entry: Option<&Value>) -> DeliveryStatus {
    match entry {
        Some(Value::List(pair)) => match pair.first().and_then(Value::as_int) {
            Some(1) => DeliveryStatus::Unknown,
            Some(2) => DeliveryStatus::ByUser,
            Some(3) => DeliveryStatus::ByModerator,
            Some(4) => DeliveryStatus::ByBounces,
            _ => DeliveryStatus::Enabled,
        },
        _ => DeliveryStatus::Enabled,
    }
}

fn rosters(config: &Config21, plan: &mut Plan) {
    let regulars = config.dict("members");
    let digesters = config.dict("digest_members");
    let options = config.dict("user_options");
    let usernames = config.text_dict("usernames");
    let languages = config.text_dict("language");
    let statuses = config.dict("delivery_status");
    let moderated = moderation_action(config.int("member_moderation_action"));
    let mut seen: Vec<(String, MemberRole)> = Vec::new();
    for (email, role, nonmember_action) in roster_entries(config, plan) {
        let email = email.to_lowercase();
        if banned(plan, &email) {
            plan.warnings.push(format!(
                "{email} is banned and not imported with role {}",
                role.as_str()
            ));
            continue;
        }
        if seen.contains(&(email.clone(), role)) {
            plan.warnings.push(format!(
                "{email} is listed twice with role {}",
                role.as_str()
            ));
            continue;
        }
        seen.push((email.clone(), role));
        let original_email = regulars
            .get(&email)
            .or_else(|| digesters.get(&email))
            .and_then(Value::as_text)
            .filter(|original| original.contains('@'))
            .map_or_else(|| email.clone(), str::to_owned);
        let prefs = options.get(&email).and_then(Value::as_int);
        let delivery_mode = if regulars.contains_key(&email) {
            DeliveryMode::Regular
        } else if digesters.contains_key(&email) {
            if prefs.is_some_and(|bits| bits & 8 != 0) {
                DeliveryMode::PlaintextDigests
            } else {
                DeliveryMode::MimeDigests
            }
        } else {
            DeliveryMode::Regular
        };
        let delivery_status = if matches!(role, MemberRole::Owner | MemberRole::Moderator) {
            DeliveryStatus::Enabled
        } else {
            delivery_status(statuses.get(&email))
        };
        let moderation_action = match (prefs, nonmember_action) {
            (Some(bits), _) if bits & 128 != 0 => moderated.or(Some(ModerationAction::Hold)),
            (Some(_), _) => Some(ModerationAction::Defer),
            (None, action) => action,
        };
        plan.members.push(MemberImport {
            display_name: usernames.get(&email).cloned().unwrap_or_default(),
            preferred_language: languages
                .get(&email)
                .filter(|code| LANGUAGES.contains(&code.as_str()))
                .cloned(),
            delivery_mode,
            delivery_status,
            moderation_action,
            acknowledge_posts: prefs.map(|bits| bits & 4 != 0),
            hide_address: prefs.map(|bits| bits & 16 != 0),
            receive_own_postings: prefs.map(|bits| bits & 2 == 0),
            receive_list_copy: prefs.map(|bits| bits & 256 == 0),
            email,
            original_email,
            role,
        });
    }
}

/// The lowercased addresses already on `list`, by role name.
async fn subscribed(
    db: &Database,
    list: &ListId,
) -> crate::Result<BTreeMap<&'static str, Vec<String>>> {
    let mut existing = BTreeMap::new();
    for role in [
        MemberRole::Member,
        MemberRole::Owner,
        MemberRole::Moderator,
        MemberRole::Nonmember,
    ] {
        let mut emails = Vec::new();
        for member in db.members().roster(list, role).await? {
            emails.push(
                db.addresses()
                    .get_by_id(member.address_id)
                    .await?
                    .email
                    .to_lowercase(),
            );
        }
        existing.insert(role.as_str(), emails);
    }
    Ok(existing)
}

/// Subscribe one planned member by address, then its preferences and its
/// own moderation action.
async fn subscribe(
    db: &Database,
    list: &ListId,
    entry: &MemberImport,
    context: &AuditContext,
) -> crate::Result<()> {
    let member = db
        .members()
        .subscribe_with_context(
            NewMember {
                list_id: list.clone(),
                email: entry.original_email.clone(),
                role: entry.role,
                subscription_mode: SubscriptionMode::AsAddress,
                display_name: entry.display_name.clone(),
            },
            true,
            context,
        )
        .await?;
    db.preferences()
        .set_member_with_context(
            member.id,
            listmngr_core::Preferences {
                acknowledge_posts: entry.acknowledge_posts,
                hide_address: entry.hide_address,
                preferred_language: entry.preferred_language.clone(),
                receive_list_copy: entry.receive_list_copy,
                receive_own_postings: entry.receive_own_postings,
                delivery_mode: Some(entry.delivery_mode),
                delivery_status: Some(entry.delivery_status),
            },
            context,
        )
        .await?;
    if let Some(action) = entry.moderation_action {
        db.members()
            .update_with_context(
                member.id,
                &json!({ "moderation_action": action.as_str() }),
                context,
            )
            .await?;
    }
    Ok(())
}

/// Apply `plan` to `list`: the settings in one audited write, then each
/// ban, header match, template and member in its own; what is there
/// already is left alone, so an import can be run again.
/// # Errors
/// Returns the first repository error; what was applied before it stays.
pub async fn apply(
    db: &Database,
    list: &ListId,
    plan: &Plan,
    context: &AuditContext,
) -> crate::Result<ImportReport> {
    let mut report = ImportReport {
        warnings: plan.warnings.clone(),
        ..ImportReport::default()
    };
    db.lists().get(list).await?;
    if !plan.settings.is_empty() {
        db.lists()
            .update_with_context(list, &Json::Object(plan.settings.clone()), context)
            .await?;
        report.settings = plan.settings.len();
    }
    for ban in &plan.bans {
        match db.bans().get(list, ban).await {
            Ok(_) => {}
            Err(listmngr_core::Error::NotFound(_)) => {
                db.bans().create(list, ban, context).await?;
                report.bans += 1;
            }
            Err(error) => return Err(error.into()),
        }
    }
    let existing = db.header_matches().list(list).await?;
    for row in &plan.header_matches {
        if existing
            .iter()
            .any(|have| have.header == row.header && have.pattern == row.pattern)
        {
            continue;
        }
        db.header_matches()
            .append(list, row.clone(), context)
            .await?;
        report.header_matches += 1;
    }
    let language = plan
        .settings
        .get("preferred_language")
        .and_then(Json::as_str)
        .unwrap_or("en")
        .to_owned();
    let scope = listmngr_db::templates::Scope::List(list.clone());
    for (name, text) in &plan.templates {
        db.templates()
            .set_body_with_context(&scope, name, &language, text, context)
            .await?;
        report.templates += 1;
    }
    let existing = subscribed(db, list).await?;
    for entry in &plan.members {
        if existing
            .get(entry.role.as_str())
            .is_some_and(|emails| emails.contains(&entry.email))
        {
            report.skipped += 1;
            report.warnings.push(format!(
                "{} is already subscribed with role {}",
                entry.email,
                entry.role.as_str()
            ));
            continue;
        }
        subscribe(db, list, entry, context).await?;
        match entry.role {
            MemberRole::Member => report.members += 1,
            MemberRole::Owner => report.owners += 1,
            MemberRole::Moderator => report.moderators += 1,
            MemberRole::Nonmember => report.nonmembers += 1,
        }
    }
    db.audit()
        .record_with_context(
            context,
            "list.import21",
            "list",
            list.as_str(),
            report.to_json(),
        )
        .await?;
    Ok(report)
}
