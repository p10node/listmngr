//! Confirmed own-membership departure, independent of list advertisement.
use super::{
    ApiResult, AppState, Csrf, Form, HeaderMap, IntoResponse, Path, Redirect, Response, State,
    escape, hidden, load, page, write_session,
};

pub(super) async fn preview(
    State(s): State<AppState>,
    Path(member): Path<listmngr_core::MemberId>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let session = load(&s, &headers).await?;
    let (list, email) = s.db.browser_leave_preview(&session, member).await?;
    Ok(page(
        "Leave list",
        &format!(
            "<p>Remove the membership for {} on {}? This removes only this subscription, not your account or any owner/moderator role. Already queued mail may still arrive.</p><form method=\"post\" action=\"/web/members/{member}/leave\">{}<button>Leave this list</button></form><p><a href=\"/web/account\">Cancel</a></p>",
            escape(&email),
            escape(list.as_str()),
            hidden(&session)
        ),
    ))
}

pub(super) async fn leave(
    State(s): State<AppState>,
    Path(member): Path<listmngr_core::MemberId>,
    headers: HeaderMap,
    Form(form): Form<Csrf>,
) -> ApiResult<Response> {
    let session = write_session(&s, &headers, &form.csrf).await?;
    s.db.browser_leave(&session, member).await?;
    Ok(Redirect::to("/web/account").into_response())
}
