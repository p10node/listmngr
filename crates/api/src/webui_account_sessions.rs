//! The reader's own browser sessions: what is signed in, and ending one or all
//! others. Sessions are addressed by an opaque id; the token never appears.
use super::{
    ApiResult, AppState, Csrf, Form, HeaderMap, IntoResponse, Path, Redirect, Response, Shell,
    State, html, language, load, write_session,
};
use listmngr_web::Nav;

/// Unix milliseconds as a readable UTC instant; the browser's own locale
/// formatting would need script, which no page carries.
fn moment(milliseconds: i64) -> String {
    chrono::DateTime::from_timestamp_millis(milliseconds)
        .unwrap_or_default()
        .format("%Y-%m-%d %H:%M UTC")
        .to_string()
}

pub(super) async fn index(State(s): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    let session = load(&s, &headers).await?;
    let language = language(&s, &headers);
    let listed = s.db.browser_sessions(&session).await?;
    let others = listed.iter().any(|item| !item.current);
    let sessions = listed
        .into_iter()
        .map(|item| listmngr_web::SessionRow {
            action: format!("/web/account/sessions/{}/revoke", item.id),
            created: moment(item.created_at),
            expires: moment(item.expires_at),
            current: item.current,
        })
        .collect();
    Ok(html(&listmngr_web::Sessions {
        shell: Shell::new(language, "web-title-sessions", Nav::Account),
        csrf: session.csrf.clone(),
        sessions,
        others,
    }))
}

pub(super) async fn revoke(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(form): Form<Csrf>,
) -> ApiResult<Response> {
    let session = write_session(&s, &headers, &form.csrf).await?;
    let was_current = s.db.browser_revoke_session(&session, &id).await?;
    finish(&s, was_current)
}

pub(super) async fn revoke_others(
    State(s): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<Csrf>,
) -> ApiResult<Response> {
    let session = write_session(&s, &headers, &form.csrf).await?;
    s.db.browser_revoke_other_sessions(&session).await?;
    finish(&s, false)
}

/// Ending the current session clears its cookie and returns to the public
/// directory; ending another returns to the list that no longer has it.
fn finish(s: &AppState, signed_out: bool) -> ApiResult<Response> {
    if !signed_out {
        return Ok(Redirect::to("/web/account/sessions").into_response());
    }
    let mut response = Redirect::to("/web").into_response();
    super::clear_cookie(s, &mut response)?;
    Ok(response)
}
