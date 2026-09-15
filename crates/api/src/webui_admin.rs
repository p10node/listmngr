//! Owner-facing bounded member roster and posting policy forms.
#[path = "webui_member_query.rs"]
mod member_query;
use super::{
    ApiResult, AppState, BrowserPage, Form, HeaderMap, IntoResponse, Path, Query, Redirect,
    Response, Shell, State, html, load, reader_language, write_session,
};
use listmngr_core::{ListId, MemberId};
use listmngr_web::{Nav, choices};
use member_query::MemberQuery;
use serde::Deserialize;

const POLICIES: &[(&str, &str)] = &[
    ("default", "web-policy-default"),
    ("defer", "web-policy-defer"),
    ("accept", "web-policy-accept"),
    ("hold", "web-policy-hold"),
    ("reject", "web-policy-reject"),
    ("discard", "web-policy-discard"),
];

pub(super) async fn index(
    State(s): State<AppState>,
    Query(paging): Query<BrowserPage>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let session = load(&s, &headers).await?;
    let rows = s.db.browser_admin_lists(&session, paging.offset()?).await?;
    let more = rows.len() > 20;
    let rows = rows
        .into_iter()
        .take(20)
        .map(|(id, name)| listmngr_web::AdminRow {
            members_href: format!("/web/lists/{id}/members"),
            settings_href: format!("/web/lists/{id}/settings"),
            id,
            name,
        })
        .collect();
    Ok(html(&listmngr_web::AdminIndex {
        shell: Shell::new(
            reader_language(&s, &headers, &session).await?,
            "web-title-admin",
            Nav::Account,
        ),
        rows,
        pagination: paging.pagination("/web/admin", more),
    }))
}

pub(super) async fn members(
    State(s): State<AppState>,
    Path(list): Path<ListId>,
    Query(paging): Query<MemberQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let session = load(&s, &headers).await?;
    let language = reader_language(&s, &headers, &session).await?;
    let rows =
        s.db.browser_search_members(&session, &list, paging.offset()?, &paging.q)
            .await?;
    let more = rows.len() > 20;
    let rows = rows
        .into_iter()
        .take(20)
        .map(|member| {
            let selected = member
                .action
                .map_or_else(|| "default".into(), |a| a.to_string());
            listmngr_web::MemberRow {
                email: member.email,
                action: format!("/web/lists/{}/members/{}/policy", list.as_str(), member.id),
                control: format!("policy-{}", member.id),
                choices: choices(language, POLICIES, Some(&selected)),
            }
        })
        .collect();
    Ok(html(&listmngr_web::Members {
        shell: Shell::new(language, "web-title-members", Nav::Account),
        intro: listmngr_i18n::message(language, "web-members-intro", &[("list", list.as_str())]),
        search_action: format!("/web/lists/{}/members", list.as_str()),
        query: paging.q.clone(),
        clear_href: format!("/web/lists/{}/members", list.as_str()),
        csrf: session.csrf.clone(),
        carried_query: paging.q.clone(),
        carried_page: paging.page,
        rows,
        pagination: paging.pagination(&list, more),
        admin_href: "/web/admin".into(),
    }))
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
}

pub(super) async fn policy(
    State(s): State<AppState>,
    Path((list, member)): Path<(ListId, MemberId)>,
    headers: HeaderMap,
    Form(form): Form<PolicyForm>,
) -> ApiResult<Response> {
    let session = write_session(&s, &headers, &form.csrf).await?;
    let query = MemberQuery {
        q: form.q,
        page: form.page,
    };
    query.offset()?;
    let action = if form.action == "default" {
        None
    } else {
        Some(form.action.parse()?)
    };
    s.db.browser_member_policy(&session, &list, member, action)
        .await?;
    Ok(Redirect::to(&query.url(&list, query.page)).into_response())
}
