//! Authenticated password change; password reset/recovery is a separate workflow.
use super::*;

pub(super) async fn form(State(s): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    let session = load(&s, &headers).await?;
    session.user_id.ok_or(Error::Authentication)?;
    Ok(page(
        "Change password",
        &format!(
            "<form method=\"post\" action=\"/web/account/password\">{}<p><label>Current password <input type=\"password\" name=\"current_password\" autocomplete=\"current-password\" maxlength=\"1024\" required></label></p><p><label>New password <input type=\"password\" name=\"new_password\" autocomplete=\"new-password\" maxlength=\"1024\" required></label></p><p><label>Confirm new password <input type=\"password\" name=\"confirm_password\" autocomplete=\"new-password\" maxlength=\"1024\" required></label></p><p>Changing your password signs you out on all browsers.</p><button>Change password</button></form>",
            hidden(&session)
        ),
    ))
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
    let mut response = page(
        "Password changed",
        "<p>All your browser sessions have been signed out.</p><p><a href=\"/web/login\">Log in with your new password</a></p>",
    );
    response.headers_mut().insert(
        header::SET_COOKIE,
        "listmngr_session=; Path=/web; HttpOnly; SameSite=Strict; Max-Age=0"
            .parse()
            .expect("static cookie"),
    );
    Ok(response)
}
