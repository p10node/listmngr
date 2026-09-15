//! Self-service account creation and the mailed-token verification that
//! turns the account on. Both are anonymous, CSRF-anchored and rate limited;
//! the signup response never says whether the address already had an account.
use super::{
    ApiError, ApiResult, AppState, Form, HeaderMap, Query, Response, Shell, State, anonymous, html,
    language, now, reader_language, set_cookie, write_session,
};
use axum::http::StatusCode;
use listmngr_core::Error;
use listmngr_web::Nav;
use serde::Deserialize;

fn offered(s: &AppState) -> ApiResult<()> {
    if s.config.web.signup {
        Ok(())
    } else {
        Err(Error::NotFound("signup".into()).into())
    }
}

pub(super) async fn form(State(s): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    offered(&s)?;
    let session = anonymous(&s, &headers).await?;
    let mut response = html(&listmngr_web::SignupPage {
        shell: Shell::new(
            reader_language(&s, &headers, &session).await?,
            "web-title-signup",
            Nav::Login,
        ),
        csrf: session.csrf.clone(),
    });
    set_cookie(&s, &session, &mut response)?;
    Ok(response)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SignupForm {
    #[serde(default)]
    csrf: String,
    email: String,
    display_name: String,
    password: String,
    confirm_password: String,
}

pub(super) async fn create(
    State(s): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<SignupForm>,
) -> ApiResult<Response> {
    offered(&s)?;
    let session = write_session_anonymous(&s, &headers, &form.csrf).await?;
    s.pre_auth_rate
        .check("web-signup")
        .map_err(|retry_after| ApiError(Error::RateLimited { retry_after }))?;
    if form.password != form.confirm_password {
        return Err(Error::Validation("password confirmation does not match".into()).into());
    }
    if form.password.len() > 1024 || form.email.len() > 320 {
        return Err(Error::Validation("field too long".into()).into());
    }
    let language = language(&s, &headers);
    s.db.browser_signup(
        &session,
        &listmngr_db::Signup {
            email: form.email,
            display_name: form.display_name,
            password: form.password,
            language: language.to_owned(),
        },
        now(),
    )
    .await?;
    let mut response = html(&listmngr_web::CheckEmail {
        shell: Shell::new(language, "web-title-check-email", Nav::Login),
    });
    *response.status_mut() = StatusCode::ACCEPTED;
    Ok(response)
}

/// Origin and CSRF on an anonymous session: the same anchor the subscription
/// request form uses, without requiring a signed-in user.
async fn write_session_anonymous(
    s: &AppState,
    headers: &HeaderMap,
    csrf: &str,
) -> ApiResult<listmngr_db::web_sessions::WebSession> {
    write_session(s, headers, csrf).await
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(super) struct VerifyQuery {
    #[serde(default)]
    token: String,
}

pub(super) async fn verify_form(
    State(s): State<AppState>,
    Query(query): Query<VerifyQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    if query.token.len() > 128 {
        return Err(Error::Validation("token".into()).into());
    }
    let session = anonymous(&s, &headers).await?;
    let mut response = html(&listmngr_web::VerifyForm {
        shell: Shell::new(
            reader_language(&s, &headers, &session).await?,
            "web-title-verify",
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
pub(super) struct VerifyForm {
    #[serde(default)]
    csrf: String,
    token: String,
}

pub(super) async fn verify(
    State(s): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<VerifyForm>,
) -> ApiResult<Response> {
    let session = write_session(&s, &headers, &form.csrf).await?;
    s.pre_auth_rate
        .check("web-verify")
        .map_err(|retry_after| ApiError(Error::RateLimited { retry_after }))?;
    if form.token.len() > 128 {
        return Err(Error::Validation("token".into()).into());
    }
    s.db.browser_verify_address(&session, &form.token, now())
        .await?;
    Ok(html(&listmngr_web::Verified {
        shell: Shell::new(language(&s, &headers), "web-title-verified", Nav::Login),
    }))
}
