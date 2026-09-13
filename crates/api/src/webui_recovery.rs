//! Verified-session confirmation only; no mailbox challenge or probe is sent.
use super::{
    ApiResult, AppState, Csrf, Form, HeaderMap, IntoResponse, Path, Redirect, Response, State,
    escape, hidden, load, page, write_session,
};
pub(super) async fn recover(
    State(s): State<AppState>,
    Path(member): Path<listmngr_core::MemberId>,
    headers: HeaderMap,
    Form(form): Form<Csrf>,
) -> ApiResult<Response> {
    let session = write_session(&s, &headers, &form.csrf).await?;
    s.db.browser_recover(&session, member).await?;
    Ok(Redirect::to("/web/account").into_response())
}
pub(super) async fn preview(
    State(s): State<AppState>,
    Path(member): Path<listmngr_core::MemberId>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let session = load(&s, &headers).await?;
    let (list, email) = s.db.browser_recover_preview(&session, member).await?;
    Ok(page(
        "Restore delivery",
        &format!(
            "<p>Verify that your mailbox is working before restoring delivery for {} on {}. This resets this subscription's bounce score and warning cycle. No mailbox challenge or probe is sent.</p><form method=\"post\" action=\"/web/members/{member}/recover\">{}<button>Restore delivery</button></form><p><a href=\"/web/account\">Cancel</a></p>",
            escape(&email),
            escape(list.as_str()),
            hidden(&session)
        ),
    ))
}
