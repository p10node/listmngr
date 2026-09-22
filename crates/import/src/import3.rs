//! A Mailman 3 site read over its REST API and applied here.
//!
//! Mailman 3 is this project's model, so most of a list's configuration
//! carries its own name; what this module does is fetch the site
//! (domains, lists, the rosters with each member's own preferences, bans
//! and header matches), turn Mailman's spellings into this site's
//! (durations as days, one alias per line, the resources Mailman derives
//! left out) and write it, leaving alone whatever is here already so an
//! import can be run again.
use crate::Result;
use listmngr_core::{
    DeliveryMode, DeliveryStatus, ListId, MemberRole, ModerationAction, Preferences,
    SubscriptionMode,
};
use listmngr_db::header_matches::HeaderMatchRow;
use listmngr_db::workflows::SubscriptionAction;
use listmngr_db::{
    AuditContext, Database, ImportedAddress, ImportedHold, ImportedRequest, ImportedUser, NewList,
    NewMember,
};
use serde_json::{Map, Value as Json, json};

/// Where the site is read from: the REST client, or a recorded site in
/// the tests.
pub trait Source {
    /// One resource, by its path under the API root.
    ///
    /// # Errors
    /// Whatever the source cannot read.
    fn get(&self, path: &str) -> impl Future<Output = Result<Json>> + Send;

    /// One resource that may not be there: Mailman answers `404` for a
    /// user without a preferred address, which is not an error.
    ///
    /// # Errors
    /// Whatever the source cannot read, other than a missing resource.
    fn get_optional(&self, path: &str) -> impl Future<Output = Result<Option<Json>>> + Send;
}

/// One address of a Mailman account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address3 {
    pub email: String,
    pub display_name: String,
    /// Whether Mailman had proof of the mailbox.
    pub verified: bool,
}

/// An account of the Mailman site. Mailman has one behind every
/// address, so most of these carry nothing but the address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User3 {
    pub user_id: String,
    pub display_name: String,
    pub is_server_owner: bool,
    /// Whether Mailman had a password hash for the account. The hash
    /// itself is never read out of the core.
    pub has_password: bool,
    pub addresses: Vec<Address3>,
    pub preferred: Option<String>,
    pub preferences: Preferences,
}

/// A message the core is holding for a moderator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hold3 {
    pub sender: String,
    pub subject: String,
    pub reason: String,
    /// When the core held it, in milliseconds.
    pub hold_date: i64,
    /// The message as the core kept it.
    pub raw: Vec<u8>,
}

/// A subscription change the core has not decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request3 {
    pub email: String,
    pub display_name: String,
    /// Mailman's `subscription` or `unsubscription`.
    pub action: String,
    /// Mailman's `moderator` (a moderator decides) or `subscriber` (a
    /// confirmation is out).
    pub token_owner: String,
    pub requested_at: i64,
}

/// A domain of the Mailman site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Domain3 {
    pub mail_host: String,
    pub description: String,
    pub alias_domain: Option<String>,
}

/// A member of one of its lists, with the preferences set on the member
/// itself (Mailman answers only those).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member3 {
    pub email: String,
    pub display_name: String,
    pub role: MemberRole,
    pub moderation_action: Option<ModerationAction>,
    /// Mailman's `as_address` or `as_user`.
    pub subscription_mode: String,
    pub preferences: Preferences,
}

/// One of its lists, as the core answers for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct List3 {
    pub list_id: ListId,
    pub mail_host: String,
    pub display_name: String,
    /// The `config` resource, Mailman's own names and values.
    pub config: Map<String, Json>,
    pub members: Vec<Member3>,
    pub bans: Vec<String>,
    pub header_matches: Vec<HeaderMatchRow>,
    /// `(template name, URI)`; Mailman keeps only the URI.
    pub uris: Vec<(String, String)>,
    pub held: Vec<Hold3>,
    pub requests: Vec<Request3>,
}

/// The whole site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    pub domains: Vec<Domain3>,
    pub site_bans: Vec<String>,
    pub users: Vec<User3>,
    pub lists: Vec<List3>,
    /// What the source could not read, for the plan to carry.
    pub warnings: Vec<String>,
}

/// What an import will write for one list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListPlan3 {
    pub list_id: ListId,
    pub mail_host: String,
    pub display_name: String,
    /// The configuration patch in this site's vocabulary.
    pub settings: Map<String, Json>,
    pub bans: Vec<String>,
    pub header_matches: Vec<HeaderMatchRow>,
    pub members: Vec<Member3>,
    pub held: Vec<Hold3>,
    /// Only the requests a moderator has to decide: a confirmation the
    /// subscriber owes belongs to the token the other system issued.
    pub requests: Vec<Request3>,
}

/// What an import will write.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan3 {
    pub domains: Vec<Domain3>,
    pub site_bans: Vec<String>,
    pub users: Vec<User3>,
    pub lists: Vec<ListPlan3>,
    pub warnings: Vec<String>,
}

impl Plan3 {
    /// The plan as JSON, for `import3 --dry-run`.
    #[must_use]
    pub fn to_json(&self) -> Json {
        json!({
            "domains": self.domains.iter().map(|domain| json!({
                "mail_host": domain.mail_host,
                "description": domain.description,
                "alias_domain": domain.alias_domain,
            })).collect::<Vec<_>>(),
            "site_bans": self.site_bans,
            "users": self.users.iter().map(|user| json!({
                "display_name": user.display_name,
                "is_server_owner": user.is_server_owner,
                "addresses": user.addresses.iter().map(|address| json!({
                    "email": address.email,
                    "verified": address.verified,
                })).collect::<Vec<_>>(),
                "preferred": user.preferred,
            })).collect::<Vec<_>>(),
            "lists": self.lists.iter().map(|list| json!({
                "list_id": list.list_id.as_str(),
                "display_name": list.display_name,
                "settings": list.settings,
                "bans": list.bans,
                "header_matches": list.header_matches,
                "held": list.held.len(),
                "requests": list.requests.iter().map(|request| json!({
                    "email": request.email,
                    "action": request.action,
                })).collect::<Vec<_>>(),
                "members": list.members.iter().map(|member| json!({
                    "email": member.email,
                    "role": member.role.as_str(),
                    "display_name": member.display_name,
                    "moderation_action": member.moderation_action.map(ModerationAction::as_str),
                    "subscription_mode": member.subscription_mode,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "warnings": self.warnings,
        })
    }
}

/// What an import did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report3 {
    pub domains: usize,
    pub users: usize,
    pub addresses: usize,
    pub lists: usize,
    pub settings: usize,
    pub members: usize,
    pub owners: usize,
    pub moderators: usize,
    pub nonmembers: usize,
    pub skipped: usize,
    pub bans: usize,
    pub site_bans: usize,
    pub header_matches: usize,
    pub held: usize,
    pub requests: usize,
    pub warnings: Vec<String>,
}

impl Report3 {
    /// The report as JSON, for the command line.
    #[must_use]
    pub fn to_json(&self) -> Json {
        json!({
            "domains": self.domains,
            "users": self.users,
            "addresses": self.addresses,
            "lists": self.lists,
            "settings": self.settings,
            "members": self.members,
            "owners": self.owners,
            "moderators": self.moderators,
            "nonmembers": self.nonmembers,
            "skipped": self.skipped,
            "bans": self.bans,
            "site_bans": self.site_bans,
            "header_matches": self.header_matches,
            "held": self.held,
            "requests": self.requests,
            "warnings": self.warnings,
        })
    }
}

/// The languages this site's catalog has.
const LANGUAGES: [&str; 2] = ["en", "vi"];

/// How a Mailman value becomes this site's.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind3 {
    /// The same JSON value.
    Same,
    /// Mailman's `7d` timedelta as whole days.
    Days,
    /// Mailman keeps one string per entry but accepts a multi-line one;
    /// the aliases are one per line either way.
    Lines,
    /// A number this site keeps as a float, whichever way Mailman wrote
    /// it (`5` and `5.0` are the same threshold).
    Float,
}

/// Every Mailman 3 list setting this site has, with its conversion.
const SETTINGS: &[(&str, Kind3)] = &[
    ("accept_these_nonmembers", Kind3::Same),
    ("acceptable_aliases", Kind3::Lines),
    ("admin_immed_notify", Kind3::Same),
    ("admin_notify_mchanges", Kind3::Same),
    ("administrivia", Kind3::Same),
    ("advertised", Kind3::Same),
    ("allow_list_posts", Kind3::Same),
    ("anonymous_list", Kind3::Same),
    ("archive_policy", Kind3::Same),
    ("archive_rendering_mode", Kind3::Same),
    ("autorespond_owner", Kind3::Same),
    ("autorespond_postings", Kind3::Same),
    ("autorespond_requests", Kind3::Same),
    ("autoresponse_grace_period", Kind3::Days),
    ("autoresponse_owner_text", Kind3::Same),
    ("autoresponse_postings_text", Kind3::Same),
    ("autoresponse_request_text", Kind3::Same),
    ("bounce_info_stale_after", Kind3::Days),
    ("bounce_notify_owner_on_bounce_increment", Kind3::Same),
    ("bounce_notify_owner_on_disable", Kind3::Same),
    ("bounce_notify_owner_on_removal", Kind3::Same),
    ("bounce_score_threshold", Kind3::Float),
    ("bounce_you_are_disabled_warnings", Kind3::Same),
    ("bounce_you_are_disabled_warnings_interval", Kind3::Days),
    ("collapse_alternatives", Kind3::Same),
    ("convert_html_to_plaintext", Kind3::Same),
    ("default_member_action", Kind3::Same),
    ("default_nonmember_action", Kind3::Same),
    ("description", Kind3::Same),
    ("digest_send_periodic", Kind3::Same),
    ("digest_size_threshold", Kind3::Float),
    ("digest_volume_frequency", Kind3::Same),
    ("digests_enabled", Kind3::Same),
    ("discard_these_nonmembers", Kind3::Same),
    ("display_name", Kind3::Same),
    ("dmarc_addresses", Kind3::Same),
    ("dmarc_mitigate_action", Kind3::Same),
    ("dmarc_mitigate_unconditionally", Kind3::Same),
    ("dmarc_moderation_notice", Kind3::Same),
    ("dmarc_wrapped_message_text", Kind3::Same),
    ("emergency", Kind3::Same),
    ("filter_action", Kind3::Same),
    ("filter_content", Kind3::Same),
    ("filter_extensions", Kind3::Same),
    ("filter_types", Kind3::Same),
    ("first_strip_reply_to", Kind3::Same),
    ("forward_unrecognized_bounces_to", Kind3::Same),
    ("gateway_to_mail", Kind3::Same),
    ("gateway_to_news", Kind3::Same),
    ("hold_these_nonmembers", Kind3::Same),
    ("include_rfc2369_headers", Kind3::Same),
    ("info", Kind3::Same),
    ("linked_newsgroup", Kind3::Same),
    ("max_message_size", Kind3::Same),
    ("max_num_recipients", Kind3::Same),
    ("member_roster_visibility", Kind3::Same),
    ("newsgroup_moderation", Kind3::Same),
    ("next_digest_number", Kind3::Same),
    ("nntp_prefix_subject_too", Kind3::Same),
    ("pass_extensions", Kind3::Same),
    ("pass_types", Kind3::Same),
    ("personalize", Kind3::Same),
    ("posting_pipeline", Kind3::Same),
    ("preferred_language", Kind3::Same),
    ("process_bounces", Kind3::Same),
    ("reject_these_nonmembers", Kind3::Same),
    ("reply_goes_to_list", Kind3::Same),
    ("reply_to_address", Kind3::Same),
    ("require_explicit_destination", Kind3::Same),
    ("respond_to_post_requests", Kind3::Same),
    ("send_goodbye_message", Kind3::Same),
    ("send_welcome_message", Kind3::Same),
    ("subject_prefix", Kind3::Same),
    ("subscription_policy", Kind3::Same),
    ("unsubscription_policy", Kind3::Same),
];

/// What Mailman derives or keeps for itself: read, never written.
const DERIVED: &[&str] = &[
    "bounces_address",
    "created_at",
    "digest_last_sent_at",
    "fqdn_listname",
    "http_etag",
    "join_address",
    "last_post_at",
    "leave_address",
    "list_name",
    "mail_host",
    "no_reply_address",
    "owner_address",
    "post_id",
    "posting_address",
    "request_address",
    "self_link",
    "usenet_watermark",
    "volume",
];

/// Mailman's `7d`/`1d2h30m` timedelta as whole days, and whatever is
/// left over.
fn days(value: &Json) -> Option<(i64, bool)> {
    if let Some(number) = value.as_i64() {
        return Some((number, false));
    }
    let text = value.as_str()?;
    let mut total = 0_i64;
    let mut number = String::new();
    for character in text.chars() {
        if character.is_ascii_digit() {
            number.push(character);
            continue;
        }
        let amount: i64 = number.parse().ok()?;
        number.clear();
        let seconds = match character {
            'd' => 86_400,
            'h' => 3_600,
            'm' => 60,
            's' => 1,
            _ => return None,
        };
        total += amount * seconds;
    }
    if !number.is_empty() {
        return None;
    }
    Some((total / 86_400, total % 86_400 != 0))
}

/// Mailman accepts a list of aliases or one multi-line string; this site
/// keeps one per line either way.
fn lines(value: &Json) -> Vec<String> {
    let mut out = Vec::new();
    match value {
        Json::Array(items) => {
            for item in items {
                out.extend(lines(item));
            }
        }
        Json::String(text) => out.extend(
            text.lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned),
        ),
        _ => {}
    }
    out
}

/// The whole site, or one list of it, read from `source`.
///
/// # Errors
/// Whatever the source cannot read, and a list identifier the core gives
/// that this site cannot parse.
pub async fn fetch<S: Source + Sync>(source: &S, only: Option<&ListId>) -> Result<Site> {
    let mut site = Site {
        domains: Vec::new(),
        site_bans: Vec::new(),
        users: Vec::new(),
        lists: Vec::new(),
        warnings: Vec::new(),
    };
    for entry in collection(source, "domains").await? {
        site.domains.push(Domain3 {
            mail_host: text(&entry, "mail_host"),
            description: text(&entry, "description"),
            alias_domain: entry
                .get("alias_domain")
                .and_then(Json::as_str)
                .filter(|alias| !alias.is_empty())
                .map(str::to_owned),
        });
    }
    for entry in collection(source, "bans").await? {
        site.site_bans.push(text(&entry, "email"));
    }
    for entry in collection(source, "users").await? {
        site.users.push(one_user(source, &entry).await?);
    }
    for entry in collection(source, "lists?advertised=false").await? {
        let list_id: ListId = text(&entry, "list_id").parse()?;
        if only.is_some_and(|wanted| wanted != &list_id) {
            continue;
        }
        site.lists.push(one_list(source, &list_id, &entry).await?);
    }
    Ok(site)
}

/// One account, with its addresses, the one it prefers (Mailman answers
/// `404` when there is none) and its own preferences. The password hash
/// Mailman may put in the resource is noted but never read out.
async fn one_user<S: Source + Sync>(source: &S, entry: &Json) -> Result<User3> {
    let user_id = text(entry, "user_id");
    let mut addresses = Vec::new();
    for found in collection(source, &format!("users/{user_id}/addresses")).await? {
        addresses.push(Address3 {
            email: text(&found, "email").to_lowercase(),
            display_name: text(&found, "display_name"),
            verified: found
                .get("verified_on")
                .is_some_and(|value| !value.is_null()),
        });
    }
    let preferred = source
        .get_optional(&format!("users/{user_id}/preferred_address"))
        .await?
        .map(|address| text(&address, "email").to_lowercase());
    let preferences = source
        .get(&format!("users/{user_id}/preferences"))
        .await
        .unwrap_or_else(|_| json!({}));
    Ok(User3 {
        user_id,
        display_name: text(entry, "display_name"),
        is_server_owner: entry
            .get("is_server_owner")
            .and_then(Json::as_bool)
            .unwrap_or(false),
        has_password: entry
            .get("password")
            .is_some_and(|value| value.as_str().is_some_and(|hash| !hash.is_empty())),
        addresses,
        preferred,
        preferences: preferences_of(&preferences),
    })
}

async fn one_list<S: Source + Sync>(source: &S, list_id: &ListId, entry: &Json) -> Result<List3> {
    let id = list_id.as_str();
    let config = source.get(&format!("lists/{id}/config")).await?;
    let mut members = Vec::new();
    for role in [
        MemberRole::Member,
        MemberRole::Owner,
        MemberRole::Moderator,
        MemberRole::Nonmember,
    ] {
        for found in collection(source, &format!("lists/{id}/roster/{}", role.as_str())).await? {
            members.push(one_member(source, role, &found).await?);
        }
    }
    let mut bans = Vec::new();
    for found in collection(source, &format!("lists/{id}/bans")).await? {
        bans.push(text(&found, "email"));
    }
    let mut header_matches = Vec::new();
    for found in collection(source, &format!("lists/{id}/header-matches")).await? {
        header_matches.push(HeaderMatchRow {
            header: text(&found, "header").to_ascii_lowercase(),
            pattern: text(&found, "pattern"),
            chain: found
                .get("action")
                .and_then(Json::as_str)
                .filter(|action| listmngr_db::header_matches::TARGET_CHAINS.contains(action))
                .map(str::to_owned),
            tag: None,
        });
    }
    let mut uris = Vec::new();
    for found in collection(source, &format!("lists/{id}/uris")).await? {
        let (Some(name), Some(uri)) = (
            found.get("name").and_then(Json::as_str),
            found.get("uri").and_then(Json::as_str),
        ) else {
            continue;
        };
        uris.push((name.to_owned(), uri.to_owned()));
    }
    let mut held = Vec::new();
    for found in collection(source, &format!("lists/{id}/held")).await? {
        held.push(Hold3 {
            sender: text(&found, "sender"),
            subject: text(&found, "subject"),
            reason: found
                .get("moderation_reasons")
                .and_then(Json::as_array)
                .map(|reasons| {
                    reasons
                        .iter()
                        .filter_map(Json::as_str)
                        .collect::<Vec<_>>()
                        .join("; ")
                })
                .filter(|reason| !reason.is_empty())
                .unwrap_or_else(|| text(&found, "reason")),
            hold_date: milliseconds_of(&text(&found, "hold_date")),
            raw: text(&found, "msg").into_bytes(),
        });
    }
    let mut requests = Vec::new();
    for found in collection(source, &format!("lists/{id}/requests")).await? {
        requests.push(Request3 {
            email: text(&found, "email").to_lowercase(),
            display_name: text(&found, "display_name"),
            action: text(&found, "type"),
            token_owner: text(&found, "token_owner"),
            requested_at: milliseconds_of(&text(&found, "when")),
        });
    }
    Ok(List3 {
        list_id: list_id.clone(),
        mail_host: text(entry, "mail_host"),
        display_name: text(entry, "display_name"),
        config: config.as_object().cloned().unwrap_or_default(),
        members,
        bans,
        header_matches,
        uris,
        held,
        requests,
    })
}

/// A Mailman timestamp (`2026-09-21T11:49:26.586592`, no zone: the core
/// writes UTC) in milliseconds; an unreadable one is "now", so a hold
/// still comes over.
#[must_use]
pub fn milliseconds_of(stamp: &str) -> i64 {
    chrono::NaiveDateTime::parse_from_str(stamp, "%Y-%m-%dT%H:%M:%S%.f").map_or_else(
        |_| chrono::Utc::now().timestamp_millis(),
        |naive| naive.and_utc().timestamp_millis(),
    )
}

async fn one_member<S: Source + Sync>(
    source: &S,
    role: MemberRole,
    entry: &Json,
) -> Result<Member3> {
    let member_id = text(entry, "member_id");
    let preferences = source
        .get(&format!("members/{member_id}/preferences"))
        .await?;
    Ok(Member3 {
        email: text(entry, "email").to_lowercase(),
        display_name: text(entry, "display_name"),
        role,
        moderation_action: entry
            .get("moderation_action")
            .and_then(Json::as_str)
            .and_then(|action| action.parse().ok()),
        subscription_mode: entry
            .get("subscription_mode")
            .and_then(Json::as_str)
            .unwrap_or("as_address")
            .to_owned(),
        preferences: preferences_of(&preferences),
    })
}

/// The preferences Mailman answers, which are only those set on the
/// resource itself; a language this site's catalog does not have is
/// left out.
fn preferences_of(preferences: &Json) -> Preferences {
    Preferences {
        acknowledge_posts: preferences.get("acknowledge_posts").and_then(Json::as_bool),
        hide_address: preferences.get("hide_address").and_then(Json::as_bool),
        preferred_language: preferences
            .get("preferred_language")
            .and_then(Json::as_str)
            .filter(|code| LANGUAGES.contains(code))
            .map(str::to_owned),
        receive_list_copy: preferences.get("receive_list_copy").and_then(Json::as_bool),
        receive_own_postings: preferences
            .get("receive_own_postings")
            .and_then(Json::as_bool),
        delivery_mode: preferences
            .get("delivery_mode")
            .and_then(Json::as_str)
            .and_then(|mode| mode.parse::<DeliveryMode>().ok()),
        delivery_status: preferences
            .get("delivery_status")
            .and_then(Json::as_str)
            .and_then(|status| status.parse::<DeliveryStatus>().ok()),
    }
}

/// A collection, page by page: Mailman answers `entries`, `start` and
/// `total_size`, and pages with `count` and `page`.
async fn collection<S: Source + Sync>(source: &S, path: &str) -> Result<Vec<Json>> {
    let first = source.get(path).await?;
    let mut entries = entries_of(&first);
    let total = first
        .get("total_size")
        .and_then(Json::as_u64)
        .and_then(|total| usize::try_from(total).ok())
        .unwrap_or(entries.len());
    let count = entries.len();
    if count == 0 || entries.len() >= total {
        return Ok(entries);
    }
    let separator = if path.contains('?') { '&' } else { '?' };
    let mut page = 2;
    while entries.len() < total {
        let more = source
            .get(&format!("{path}{separator}count={count}&page={page}"))
            .await?;
        let found = entries_of(&more);
        if found.is_empty() {
            break;
        }
        entries.extend(found);
        page += 1;
    }
    Ok(entries)
}

fn entries_of(body: &Json) -> Vec<Json> {
    body.get("entries")
        .and_then(Json::as_array)
        .cloned()
        .unwrap_or_default()
}

fn text(value: &Json, key: &str) -> String {
    value
        .get(key)
        .and_then(Json::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// What the site will be written as, with a warning for everything left
/// out.
#[must_use]
pub fn plan(site: &Site) -> Plan3 {
    let mut plan = Plan3 {
        domains: site.domains.clone(),
        site_bans: site.site_bans.clone(),
        users: site.users.clone(),
        warnings: site.warnings.clone(),
        ..Plan3::default()
    };
    for user in &site.users {
        if !user.has_password {
            continue;
        }
        let who = user
            .preferred
            .clone()
            .or_else(|| user.addresses.first().map(|address| address.email.clone()))
            .unwrap_or_else(|| user.user_id.clone());
        plan.warnings.push(format!(
            "{who}: the account's password is hashed by Mailman and cannot be carried over; the account is imported without a usable password and its owner sets one through the recovery flow"
        ));
    }
    for list in &site.lists {
        let mut settings = Map::new();
        for (key, value) in &list.config {
            let Some((_, kind)) = SETTINGS.iter().find(|(name, _)| name == key) else {
                if !DERIVED.contains(&key.as_str()) {
                    unsupported(&mut plan, list, key, value);
                }
                continue;
            };
            match convert(*kind, value) {
                Ok(Some(value)) => {
                    settings.insert(key.clone(), value);
                }
                Ok(None) => {}
                Err(reason) => plan
                    .warnings
                    .push(format!("{}: {key} {reason}", list.list_id.as_str())),
            }
        }
        if let Some(code) = settings.get("preferred_language").and_then(Json::as_str)
            && !LANGUAGES.contains(&code)
        {
            plan.warnings.push(format!(
                "{}: preferred_language {code:?} is not a language this site has; kept the list's",
                list.list_id.as_str()
            ));
            settings.remove("preferred_language");
        }
        for (name, uri) in &list.uris {
            plan.warnings.push(format!(
                "{}: the template {name} is a URI in Mailman ({uri}); set its text here with listmngr templates",
                list.list_id.as_str()
            ));
        }
        for member in &list.members {
            if member.subscription_mode == "as_address" {
                continue;
            }
            let known = site.users.iter().any(|user| {
                user.addresses
                    .iter()
                    .any(|address| address.email == member.email)
            });
            if !known {
                plan.warnings.push(format!(
                    "{}: {} is subscribed {} in Mailman, whose account this import did not see; subscribed as the address",
                    list.list_id.as_str(),
                    member.email,
                    member.subscription_mode
                ));
            }
        }
        let mut requests = Vec::new();
        for request in &list.requests {
            if request.token_owner == "moderator" {
                requests.push(request.clone());
                continue;
            }
            plan.warnings.push(format!(
                "{}: {} still had to confirm a {} in Mailman; that confirmation token belongs to the old site and cannot be carried over, so the request is not imported",
                list.list_id.as_str(),
                request.email,
                request.action
            ));
        }
        plan.lists.push(ListPlan3 {
            list_id: list.list_id.clone(),
            mail_host: list.mail_host.clone(),
            display_name: list.display_name.clone(),
            settings,
            bans: list.bans.clone(),
            header_matches: list.header_matches.clone(),
            members: list.members.clone(),
            held: list.held.clone(),
            requests,
        });
    }
    plan
}

/// A setting this site does not have. `moderator_password` is a hash and
/// is never carried over, and its value never reaches a message.
fn unsupported(plan: &mut Plan3, list: &List3, key: &str, value: &Json) {
    let id = list.list_id.as_str();
    if key == "moderator_password" {
        if !value.is_null() {
            plan.warnings.push(format!(
                "{id}: the moderator password is a hash in Mailman and cannot be imported; set a new one"
            ));
        }
        return;
    }
    if value.is_null() || value == &json!(0) || value == &json!("") || value == &json!([]) {
        // A setting this site does not have, left at Mailman's default:
        // nothing is lost by not importing it.
        return;
    }
    plan.warnings.push(format!(
        "{id}: {key} is not a setting this site has ({value}); not imported"
    ));
}

fn convert(kind: Kind3, value: &Json) -> std::result::Result<Option<Json>, String> {
    match kind {
        Kind3::Same => {
            if value.is_null() {
                return Ok(None);
            }
            Ok(Some(value.clone()))
        }
        Kind3::Days => match days(value) {
            Some((whole, remainder)) => {
                if remainder {
                    return Err(format!(
                        "is {value} in Mailman; this site counts whole days, so it becomes {whole}"
                    ));
                }
                Ok(Some(json!(whole)))
            }
            None => Err(format!("is {value}, which is not a duration")),
        },
        Kind3::Lines => Ok(Some(json!(lines(value)))),
        Kind3::Float => value.as_f64().map_or_else(
            || Err(format!("is {value}, which is not a number")),
            |number| Ok(Some(json!(number))),
        ),
    }
}

/// Apply `plan` to this site.
///
/// The domains first, then each list with its configuration, bans,
/// header matches and members, every write audited, and one
/// `site.import3` event with the report. What is here already is left
/// alone and counted as skipped, so an import can be run again.
///
/// # Errors
/// Returns the first repository error; what was applied before it stays.
pub async fn apply(db: &Database, plan: &Plan3, context: &AuditContext) -> Result<Report3> {
    let mut report = Report3 {
        warnings: plan.warnings.clone(),
        ..Report3::default()
    };
    for domain in &plan.domains {
        match db.domains().get(&domain.mail_host).await {
            Ok(_) => report.skipped += 1,
            Err(listmngr_core::Error::NotFound(_)) => {
                db.domains()
                    .create_with_context(
                        &domain.mail_host,
                        &domain.description,
                        domain.alias_domain.as_deref(),
                        context,
                    )
                    .await?;
                report.domains += 1;
            }
            Err(error) => return Err(error.into()),
        }
    }
    for user in &plan.users {
        apply_user(db, user, context, &mut report).await?;
    }
    for ban in &plan.site_bans {
        match db.bans().site_get(ban).await {
            Ok(_) => {}
            Err(listmngr_core::Error::NotFound(_)) => {
                db.bans().site_create(ban, context).await?;
                report.site_bans += 1;
            }
            Err(error) => return Err(error.into()),
        }
    }
    for list in &plan.lists {
        apply_list(db, list, context, &mut report).await?;
    }
    db.audit()
        .record_with_context(context, "site.import3", "site", "", report.to_json())
        .await?;
    Ok(report)
}

/// One account: created with its addresses when none of them is here
/// yet, skipped when any already belongs to an account (the site this
/// import writes into wins).
async fn apply_user(
    db: &Database,
    user: &User3,
    context: &AuditContext,
    report: &mut Report3,
) -> Result<()> {
    if user.addresses.is_empty() {
        report.warnings.push(format!(
            "the Mailman account {} has no address and cannot be imported",
            user.user_id
        ));
        return Ok(());
    }
    for address in &user.addresses {
        if db.address_has_account(&address.email).await? {
            report.skipped += 1;
            return Ok(());
        }
    }
    let imported = ImportedUser {
        display_name: user.display_name.clone(),
        is_server_owner: user.is_server_owner,
        locale: user
            .preferences
            .preferred_language
            .clone()
            .unwrap_or_else(|| "en".into()),
        addresses: user
            .addresses
            .iter()
            .map(|address| ImportedAddress {
                email: address.email.clone(),
                display_name: address.display_name.clone(),
                verified: address.verified,
            })
            .collect(),
        preferred: user.preferred.clone(),
    };
    let created = db
        .users()
        .create_imported_with_context(imported, context)
        .await?;
    if user.preferences != Preferences::default() {
        db.preferences()
            .set_user_with_context(created.id, user.preferences.clone(), context)
            .await?;
    }
    report.users += 1;
    report.addresses += user.addresses.len();
    Ok(())
}

async fn apply_list(
    db: &Database,
    list: &ListPlan3,
    context: &AuditContext,
    report: &mut Report3,
) -> Result<()> {
    match db.lists().get(&list.list_id).await {
        Ok(_) => report.skipped += 1,
        Err(listmngr_core::Error::NotFound(_)) => {
            db.lists()
                .create_with_context(
                    NewList {
                        list_id: list.list_id.clone(),
                        display_name: list.display_name.clone(),
                        style: "legacy-default".into(),
                    },
                    context,
                )
                .await?;
            report.lists += 1;
        }
        Err(error) => return Err(error.into()),
    }
    if !list.settings.is_empty() {
        db.lists()
            .update_with_context(&list.list_id, &Json::Object(list.settings.clone()), context)
            .await?;
        report.settings += list.settings.len();
    }
    for ban in &list.bans {
        match db.bans().get(&list.list_id, ban).await {
            Ok(_) => {}
            Err(listmngr_core::Error::NotFound(_)) => {
                db.bans().create(&list.list_id, ban, context).await?;
                report.bans += 1;
            }
            Err(error) => return Err(error.into()),
        }
    }
    let existing = db.header_matches().list(&list.list_id).await?;
    for row in &list.header_matches {
        if existing
            .iter()
            .any(|have| have.header == row.header && have.pattern == row.pattern)
        {
            continue;
        }
        db.header_matches()
            .append(&list.list_id, row.clone(), context)
            .await?;
        report.header_matches += 1;
    }
    apply_members(db, list, context, report).await?;
    apply_waiting(db, list, context, report).await
}

/// What the other site was still holding: the messages, then the
/// requests a moderator has to decide. A message already held here is
/// left alone, and so is a request for an address that is waiting
/// already, so an import can be run again.
async fn apply_waiting(
    db: &Database,
    list: &ListPlan3,
    context: &AuditContext,
    report: &mut Report3,
) -> Result<()> {
    for hold in &list.held {
        match db
            .moderation()
            .hold_imported_with_context(
                ImportedHold {
                    list_id: &list.list_id,
                    raw: &hold.raw,
                    sender: &hold.sender,
                    subject: &hold.subject,
                    reason: &hold.reason,
                    hold_date: hold.hold_date,
                },
                context,
            )
            .await
        {
            Ok(_) => report.held += 1,
            Err(listmngr_core::Error::Conflict(_)) => report.skipped += 1,
            Err(error) => return Err(error.into()),
        }
    }
    if list.requests.is_empty() {
        return Ok(());
    }
    let waiting = db
        .workflows()
        .pending(
            &list.list_id,
            listmngr_db::workflows::RequestFilter::default(),
        )
        .await?;
    for request in &list.requests {
        if waiting
            .iter()
            .any(|pending| pending.email.to_lowercase() == request.email)
        {
            report.skipped += 1;
            continue;
        }
        let action = if request.action == "unsubscription" {
            SubscriptionAction::Leave
        } else {
            SubscriptionAction::Join
        };
        match db
            .workflows()
            .import_request_with_context(
                ImportedRequest {
                    list_id: &list.list_id,
                    email: &request.email,
                    display_name: &request.display_name,
                    action,
                    requested_at: request.requested_at,
                },
                context,
            )
            .await
        {
            Ok(_) => report.requests += 1,
            Err(listmngr_core::Error::Validation(reason)) => report.warnings.push(format!(
                "{}: the waiting request of {} was not imported: {reason}",
                list.list_id.as_str(),
                request.email
            )),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

async fn apply_members(
    db: &Database,
    list: &ListPlan3,
    context: &AuditContext,
    report: &mut Report3,
) -> Result<()> {
    let mut subscribed: Vec<(MemberRole, String)> = Vec::new();
    for role in [
        MemberRole::Member,
        MemberRole::Owner,
        MemberRole::Moderator,
        MemberRole::Nonmember,
    ] {
        for member in db.members().roster(&list.list_id, role).await? {
            let address = db.addresses().get_by_id(member.address_id).await?;
            subscribed.push((role, address.email.to_lowercase()));
        }
    }
    for entry in &list.members {
        if subscribed
            .iter()
            .any(|(role, email)| *role == entry.role && email == &entry.email)
        {
            report.skipped += 1;
            report.warnings.push(format!(
                "{}: {} is already subscribed with role {}",
                list.list_id.as_str(),
                entry.email,
                entry.role.as_str()
            ));
            continue;
        }
        let member = db
            .members()
            .subscribe_with_context(
                NewMember {
                    list_id: list.list_id.clone(),
                    email: entry.email.clone(),
                    role: entry.role,
                    subscription_mode: if entry.subscription_mode == "as_user"
                        && db.address_has_account(&entry.email).await?
                    {
                        SubscriptionMode::AsUser
                    } else {
                        SubscriptionMode::AsAddress
                    },
                    display_name: entry.display_name.clone(),
                },
                true,
                context,
            )
            .await?;
        db.preferences()
            .set_member_with_context(member.id, entry.preferences.clone(), context)
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
        match entry.role {
            MemberRole::Member => report.members += 1,
            MemberRole::Owner => report.owners += 1,
            MemberRole::Moderator => report.moderators += 1,
            MemberRole::Nonmember => report.nonmembers += 1,
        }
    }
    Ok(())
}
