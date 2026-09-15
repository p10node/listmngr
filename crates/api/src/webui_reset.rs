//! Password reset by mailbox proof: an anonymous, CSRF-anchored, rate-limited
//! request that never says whether the address has an account, and a
//! token-plus-new-password confirmation that ends every earlier session.
use super::{
    ApiError, ApiResult, AppState, Form, HeaderMap, Query, Response, Shell, State, anonymous, html,
    language, now, reader_language, set_cookie, write_session,
};
use axum::http::StatusCode;
use listmngr_core::Error;
use listmngr_web::Nav;
use serde::Deserialize;

pub(super) async fn form(State(s): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    let session = anonymous(&s, &headers).await?;
    let mut response = html(&listmngr_web::ResetRequest {
        shell: Shell::new(
            reader_language(&s, &headers, &session).await?,
            "web-title-reset",
            Nav::Login,
        ),
        csrf: session.csrf.clone(),
    });
    set_cookie(&s, &session, &mut response)?;
    Ok(response)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RequestForm {
    #[serde(default)]
    csrf: String,
    email: String,
}

pub(super) async fn request(
    State(s): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<RequestForm>,
) -> ApiResult<Response> {
    let session = write_session(&s, &headers, &form.csrf).await?;
    s.pre_auth_rate
        .check("web-reset")
        .map_err(|retry_after| ApiError(Error::RateLimited { retry_after }))?;
    if form.email.len() > 320 {
        return Err(Error::Validation("field too long".into()).into());
    }
    let language = language(&s, &headers);
    s.db.browser_reset_request(&session, &form.email, language, now())
        .await?;
    let mut response = html(&listmngr_web::CheckEmail {
        shell: Shell::new(language, "web-title-check-email", Nav::Login),
    });
    *response.status_mut() = StatusCode::ACCEPTED;
    Ok(response)
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(super) struct TokenQuery {
    #[serde(default)]
    token: String,
}

pub(super) async fn confirm_form(
    State(s): State<AppState>,
    Query(query): Query<TokenQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    if query.token.len() > 128 {
        return Err(Error::Validation("token".into()).into());
    }
    let session = anonymous(&s, &headers).await?;
    let mut response = html(&listmngr_web::ResetConfirm {
        shell: Shell::new(
            reader_language(&s, &headers, &session).await?,
            "web-title-reset-confirm",
            Nav::Login,
        ),
        csrf: session.csrf.clone(),
        token: query.token,
    });
    set_cookie(&s, &session, &mut response)?;
    Ok(response)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ConfirmForm {
    #[serde(default)]
    csrf: String,
    token: String,
    password: String,
    confirm_password: String,
}

pub(super) async fn confirm(
    State(s): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<ConfirmForm>,
) -> ApiResult<Response> {
    let session = write_session(&s, &headers, &form.csrf).await?;
    s.pre_auth_rate
        .check("web-reset")
        .map_err(|retry_after| ApiError(Error::RateLimited { retry_after }))?;
    if form.token.len() > 128 || form.password.len() > 1024 {
        return Err(Error::Validation("field too long".into()).into());
    }
    if form.password != form.confirm_password {
        return Err(Error::Validation("password confirmation does not match".into()).into());
    }
    s.db.browser_reset_confirm(&session, &form.token, &form.password, now())
        .await?;
    Ok(html(&listmngr_web::ResetDone {
        shell: Shell::new(language(&s, &headers), "web-title-reset-done", Nav::Login),
    }))
}
