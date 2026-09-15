//! The owner's member management pages: rosters of every role with search
//! (an htmx fragment or the whole page), one member's options and bounce
//! state, mass subscription from a textarea or a file, mass removal, and
//! the CSV export.
use super::{
    ApiResult, AppState, BrowserPage, Form, HeaderMap, IntoResponse, Path, Query, Redirect,
    Response, Shell, State, check_origin, html, inline_refusal, load, privileged, reader_language,
    scripted, write_session,
};
use axum::extract::{FromRequest, Multipart, Request};
use axum::http::header;
use listmngr_core::{
    DeliveryMode, DeliveryStatus, Error, ListId, MemberId, MemberRole, ModerationAction,
    Preferences,
};
use listmngr_db::{MassFlags, MassOutcome, MemberDetail, MemberOptions};
use listmngr_web::{
    Fact, Flag, GroupLink, MassSubscribe, MemberOptionsPage, MemberRow, Members, Nav, Pagination,
    Roster, SettingField, choices,
};
use serde::Deserialize;
use std::collections::HashMap;

type Options = &'static [(&'static str, &'static str)];

const POLICIES: Options = &[
    ("default", "web-policy-default"),
    ("defer", "web-policy-defer"),
    ("accept", "web-policy-accept"),
    ("hold", "web-policy-hold"),
    ("reject", "web-policy-reject"),
    ("discard", "web-policy-discard"),
];
const ROLES: Options = &[
    ("member", "web-role-member"),
    ("owner", "web-role-owner"),
    ("moderator", "web-role-moderator"),
    ("nonmember", "web-role-nonmember"),
];
const TRISTATE: Options = &[
    ("default", "web-member-inherit"),
    ("true", "web-yes"),
    ("false", "web-no"),
];
const DELIVERY_MODES: Options = &[
    ("default", "web-member-inherit"),
    ("regular", "web-delivery-regular"),
    ("plaintext_digests", "web-delivery-plaintext"),
    ("mime_digests", "web-delivery-mime"),
    ("summary_digests", "web-delivery-summary"),
];
const DELIVERY_STATUSES: Options = &[
    ("default", "web-member-inherit"),
    ("enabled", "web-status-enabled"),
    ("by_user", "web-status-paused"),
    ("by_moderator", "web-status-by-moderator"),
    ("by_bounces", "web-status-by-bounces"),
];
const LANGUAGES: Options = &[
    ("default", "web-member-inherit"),
    ("en", "web-language-en"),
    ("vi", "web-language-vi"),
];

/// The most addresses one mass form takes.
const MASS_LIMIT: usize = 1000;
/// The most bytes an uploaded address file may have.
const FILE_LIMIT: usize = 256 * 1024;

fn roster_base(list: &ListId) -> String {
    format!("/web/lists/{}/members", list.as_str())
}

fn parse_role(value: &str) -> ApiResult<MemberRole> {
    if value.is_empty() {
        return Ok(MemberRole::Member);
    }
    ROLES
        .iter()
        .find(|(name, _)| *name == value)
        .map(|_| value.parse())
        .transpose()?
        .ok_or_else(|| Error::Validation("role".into()).into())
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(super) struct RosterQuery {
    #[serde(default)]
    pub page: u32,
    #[serde(default)]
    pub q: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub removed: Option<usize>,
}

impl RosterQuery {
    fn offset(&self) -> ApiResult<i64> {
        if self.q.len() > 320 || self.q.chars().any(char::is_control) {
            return Err(Error::Validation("invalid member search".into()).into());
        }
        BrowserPage { page: self.page }.offset()
    }
    fn url(&self, list: &ListId, page: u32) -> String {
        let base = roster_base(list);
        let mut pairs: Vec<(&str, String)> = Vec::new();
        if !self.role.is_empty() && self.role != "member" {
            pairs.push(("role", self.role.clone()));
        }
        if !self.q.is_empty() {
            pairs.push(("q", self.q.clone()));
        }
        if page > 0 {
            pairs.push(("page", page.to_string()));
        }
        if pairs.is_empty() {
            return base;
        }
        format!(
            "{base}?{}",
            serde_urlencoded::to_string(pairs).expect("string query")
        )
    }
    fn pagination(&self, list: &ListId, more: bool) -> Pagination {
        Pagination {
            previous: self.page.checked_sub(1).map(|page| self.url(list, page)),
            next: (more && self.page < BrowserPage::LAST).then(|| self.url(list, self.page + 1)),
        }
    }
}

fn delivery_summary(language: &str, mode: Option<&str>, status: Option<&str>) -> String {
    if mode.is_none() && status.is_none() {
        return String::new();
    }
    listmngr_i18n::message(
        language,
        "web-members-delivery",
        &[
            ("mode", mode.unwrap_or("-")),
            ("status", status.unwrap_or("-")),
        ],
    )
}

/// `GET /web/lists/{id}/members`: the roster page, or its fragment for htmx.
pub(super) async fn roster(
    State(s): State<AppState>,
    Path(list): Path<ListId>,
    Query(query): Query<RosterQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let session = load(&s, &headers).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &headers, &session).await?;
    let role = parse_role(&query.role)?;
    let rows =
        s.db.browser_roster(&session, &list, role, query.offset()?, &query.q)
            .await?;
    let more = rows.len() > 20;
    let rows = rows
        .into_iter()
        .take(20)
        .map(|member| {
            let selected = member
                .moderation_action
                .map_or_else(|| "default".into(), |a| a.to_string());
            MemberRow {
                id: member.id.to_string(),
                href: format!("{}/{}", roster_base(&list), member.id),
                action: format!("{}/{}/policy", roster_base(&list), member.id),
                control: format!("policy-{}", member.id),
                choices: choices(language, POLICIES, Some(&selected)),
                delivery: delivery_summary(
                    language,
                    member.delivery_mode.as_deref(),
                    member.delivery_status.as_deref(),
                ),
                bounce: (member.bounce_score > 0.0).then(|| format!("{:.1}", member.bounce_score)),
                email: member.email,
                display_name: member.display_name,
            }
        })
        .collect();
    let shell = Shell::new(language, "web-title-members", Nav::Account).with_htmx();
    let roster = Roster {
        shell: shell.clone(),
        csrf: session.csrf.clone(),
        carried_query: query.q.clone(),
        carried_page: query.page,
        carried_role: role.as_str().into(),
        rows,
        pagination: query.pagination(&list, more),
    };
    if headers
        .get("hx-request")
        .is_some_and(|value| value == "true")
    {
        return Ok(scripted(html(&roster)));
    }
    let base = roster_base(&list);
    let roles = ROLES
        .iter()
        .map(|(name, id)| GroupLink {
            href: if *name == "member" {
                base.clone()
            } else {
                format!("{base}?role={name}")
            },
            label: listmngr_i18n::message(language, id, &[]),
            current: *name == role.as_str(),
        })
        .collect();
    let notice = query.removed.map(|count| {
        listmngr_i18n::message(
            language,
            "web-members-removed",
            &[("count", &count.to_string())],
        )
    });
    Ok(scripted(html(&Members {
        intro: listmngr_i18n::message(language, "web-members-intro", &[("list", list.as_str())]),
        roster,
        roles,
        role: role.as_str().into(),
        search_action: base.clone(),
        query: query.q.clone(),
        clear_href: if role == MemberRole::Member {
            base.clone()
        } else {
            format!("{base}?role={}", role.as_str())
        },
        csrf: session.csrf.clone(),
        subscribe_href: format!("{base}/subscribe"),
        export_href: format!(
            "{base}/export.csv{}",
            if role == MemberRole::Member {
                String::new()
            } else {
                format!("?role={}", role.as_str())
            }
        ),
        remove_action: format!("{base}/remove"),
        notice,
        admin_href: "/web/admin".into(),
        shell,
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PolicyForm {
    csrf: String,
    action: String,
    #[serde(default)]
    q: String,
    #[serde(default)]
    page: u32,
    #[serde(default)]
    role: String,
}

/// `POST /web/lists/{id}/members/{member}/policy`: the roster's inline
/// posting override.
pub(super) async fn policy(
    State(s): State<AppState>,
    Path((list, member)): Path<(ListId, MemberId)>,
    headers: HeaderMap,
    Form(form): Form<PolicyForm>,
) -> ApiResult<Response> {
    let session = write_session(&s, &headers, &form.csrf).await?;
    privileged(&s, &session).await?;
    let query = RosterQuery {
        q: form.q,
        page: form.page,
        role: form.role,
        removed: None,
    };
    query.offset()?;
    parse_role(&query.role)?;
    let action = if form.action == "default" {
        None
    } else {
        Some(form.action.parse()?)
    };
    s.db.browser_member_policy(&session, &list, member, action)
        .await?;
    Ok(Redirect::to(&query.url(&list, query.page)).into_response())
}

// ----- one member ------------------------------------------------------------

fn option_text<T: ToString>(value: Option<T>) -> String {
    value.map_or_else(|| "default".into(), |v| v.to_string())
}

fn tristate(value: Option<bool>) -> String {
    value.map_or_else(|| "default".into(), |v| v.to_string())
}

fn stamp(language: &str, at: Option<chrono::DateTime<chrono::Utc>>) -> String {
    at.map_or_else(
        || listmngr_i18n::message(language, "web-ls-never", &[]),
        |at| at.format("%Y-%m-%d %H:%M UTC").to_string(),
    )
}

/// The options form from stored values or a submitted form.
fn option_fields(
    language: &str,
    values: &dyn Fn(&str) -> String,
    errors: &HashMap<&str, String>,
) -> Vec<SettingField> {
    let specs: [(&str, Options, bool); 10] = [
        ("moderation_action", POLICIES, true),
        ("display_name", &[], false),
        ("role", ROLES, true),
        ("delivery_mode", DELIVERY_MODES, false),
        ("delivery_status", DELIVERY_STATUSES, true),
        ("acknowledge_posts", TRISTATE, false),
        ("hide_address", TRISTATE, false),
        ("receive_list_copy", TRISTATE, false),
        ("receive_own_postings", TRISTATE, false),
        ("preferred_language", LANGUAGES, false),
    ];
    specs
        .iter()
        .map(|(name, options, help)| {
            let value = values(name);
            SettingField {
                name: (*name).to_owned(),
                label: listmngr_i18n::message(
                    language,
                    &format!("web-member-{}", name.replace('_', "-")),
                    &[],
                ),
                help: if *help {
                    listmngr_i18n::message(
                        language,
                        &format!("web-member-{}-help", name.replace('_', "-")),
                        &[],
                    )
                } else {
                    String::new()
                },
                kind: if options.is_empty() { "text" } else { "select" }.into(),
                choices: if options.is_empty() {
                    Vec::new()
                } else {
                    choices(language, options, Some(&value))
                },
                value,
                error: errors.get(name).cloned(),
            }
        })
        .collect()
}

fn effective_facts(language: &str, resolved: &Preferences) -> Vec<Fact> {
    let yes_no = |value: Option<bool>| {
        value.map_or_else(
            || "-".to_owned(),
            |v| listmngr_i18n::message(language, if v { "web-yes" } else { "web-no" }, &[]),
        )
    };
    [
        ("delivery_mode", option_text(resolved.delivery_mode)),
        ("delivery_status", option_text(resolved.delivery_status)),
        ("acknowledge_posts", yes_no(resolved.acknowledge_posts)),
        ("hide_address", yes_no(resolved.hide_address)),
        ("receive_list_copy", yes_no(resolved.receive_list_copy)),
        (
            "receive_own_postings",
            yes_no(resolved.receive_own_postings),
        ),
        (
            "preferred_language",
            resolved.preferred_language.clone().unwrap_or_default(),
        ),
    ]
    .into_iter()
    .map(|(name, value)| Fact {
        label: listmngr_i18n::message(
            language,
            &format!("web-member-{}", name.replace('_', "-")),
            &[],
        ),
        value: if value == "default" {
            "-".into()
        } else {
            value
        },
    })
    .collect()
}

#[allow(clippy::too_many_arguments)]
fn render_member(
    language: &str,
    csrf: &str,
    list: &ListId,
    member: MemberId,
    detail: &MemberDetail,
    values: &dyn Fn(&str) -> String,
    errors: &HashMap<&str, String>,
    notice: Option<String>,
    error: Option<String>,
) -> Response {
    let base = format!("{}/{}", roster_base(list), member);
    html(&MemberOptionsPage {
        shell: Shell::new(language, "web-title-member", Nav::Account),
        email: detail.email.clone(),
        roster_href: roster_base(list),
        action: base.clone(),
        csrf: csrf.to_owned(),
        facts: vec![
            Fact {
                label: listmngr_i18n::message(language, "web-member-fact-mode", &[]),
                value: detail.member.subscription_mode.to_string(),
            },
            Fact {
                label: listmngr_i18n::message(language, "web-member-fact-since", &[]),
                value: stamp(language, Some(detail.member.created_at)),
            },
        ],
        fields: option_fields(language, values, errors),
        effective: effective_facts(language, &detail.resolved),
        bounce_score: format!("{:.1}", detail.member.bounce_score),
        last_bounce: stamp(language, detail.member.last_bounce_received),
        bounced: detail.preferences.delivery_status == Some(DeliveryStatus::ByBounces),
        bounce_action: format!("{base}/bounce/reset"),
        remove_action: format!("{base}/remove"),
        notice,
        error,
    })
}

fn stored_values(detail: &MemberDetail) -> impl Fn(&str) -> String + '_ {
    move |name: &str| match name {
        "moderation_action" => option_text(detail.member.moderation_action),
        "display_name" => detail.member.display_name.clone(),
        "role" => detail.member.role.to_string(),
        "delivery_mode" => option_text(detail.preferences.delivery_mode),
        "delivery_status" => option_text(detail.preferences.delivery_status),
        "acknowledge_posts" => tristate(detail.preferences.acknowledge_posts),
        "hide_address" => tristate(detail.preferences.hide_address),
        "receive_list_copy" => tristate(detail.preferences.receive_list_copy),
        "receive_own_postings" => tristate(detail.preferences.receive_own_postings),
        "preferred_language" => detail
            .preferences
            .preferred_language
            .clone()
            .unwrap_or_else(|| "default".into()),
        _ => String::new(),
    }
}

#[derive(Deserialize)]
pub(super) struct MemberNotice {
    #[serde(default)]
    saved: String,
}

/// `GET /web/lists/{id}/members/{member}`.
pub(super) async fn member(
    State(s): State<AppState>,
    Path((list, member)): Path<(ListId, MemberId)>,
    Query(q): Query<MemberNotice>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let session = load(&s, &headers).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &headers, &session).await?;
    let detail = s.db.browser_member_detail(&session, &list, member).await?;
    let notice = match q.saved.as_str() {
        "1" => Some(listmngr_i18n::message(language, "web-ls-saved", &[])),
        "bounce" => Some(listmngr_i18n::message(
            language,
            "web-member-bounce-done",
            &[],
        )),
        _ => None,
    };
    Ok(render_member(
        language,
        &session.csrf,
        &list,
        member,
        &detail,
        &stored_values(&detail),
        &HashMap::new(),
        notice,
        None,
    ))
}

fn parse_tristate(value: &str) -> Result<Option<bool>, ()> {
    match value {
        "default" => Ok(None),
        "true" => Ok(Some(true)),
        "false" => Ok(Some(false)),
        _ => Err(()),
    }
}

/// `default` is none; anything else must parse.
fn optional<T: std::str::FromStr>(
    value: &str,
    name: &'static str,
    refused: &mut Vec<&'static str>,
) -> Option<T> {
    if value == "default" {
        return None;
    }
    value.parse::<T>().map_or_else(
        |_| {
            refused.push(name);
            None
        },
        Some,
    )
}

/// The options a form asks for, or the fields that were refused.
fn parse_options(form: &HashMap<String, String>) -> Result<MemberOptions, Vec<&'static str>> {
    let get = |name: &str| form.get(name).map(String::as_str).unwrap_or_default();
    let mut refused = Vec::new();
    let moderation_action =
        optional::<ModerationAction>(get("moderation_action"), "moderation_action", &mut refused);
    let display_name = get("display_name").trim().to_owned();
    if display_name.chars().count() > 256 || display_name.chars().any(char::is_control) {
        refused.push("display_name");
    }
    let role = get("role").parse::<MemberRole>().unwrap_or_else(|_| {
        refused.push("role");
        MemberRole::Member
    });
    let delivery_mode =
        optional::<DeliveryMode>(get("delivery_mode"), "delivery_mode", &mut refused);
    let delivery_status =
        optional::<DeliveryStatus>(get("delivery_status"), "delivery_status", &mut refused);
    let mut flag = |name: &'static str| {
        parse_tristate(get(name)).unwrap_or_else(|()| {
            refused.push(name);
            None
        })
    };
    let acknowledge_posts = flag("acknowledge_posts");
    let hide_address = flag("hide_address");
    let receive_list_copy = flag("receive_list_copy");
    let receive_own_postings = flag("receive_own_postings");
    let preferred_language = match get("preferred_language") {
        "default" => None,
        value if LANGUAGES.iter().any(|(code, _)| *code == value) => Some(value.to_owned()),
        _ => {
            refused.push("preferred_language");
            None
        }
    };
    if !refused.is_empty() {
        return Err(refused);
    }
    Ok(MemberOptions {
        moderation_action,
        display_name,
        role,
        preferences: Preferences {
            acknowledge_posts,
            hide_address,
            preferred_language,
            receive_list_copy,
            receive_own_postings,
            delivery_mode,
            delivery_status,
        },
    })
}

/// `POST /web/lists/{id}/members/{member}`: save the options.
pub(super) async fn member_save(
    State(s): State<AppState>,
    Path((list, member)): Path<(ListId, MemberId)>,
    headers: HeaderMap,
    Form(form): Form<HashMap<String, String>>,
) -> ApiResult<Response> {
    let csrf = form.get("csrf").cloned().unwrap_or_default();
    let session = write_session(&s, &headers, &csrf).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &headers, &session).await?;
    let detail = s.db.browser_member_detail(&session, &list, member).await?;
    let submitted = |name: &str| form.get(name).cloned().unwrap_or_default();
    match parse_options(&form) {
        Ok(options) => {
            s.db.browser_member_options(&session, &list, member, &options)
                .await?;
            Ok(Redirect::to(&format!("{}/{member}?saved=1", roster_base(&list))).into_response())
        }
        Err(refused) => {
            let errors = refused
                .into_iter()
                .map(|name| {
                    (
                        name,
                        listmngr_i18n::message(language, "web-ls-error-choice", &[]),
                    )
                })
                .collect();
            Ok(inline_refusal(render_member(
                language,
                &session.csrf,
                &list,
                member,
                &detail,
                &submitted,
                &errors,
                None,
                None,
            )))
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Csrf {
    #[serde(default)]
    csrf: String,
}

/// `POST /web/lists/{id}/members/{member}/bounce/reset`.
pub(super) async fn bounce_reset(
    State(s): State<AppState>,
    Path((list, member)): Path<(ListId, MemberId)>,
    headers: HeaderMap,
    Form(form): Form<Csrf>,
) -> ApiResult<Response> {
    let session = write_session(&s, &headers, &form.csrf).await?;
    privileged(&s, &session).await?;
    s.db.browser_member_bounce_reset(&session, &list, member)
        .await?;
    Ok(Redirect::to(&format!("{}/{member}?saved=bounce", roster_base(&list))).into_response())
}

/// `POST /web/lists/{id}/members/{member}/remove`.
pub(super) async fn member_remove(
    State(s): State<AppState>,
    Path((list, member)): Path<(ListId, MemberId)>,
    headers: HeaderMap,
    Form(form): Form<Csrf>,
) -> ApiResult<Response> {
    let session = write_session(&s, &headers, &form.csrf).await?;
    privileged(&s, &session).await?;
    let removed =
        s.db.browser_mass_remove(&session, &list, &[member], &[])
            .await?;
    Ok(Redirect::to(&format!("{}?removed={removed}", roster_base(&list))).into_response())
}

// ----- mass removal ----------------------------------------------------------

/// `POST /web/lists/{id}/members/remove`: the selected rows and the pasted
/// addresses. The body is read as repeated `member=` pairs, which `Form`
/// into a map would collapse.
pub(super) async fn mass_remove(
    State(s): State<AppState>,
    Path(list): Path<ListId>,
    headers: HeaderMap,
    body: String,
) -> ApiResult<Response> {
    let pairs: Vec<(String, String)> =
        serde_urlencoded::from_str(&body).map_err(|_| Error::Validation("form".into()))?;
    let field = |name: &str| {
        pairs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
            .unwrap_or_default()
    };
    let session = write_session(&s, &headers, &field("csrf")).await?;
    privileged(&s, &session).await?;
    let ids: Vec<MemberId> = pairs
        .iter()
        .filter(|(key, _)| key == "member")
        .map(|(_, value)| value.parse::<MemberId>())
        .collect::<Result<_, _>>()
        .map_err(|_| Error::Validation("member".into()))?;
    let emails: Vec<String> = field("addresses")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(String::from)
        .collect();
    if ids.len() + emails.len() > MASS_LIMIT {
        return Err(Error::Validation("too many members at once".into()).into());
    }
    let removed =
        s.db.browser_mass_remove(&session, &list, &ids, &emails)
            .await?;
    Ok(Redirect::to(&format!("{}?removed={removed}", roster_base(&list))).into_response())
}

// ----- mass subscription ----------------------------------------------------

fn flags(language: &str, values: &dyn Fn(&str) -> bool) -> Vec<Flag> {
    [
        "pre_verified",
        "pre_confirmed",
        "pre_approved",
        "invitation",
    ]
    .into_iter()
    .map(|name| Flag {
        name: name.into(),
        label: listmngr_i18n::message(
            language,
            &format!("web-mass-{}", name.replace('_', "-")),
            &[],
        ),
        help: listmngr_i18n::message(
            language,
            &format!("web-mass-{}-help", name.replace('_', "-")),
            &[],
        ),
        checked: values(name),
    })
    .collect()
}

#[allow(clippy::too_many_arguments)]
fn render_mass(
    language: &str,
    csrf: &str,
    list: &ListId,
    role: &str,
    flag_values: &dyn Fn(&str) -> bool,
    addresses: String,
    outcomes: Vec<Fact>,
    error: Option<String>,
) -> Response {
    html(&MassSubscribe {
        shell: Shell::new(language, "web-title-mass-subscribe", Nav::Account),
        roster_href: roster_base(list),
        action: format!("{}/subscribe", roster_base(list)),
        csrf: csrf.to_owned(),
        roles: choices(language, ROLES, Some(role)),
        flags: flags(language, flag_values),
        addresses,
        outcomes,
        error,
    })
}

/// `GET /web/lists/{id}/members/subscribe`.
pub(super) async fn mass_form(
    State(s): State<AppState>,
    Path(list): Path<ListId>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let session = load(&s, &headers).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &headers, &session).await?;
    s.db.browser_list_settings(&session, &list).await?;
    Ok(render_mass(
        language,
        &session.csrf,
        &list,
        "member",
        &|name| name == "pre_verified",
        String::new(),
        Vec::new(),
        None,
    ))
}

/// The fields of the mass form, from either encoding.
async fn mass_fields(s: &AppState, request: Request) -> ApiResult<HashMap<String, String>> {
    let multipart = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("multipart/form-data"));
    let mut fields: HashMap<String, String> = HashMap::new();
    if multipart {
        let mut parts = Multipart::from_request(request, s)
            .await
            .map_err(|_| Error::Validation("form".into()))?;
        while let Some(part) = parts
            .next_field()
            .await
            .map_err(|_| Error::Validation("form".into()))?
        {
            let name = part.name().unwrap_or_default().to_owned();
            let bytes = part
                .bytes()
                .await
                .map_err(|_| Error::Validation("form".into()))?;
            if bytes.len() > FILE_LIMIT {
                return Err(Error::Validation("the file is too large".into()).into());
            }
            let text = String::from_utf8_lossy(&bytes).into_owned();
            if name == "file" {
                let existing = fields.entry("addresses".to_owned()).or_default();
                if !existing.is_empty() && !text.is_empty() {
                    existing.push('\n');
                }
                existing.push_str(&text);
            } else {
                fields.insert(name, text);
            }
        }
    } else {
        let Form(form): Form<HashMap<String, String>> = Form::from_request(request, s)
            .await
            .map_err(|_| Error::Validation("form".into()))?;
        fields = form;
    }
    Ok(fields)
}

/// `Name <email>` or a bare address, one per line.
fn parse_entries(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            let line = line.trim_matches(',').trim();
            if let Some(open) = line.rfind('<')
                && line.ends_with('>')
            {
                let email = line[open + 1..line.len() - 1].trim().to_owned();
                let name = line[..open].trim().trim_matches('"').to_owned();
                (email, name)
            } else {
                (line.to_owned(), String::new())
            }
        })
        .collect()
}

/// `POST /web/lists/{id}/members/subscribe`.
pub(super) async fn mass_subscribe(
    State(s): State<AppState>,
    Path(list): Path<ListId>,
    request: Request,
) -> ApiResult<Response> {
    let headers = request.headers().clone();
    check_origin(&s, &headers)?;
    let fields = mass_fields(&s, request).await?;
    let get = |name: &str| fields.get(name).cloned().unwrap_or_default();
    let session = write_session(&s, &headers, &get("csrf")).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &headers, &session).await?;
    let role_text = if get("role").is_empty() {
        "member".to_owned()
    } else {
        get("role")
    };
    let role = parse_role(&role_text)?;
    let flag_values = |name: &str| get(name) == "true";
    let mass = MassFlags {
        pre_verified: flag_values("pre_verified"),
        pre_confirmed: flag_values("pre_confirmed"),
        pre_approved: flag_values("pre_approved"),
        invitation: flag_values("invitation"),
    };
    let addresses = get("addresses");
    let entries = parse_entries(&addresses);
    if entries.len() > MASS_LIMIT {
        return Ok(inline_refusal(render_mass(
            language,
            &session.csrf,
            &list,
            role.as_str(),
            &flag_values,
            addresses,
            Vec::new(),
            Some(listmngr_i18n::message(language, "web-mass-too-many", &[])),
        )));
    }
    // A line repeated in the same submission is reported, not subscribed twice.
    let mut seen = std::collections::HashSet::new();
    let mut outcomes: Vec<Fact> = Vec::with_capacity(entries.len());
    let mut fresh = Vec::with_capacity(entries.len());
    for (email, name) in entries {
        if seen.insert(email.to_lowercase()) {
            fresh.push((email, name));
        } else {
            outcomes.push(Fact {
                label: email,
                value: listmngr_i18n::message(language, "web-mass-duplicate", &[]),
            });
        }
    }
    let results =
        s.db.browser_mass_subscribe(&session, &list, &fresh, role, mass, super::now())
            .await?;
    for (email, outcome) in results {
        let value = match outcome {
            MassOutcome::Subscribed => listmngr_i18n::message(language, "web-mass-subscribed", &[]),
            MassOutcome::Held => listmngr_i18n::message(language, "web-mass-held", &[]),
            MassOutcome::AlreadyMember => listmngr_i18n::message(language, "web-mass-already", &[]),
            MassOutcome::Invalid => listmngr_i18n::message(language, "web-mass-invalid", &[]),
            MassOutcome::Refused(message) => {
                listmngr_i18n::message(language, "web-mass-refused", &[("reason", &message)])
            }
        };
        outcomes.push(Fact {
            label: email,
            value,
        });
    }
    Ok(render_mass(
        language,
        &session.csrf,
        &list,
        role.as_str(),
        &flag_values,
        String::new(),
        outcomes,
        None,
    ))
}

// ----- export ------------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct ExportQuery {
    #[serde(default)]
    role: String,
}

fn csv_cell(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

/// `GET /web/lists/{id}/members/export.csv?role=`.
pub(super) async fn export(
    State(s): State<AppState>,
    Path(list): Path<ListId>,
    Query(q): Query<ExportQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let session = load(&s, &headers).await?;
    privileged(&s, &session).await?;
    let role = parse_role(&q.role)?;
    let rows = s.db.browser_export_members(&session, &list, role).await?;
    let mut csv = String::from(
        "email,display_name,role,subscription_mode,delivery_mode,delivery_status,moderation_action,bounce_score,last_bounce_received,created_at\r\n",
    );
    for row in rows {
        csv.push_str(
            &[
                csv_cell(&row.email),
                csv_cell(&row.display_name),
                csv_cell(&row.role),
                csv_cell(&row.subscription_mode),
                csv_cell(&row.delivery_mode),
                csv_cell(&row.delivery_status),
                csv_cell(&row.moderation_action),
                format!("{:.1}", row.bounce_score),
                csv_cell(&row.last_bounce_received),
                csv_cell(&row.created_at),
            ]
            .join(","),
        );
        csv.push_str("\r\n");
    }
    let mut response = csv.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/csv; charset=utf-8"),
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        header::HeaderValue::from_str(&format!(
            "attachment; filename=\"{}-{}.csv\"",
            list.as_str(),
            role.as_str()
        ))
        .map_err(|_| Error::Validation("filename".into()))?,
    );
    Ok(response)
}
