//! Authenticated password change; password reset/recovery is a separate workflow.
use super::{
    ApiError, ApiResult, AppState, Error, Form, HeaderMap, Response, Shell, State, header, html,
    load, reader_language, write_session,
};
use listmngr_web::Nav;
use serde::Deserialize;

pub(super) async fn form(State(s): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    let session = load(&s, &headers).await?;
    session.user_id.ok_or(Error::Authentication)?;
    Ok(html(&listmngr_web::Password {
        shell: Shell::new(
            reader_language(&s, &headers, &session).await?,
            "web-title-password",
            Nav::Account,
        ),
        csrf: session.csrf,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Change {
    csrf: String,
    current_password: String,
    new_password: String,
    confirm_password: String,
}

pub(super) async fn change(
    State(s): State<AppState>,
    headers: HeaderMap,
    Form(f): Form<Change>,
) -> ApiResult<Response> {
    let session = write_session(&s, &headers, &f.csrf).await?;
    session.user_id.ok_or(Error::Authentication)?;
    if f.new_password != f.confirm_password {
        return Err(Error::Validation("password confirmation does not match".into()).into());
    }
    s.web_login_rate
        .check("web-login")
        .map_err(|retry_after| ApiError(Error::RateLimited { retry_after }))?;
    s.db.browser_change_password(&session, &f.current_password, &f.new_password)
        .await?;
    let mut response = html(&listmngr_web::PasswordChanged {
        shell: Shell::new(
            reader_language(&s, &headers, &session).await?,
            "web-title-password-changed",
            Nav::Account,
        ),
    });
    response.headers_mut().insert(
        header::SET_COOKIE,
        "listmngr_session=; Path=/web; HttpOnly; SameSite=Strict; Max-Age=0"
            .parse()
            .expect("static cookie"),
    );
    Ok(response)
}
