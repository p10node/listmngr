//! Owner-facing bounded member roster and posting policy forms.
#[path = "webui_member_query.rs"]
mod member_query;
use super::{
    ApiResult, AppState, BrowserPage, Form, HeaderMap, IntoResponse, Path, Query, Redirect,
    Response, State, escape, hidden, load, options, page, write_session,
};
use listmngr_core::{ListId, MemberId};
use member_query::MemberQuery;
use serde::Deserialize;
use std::fmt::Write as _;

pub(super) async fn index(
    State(s): State<AppState>,
    Query(paging): Query<BrowserPage>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let session = load(&s, &headers).await?;
    let rows = s.db.browser_admin_lists(&session, paging.offset()?).await?;
    let more = rows.len() > 20;
    let mut body = String::from("<p>Only lists you own or administer are shown.</p><ul>");
    for (id, name) in rows.into_iter().take(20) {
        write!(
            &mut body,
            "<li><a href=\"/web/lists/{}/members\">{} — {}</a> — <a href=\"/web/lists/{}/settings\">List settings</a></li>",
            escape(&id),
            escape(&id),
            escape(&name),
            escape(&id)
        )
        .expect("HTML");
    }
    body.push_str("</ul>");
    body.push_str(&paging.links("/web/admin", more));
    Ok(page("List administration", &body))
}

pub(super) async fn members(
    State(s): State<AppState>,
    Path(list): Path<ListId>,
    Query(paging): Query<MemberQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let session = load(&s, &headers).await?;
    let rows =
        s.db.browser_search_members(&session, &list, paging.offset()?, &paging.q)
            .await?;
    let more = rows.len() > 20;
    let mut body = format!(
        "<p>Members of {}. Overrides affect future posting decisions, not already held or queued mail. Other safety checks still apply.</p>",
        escape(list.as_str())
    );
    write!(&mut body, "<form method=\"get\" action=\"/web/lists/{}/members\"><label for=\"member-search\">Search member email</label><input id=\"member-search\" name=\"q\" value=\"{}\" maxlength=\"320\"><button>Search members</button></form><p><a href=\"/web/lists/{}/members\">Clear search</a></p>", escape(list.as_str()), escape(&paging.q), escape(list.as_str())).expect("HTML");
    if rows.is_empty() {
        body.push_str("<p>No matching members.</p>");
    }
    let form_hidden = hidden(&session) + &paging.hidden();
    for member in rows.into_iter().take(20) {
        let selected = member
            .action
            .map_or_else(|| "default".into(), |a| a.to_string());
        write!(&mut body, "<article><h2>{}</h2><form method=\"post\" action=\"/web/lists/{}/members/{}/policy\">{}<p><label for=\"policy-{member_id}\">Posting policy</label> <select id=\"policy-{member_id}\" name=\"action\">{}</select></p><button>Save posting policy</button></form></article>", escape(&member.email), escape(list.as_str()), member.id, form_hidden, options(&[("default","Use list default"),("defer","Defer (currently accepts after safety checks)"),("accept","Accept"),("hold","Hold for review"),("reject","Reject"),("discard","Discard")], Some(&selected)), member_id=member.id).expect("HTML");
    }
    body.push_str(&paging.links(&list, more));
    body.push_str("<p><a href=\"/web/admin\">List administration</a></p>");
    Ok(page("List members", &body))
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
