//! The list directory with its filters, the create-list form and the list
//! summary. Handlers build view models; the templates escape every value.
use super::{
    ApiResult, AppState, BrowserPage, Form, HeaderMap, Path, Query, Redirect, Response, Shell,
    State, anonymous, html, inline_refusal, language, load, now, privileged, reader_language,
    set_cookie, token, write_session,
};
use axum::response::IntoResponse;
use listmngr_core::{Address, Domain, Error, ListId, MemberRole, builtin_styles};
use listmngr_db::web_lists::{BrowserNewList, DirectoryFilter};
use listmngr_db::web_sessions::WebSession;
use listmngr_web::{Choice, Fact, Nav, Pagination, SettingField, choices};
use serde::Deserialize;

/// The directory's query string: a page, a search, a domain and a scope.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(super) struct DirectoryQuery {
    #[serde(default)]
    page: u32,
    #[serde(default)]
    q: String,
    #[serde(default)]
    domain: String,
    #[serde(default)]
    show: String,
}

/// The live session behind a presented cookie, if any. A visitor without one
/// gets none: the directory and the summary's visibility check issue no
/// cookie.
async fn presented(s: &AppState, h: &HeaderMap) -> ApiResult<Option<WebSession>> {
    let Some(t) = token(h) else {
        return Ok(None);
    };
    match s.db.web_session(t, now()).await {
        Ok(session) => Ok(Some(session)),
        Err(Error::Authentication) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// The reader's translated role on a list, when they have one worth showing.
fn badge(language: &str, role: Option<MemberRole>) -> Option<String> {
    let id = match role? {
        MemberRole::Owner => "web-badge-owner",
        MemberRole::Moderator => "web-badge-moderator",
        MemberRole::Member => "web-badge-member",
        MemberRole::Nonmember => return None,
    };
    Some(listmngr_i18n::message(language, id, &[]))
}

/// Whether a signed-in reader may create a list somewhere; a stale session
/// simply may not.
async fn may_create(s: &AppState, session: Option<&WebSession>) -> ApiResult<bool> {
    let Some(session) = session.filter(|session| session.user_id.is_some()) else {
        return Ok(false);
    };
    match s.db.browser_creatable_domains(session).await {
        Ok(domains) => Ok(!domains.is_empty()),
        Err(Error::Authentication) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub(super) async fn directory(
    State(s): State<AppState>,
    Query(f): Query<DirectoryQuery>,
    h: HeaderMap,
) -> ApiResult<Response> {
    if f.q.len() > 200 || f.domain.len() > 253 || !matches!(f.show.as_str(), "" | "all") {
        return Err(Error::Validation("directory filter".into()).into());
    }
    let paging = BrowserPage { page: f.page };
    let session = presented(&s, &h).await?;
    let language = match &session {
        Some(session) => reader_language(&s, &h, session).await?,
        None => language(&s, &h),
    };
    let reader = session.as_ref().and_then(|session| session.user_id);
    let show_all = f.show == "all";
    let rows =
        s.db.browser_directory(
            reader,
            &DirectoryFilter {
                query: &f.q,
                domain: (!f.domain.is_empty()).then_some(f.domain.as_str()),
                show_all,
                offset: paging.offset()?,
            },
        )
        .await?;
    let more = rows.len() > 20;
    let entries = rows
        .into_iter()
        .take(20)
        .map(|row| listmngr_web::DirectoryEntry {
            href: format!("/web/lists/{}", row.id),
            badge: badge(language, row.role),
            unadvertised: !row.advertised,
            name: row.name,
            id: row.id,
            description: row.description,
        })
        .collect();
    let mut domains = vec![Choice {
        value: String::new(),
        label: listmngr_i18n::message(language, "web-directory-any-domain", &[]),
        selected: f.domain.is_empty(),
    }];
    domains.extend(
        s.db.domains()
            .list()
            .await?
            .into_iter()
            .map(|domain| Choice {
                selected: domain.mail_host == f.domain,
                label: domain.mail_host.clone(),
                value: domain.mail_host,
            }),
    );
    let create_href = may_create(&s, session.as_ref())
        .await?
        .then(|| "/web/lists/new".to_owned());
    let mut filters: Vec<(&str, &str)> = Vec::new();
    if !f.q.is_empty() {
        filters.push(("q", &f.q));
    }
    if !f.domain.is_empty() {
        filters.push(("domain", &f.domain));
    }
    if show_all {
        filters.push(("show", "all"));
    }
    let filters = serde_urlencoded::to_string(&filters)
        .map_err(|_| Error::Validation("directory filter".into()))?;
    Ok(html(&listmngr_web::Directory {
        shell: Shell::new(language, "web-title-directory", Nav::Lists),
        entries,
        pagination: Pagination::filtered("/web", &filters, f.page, more, BrowserPage::LAST),
        query: f.q.clone(),
        domains,
        show_all,
        signed_in: reader.is_some(),
        create_href,
    }))
}

pub(super) async fn list_page(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    h: HeaderMap,
) -> ApiResult<Response> {
    // Visibility first, with whatever session the browser presents; a list
    // the reader may not see issues no cookie.
    let reader = presented(&s, &h).await?.and_then(|session| session.user_id);
    let (list, standing) = s.db.browser_list_summary(reader, &id).await?;
    let session = anonymous(&s, &h).await?;
    let language = reader_language(&s, &h, &session).await?;
    let fact = |label: &str, value: String| Fact {
        label: listmngr_i18n::message(language, label, &[]),
        value,
    };
    let mut r = html(&listmngr_web::ListPage {
        shell: Shell::titled(language, list.display_name.clone(), Nav::Lists),
        description: list.description.clone(),
        info: list.info.clone(),
        facts: vec![
            fact("web-list-posting-address", id.posting_address()),
            fact("web-list-owner-address", id.owner_address()),
            fact("web-list-domain", id.mail_host().to_owned()),
            fact("web-list-archive-policy", list.archive_policy.to_string()),
            fact(
                "web-list-subscription-policy",
                list.member_policy.subscription_policy.to_string(),
            ),
        ],
        badge: badge(language, standing.role),
        unadvertised: !list.advertised,
        settings_href: standing
            .administers()
            .then(|| format!("/web/lists/{}/settings", id.as_str())),
        archive_href: (list.archive_policy == listmngr_core::ArchivePolicy::Public)
            .then(|| format!("/web/lists/{}/archive", id.as_str())),
        action: format!("/web/lists/{}/request", id.as_str()),
        csrf: session.csrf.clone(),
        confirm_href: format!("/web/lists/{}/confirm", id.as_str()),
    });
    set_cookie(&s, &session, &mut r)?;
    Ok(r)
}

/// The create form as submitted; every field defaults so a refused form
/// can be shown again with what was typed.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    list_name: String,
    #[serde(default)]
    domain: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    owner: String,
    #[serde(default)]
    style: String,
    #[serde(default)]
    advertised: String,
    #[serde(default)]
    description: String,
}

/// A refusal on one field.
type Refusal = (&'static str, &'static str);

fn render(
    language: &str,
    csrf: &str,
    form: &CreateForm,
    domains: &[Domain],
    refusals: &[Refusal],
    error: Option<String>,
) -> Response {
    let t = |id: &str| listmngr_i18n::message(language, id, &[]);
    let refusal = |name: &str| {
        refusals
            .iter()
            .find(|(field, _)| *field == name)
            .map(|(_, id)| t(id))
    };
    let field = |name: &'static str, kind: &str, value: &str, help: &str| SettingField {
        name: name.to_owned(),
        label: t(&format!("web-create-list-{}", name.replace('_', "-"))),
        help: if help.is_empty() {
            String::new()
        } else {
            t(help)
        },
        kind: kind.to_owned(),
        value: value.to_owned(),
        choices: Vec::new(),
        error: refusal(name),
    };
    let select = |name: &'static str, choices: Vec<Choice>, help: &str| SettingField {
        choices,
        ..field(name, "select", "", help)
    };
    let styles: Vec<Choice> = builtin_styles()
        .iter()
        .map(|style| Choice {
            value: style.name().to_owned(),
            label: style.name().to_owned(),
            selected: style.name() == form.style,
        })
        .collect();
    let fields = vec![
        field(
            "list_name",
            "text",
            &form.list_name,
            "web-create-list-name-help",
        ),
        select(
            "domain",
            domains
                .iter()
                .map(|domain| Choice {
                    value: domain.mail_host.clone(),
                    label: domain.mail_host.clone(),
                    selected: domain.mail_host == form.domain,
                })
                .collect(),
            "",
        ),
        field("display_name", "text", &form.display_name, ""),
        field("owner", "email", &form.owner, "web-create-list-owner-help"),
        select("style", styles, "web-create-list-style-help"),
        select(
            "advertised",
            choices(
                language,
                &[("true", "web-yes"), ("false", "web-no")],
                Some(if form.advertised == "false" {
                    "false"
                } else {
                    "true"
                }),
            ),
            "",
        ),
        field("description", "text", &form.description, ""),
    ];
    html(&listmngr_web::CreateList {
        shell: Shell::new(language, "web-title-create-list", Nav::Account),
        action: "/web/lists/new".into(),
        csrf: csrf.to_owned(),
        fields,
        error,
    })
}

/// The domains the reader may create on; none is a closed door.
async fn creatable(s: &AppState, session: &WebSession) -> ApiResult<Vec<Domain>> {
    let domains = s.db.browser_creatable_domains(session).await?;
    if domains.is_empty() {
        return Err(Error::Forbidden("list creation".into()).into());
    }
    Ok(domains)
}

pub(super) async fn create_form(State(s): State<AppState>, h: HeaderMap) -> ApiResult<Response> {
    let session = load(&s, &h).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &h, &session).await?;
    let domains = creatable(&s, &session).await?;
    // The reader's own first verified address is the offered owner.
    let owner: Option<String> = sqlx::query_scalar("SELECT email FROM addresses WHERE user_id=$1 AND verified_on IS NOT NULL ORDER BY registered_on,email LIMIT 1")
        .bind(session.user_id.ok_or(Error::Authentication)?.to_string())
        .fetch_optional(s.db.pool())
        .await
        .map_err(|error| Error::Database(error.to_string()))?;
    let form = CreateForm {
        owner: owner.unwrap_or_default(),
        style: "legacy-default".into(),
        advertised: "true".into(),
        ..CreateForm::default()
    };
    Ok(render(language, &session.csrf, &form, &domains, &[], None))
}

/// The typing mistakes in a submitted form, each on its field. The domain
/// is checked by the caller, which knows the reader's authority.
fn mistakes(form: &CreateForm, name: &str) -> Vec<Refusal> {
    let mut refusals: Vec<Refusal> = Vec::new();
    if name.is_empty()
        || name.len() > 64
        || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        refusals.push(("list_name", "web-create-list-bad-name"));
    }
    if Address::new(form.owner.trim(), String::new()).is_err() {
        refusals.push(("owner", "web-create-list-bad-owner"));
    }
    if !builtin_styles()
        .iter()
        .any(|style| style.name() == form.style)
    {
        refusals.push(("style", "web-create-list-bad-style"));
    }
    if !matches!(form.advertised.as_str(), "true" | "false") {
        refusals.push(("advertised", "web-create-list-bad-value"));
    }
    refusals
}

pub(super) async fn create(
    State(s): State<AppState>,
    h: HeaderMap,
    Form(form): Form<CreateForm>,
) -> ApiResult<Response> {
    let session = write_session(&s, &h, &form.csrf).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &h, &session).await?;
    let domains = creatable(&s, &session).await?;
    let name = form.list_name.trim().to_ascii_lowercase();
    let mut refusals = mistakes(&form, &name);
    if !domains.iter().any(|domain| domain.mail_host == form.domain) {
        // A domain that exists but is not the reader's is an authority
        // failure, not a typing mistake.
        if s.db.domains().get(&form.domain).await.is_ok() {
            return Err(Error::Forbidden("browser domain owner authority".into()).into());
        }
        refusals.push(("domain", "web-create-list-bad-domain"));
    }
    let list_id = format!("{name}.{}", form.domain).parse::<ListId>();
    if list_id.is_err() && !refusals.iter().any(|(field, _)| *field == "list_name") {
        refusals.push(("list_name", "web-create-list-bad-name"));
    }
    let refuse = |refusals: &[Refusal], error: Option<String>| {
        Ok(inline_refusal(render(
            language,
            &session.csrf,
            &form,
            &domains,
            refusals,
            error,
        )))
    };
    let Ok(list_id) = list_id else {
        return refuse(&refusals, None);
    };
    if !refusals.is_empty() {
        return refuse(&refusals, None);
    }
    let display_name = form.display_name.trim();
    let new = BrowserNewList {
        list_id,
        display_name: if display_name.is_empty() {
            name.clone()
        } else {
            display_name.to_owned()
        },
        style: form.style.clone(),
        owner: form.owner.trim().to_owned(),
        advertised: form.advertised == "true",
        description: form.description.trim().to_owned(),
    };
    match s.db.browser_create_list(&session, new).await {
        Ok(list) => {
            crate::refresh_mta_maps(&s).await;
            Ok(Redirect::to(&format!("/web/lists/{}/settings", list.id)).into_response())
        }
        Err(Error::Conflict(_)) => refuse(&[("list_name", "web-create-list-exists")], None),
        Err(Error::NotFound(_)) => refuse(&[("domain", "web-create-list-bad-domain")], None),
        Err(Error::Validation(reason)) => refuse(
            &[],
            Some(listmngr_i18n::message(
                language,
                "web-create-list-refused",
                &[("reason", &reason)],
            )),
        ),
        Err(error) => Err(error.into()),
    }
}
