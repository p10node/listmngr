//! Two-step sign-in: the second step of a login, and enrolment, recovery
//! codes and disabling under the account.
use super::{
    ApiError, ApiResult, AppState, Form, HeaderMap, IntoResponse, Redirect, Response, Shell, State,
    html, language, load, now, reader_language, set_cookie, write_session,
};
use listmngr_core::Error;
use listmngr_web::Nav;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CodeForm {
    #[serde(default)]
    csrf: String,
    code: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PasswordForm {
    #[serde(default)]
    csrf: String,
    password: String,
}

/// The second step: reachable only with a session the password left pending.
pub(super) async fn login_form(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let session = load(&s, &headers).await?;
    if !s.db.browser_second_factor_pending(&session).await? {
        return Err(Error::Authentication.into());
    }
    Ok(html(&listmngr_web::LoginTotp {
        shell: Shell::new(language(&s, &headers), "web-title-login-totp", Nav::Login),
        csrf: session.csrf.clone(),
    }))
}

pub(super) async fn login(
    State(s): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<CodeForm>,
) -> ApiResult<Response> {
    let session = write_session(&s, &headers, &form.csrf).await?;
    s.pre_auth_rate
        .check("web-totp")
        .map_err(|retry_after| ApiError(Error::RateLimited { retry_after }))?;
    if form.code.len() > 16 {
        return Err(Error::Authentication.into());
    }
    let fresh =
        s.db.browser_second_factor(&session, &form.code, now())
            .await?;
    let mut response = Redirect::to("/web/account").into_response();
    set_cookie(&s, &fresh, &mut response)?;
    Ok(response)
}

pub(super) async fn status(State(s): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    let session = load(&s, &headers).await?;
    let language = reader_language(&s, &headers, &session).await?;
    let status =
        s.db.browser_totp_status(&session, &s.config.security.require_2fa_for, now())
            .await?;
    let (secret, uri, qr) = status.pending.unwrap_or_default();
    Ok(html(&listmngr_web::TotpPage {
        shell: Shell::new(language, "web-title-totp", Nav::Account),
        csrf: session.csrf.clone(),
        enabled: status.enabled,
        required: status.required,
        secret,
        uri,
        qr,
    }))
}

pub(super) async fn confirm(
    State(s): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<CodeForm>,
) -> ApiResult<Response> {
    let session = write_session(&s, &headers, &form.csrf).await?;
    if form.code.len() > 16 {
        return Err(Error::Validation("code".into()).into());
    }
    let codes =
        s.db.browser_totp_confirm(&session, &form.code, now())
            .await?;
    Ok(html(&listmngr_web::TotpCodes {
        shell: Shell::new(
            reader_language(&s, &headers, &session).await?,
            "web-title-totp",
            Nav::Account,
        ),
        codes,
    }))
}

pub(super) async fn recovery(
    State(s): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<PasswordForm>,
) -> ApiResult<Response> {
    let session = write_session(&s, &headers, &form.csrf).await?;
    s.web_login_rate
        .check("web-login")
        .map_err(|retry_after| ApiError(Error::RateLimited { retry_after }))?;
    let codes =
        s.db.browser_totp_recovery_codes(&session, &form.password, now())
            .await?;
    Ok(html(&listmngr_web::TotpCodes {
        shell: Shell::new(
            reader_language(&s, &headers, &session).await?,
            "web-title-totp",
            Nav::Account,
        ),
        codes,
    }))
}

pub(super) async fn disable(
    State(s): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<PasswordForm>,
) -> ApiResult<Response> {
    let session = write_session(&s, &headers, &form.csrf).await?;
    s.web_login_rate
        .check("web-login")
        .map_err(|retry_after| ApiError(Error::RateLimited { retry_after }))?;
    s.db.browser_totp_disable(&session, &form.password).await?;
    Ok(Redirect::to("/web/account/totp").into_response())
}
