//! Passkeys: the page under the account, the JSON ceremonies its script and
//! the login page's script drive, and removal. The ceremonies are the only
//! JSON the browser surface speaks; they carry the same Origin and CSRF
//! checks as every form, with the token in a header.
use super::{
    ApiError, ApiResult, AppState, Form, HeaderMap, IntoResponse, Path, Redirect, Response, Shell,
    State, anonymous, check_origin, html, load, now, reader_language, set_cookie,
};
use axum::{Json, http::header};
use listmngr_core::Error;
use listmngr_web::Nav;
use serde::Deserialize;

fn moment(shell: &Shell, value: Option<i64>, absent: &str) -> String {
    value.map_or_else(
        || shell.t(absent),
        |at| {
            chrono::DateTime::from_timestamp_millis(at)
                .unwrap_or_default()
                .format("%Y-%m-%d %H:%M UTC")
                .to_string()
        },
    )
}

/// The CSRF token a script sends as a header; forms send it as a field.
fn header_csrf(headers: &HeaderMap) -> &str {
    headers
        .get("x-csrf-token")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
}

/// A JSON reply, or an error the shell's failure page will render.
fn json<T: serde::Serialize>(value: T) -> Response {
    Json(value).into_response()
}

pub(super) async fn index(State(s): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    let session = load(&s, &headers).await?;
    let shell = Shell::new(
        reader_language(&s, &headers, &session).await?,
        "web-title-passkeys",
        Nav::Account,
    )
    .with_scripts();
    let passkeys =
        s.db.browser_passkeys(&session)
            .await?
            .into_iter()
            .map(|item| listmngr_web::PasskeyRow {
                created: moment(&shell, Some(item.created_at), "web-passkeys-never"),
                last_used: moment(&shell, item.last_used_at, "web-passkeys-never"),
                id: item.id,
                name: item.name,
            })
            .collect();
    Ok(super::scripted(html(&listmngr_web::Passkeys {
        csrf: session.csrf.clone(),
        passkeys,
        shell,
    })))
}

pub(super) async fn register_start(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    check_origin(&s, &headers)?;
    let session = load(&s, &headers).await?;
    if !session.verifies_csrf(header_csrf(&headers)) {
        return Err(super::denied());
    }
    let options = s.db.browser_passkey_register_start(&session).await?;
    Ok(json_text(options))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RegisterFinish {
    name: String,
    credential: serde_json::Value,
}

pub(super) async fn register_finish(
    State(s): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> ApiResult<Response> {
    check_origin(&s, &headers)?;
    let session = load(&s, &headers).await?;
    if !session.verifies_csrf(header_csrf(&headers)) {
        return Err(super::denied());
    }
    let form: RegisterFinish =
        serde_json::from_str(&body).map_err(|_| Error::Validation("passkey response".into()))?;
    s.db.browser_passkey_register_finish(&session, &form.name, &form.credential.to_string(), now())
        .await?;
    Ok(json(serde_json::json!({"ok": true})))
}

pub(super) async fn login_start(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    check_origin(&s, &headers)?;
    let session = anonymous(&s, &headers).await?;
    if !session.verifies_csrf(header_csrf(&headers)) {
        return Err(super::denied());
    }
    s.pre_auth_rate
        .check("web-passkey")
        .map_err(|retry_after| ApiError(Error::RateLimited { retry_after }))?;
    let options = s.db.browser_passkey_login_start(&session, now()).await?;
    let mut response = json_text(options);
    set_cookie(&s, &session, &mut response)?;
    Ok(response)
}

pub(super) async fn login_finish(
    State(s): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> ApiResult<Response> {
    check_origin(&s, &headers)?;
    let session = load(&s, &headers).await?;
    if !session.verifies_csrf(header_csrf(&headers)) {
        return Err(super::denied());
    }
    s.pre_auth_rate
        .check("web-passkey")
        .map_err(|retry_after| ApiError(Error::RateLimited { retry_after }))?;
    let fresh =
        s.db.browser_passkey_login_finish(&session, &body, now())
            .await?;
    let mut response = json(serde_json::json!({"next": "/web/account"}));
    set_cookie(&s, &fresh, &mut response)?;
    Ok(response)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RemoveForm {
    #[serde(default)]
    csrf: String,
    password: String,
}

pub(super) async fn remove(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(form): Form<RemoveForm>,
) -> ApiResult<Response> {
    if id.len() > 64 || !id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return Err(Error::NotFound("passkey".into()).into());
    }
    let session = super::write_session(&s, &headers, &form.csrf).await?;
    s.web_login_rate
        .check("web-login")
        .map_err(|retry_after| ApiError(Error::RateLimited { retry_after }))?;
    s.db.browser_passkey_remove(&session, &id, &form.password)
        .await?;
    Ok(Redirect::to("/web/account/passkeys").into_response())
}

/// Already-serialized JSON from the ceremony library.
fn json_text(text: String) -> Response {
    (
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        text,
    )
        .into_response()
}
