//! Verified-session confirmation only; no mailbox challenge or probe is sent.
use super::{
    ApiResult, AppState, Csrf, Form, HeaderMap, IntoResponse, Path, Redirect, Response, Shell,
    State, html, load, reader_language, write_session,
};
use listmngr_web::Nav;
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
    let language = reader_language(&s, &headers, &session).await?;
    let (list, email) = s.db.browser_recover_preview(&session, member).await?;
    Ok(html(&listmngr_web::Recover {
        shell: Shell::new(language, "web-title-recover", Nav::Account),
        csrf: session.csrf.clone(),
        action: format!("/web/members/{member}/recover"),
        prompt: listmngr_i18n::message(
            language,
            "web-recover-prompt",
            &[("email", &email), ("list", list.as_str())],
        ),
    }))
}
