//! Self-service account deletion, confirmed with the password.
use super::{
    ApiResult, AppState, Form, HeaderMap, Response, Shell, State, clear_cookie, html, language,
    load, reader_language, write_session,
};
use listmngr_web::Nav;
use serde::Deserialize;

pub(super) async fn form(State(s): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    let session = load(&s, &headers).await?;
    session
        .user_id
        .ok_or(listmngr_core::Error::Authentication)?;
    Ok(html(&listmngr_web::DeleteAccount {
        shell: Shell::new(
            reader_language(&s, &headers, &session).await?,
            "web-title-delete",
            Nav::Account,
        ),
        csrf: session.csrf.clone(),
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeleteForm {
    #[serde(default)]
    csrf: String,
    password: String,
}

pub(super) async fn delete(
    State(s): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<DeleteForm>,
) -> ApiResult<Response> {
    let session = write_session(&s, &headers, &form.csrf).await?;
    // The account is gone after this, so the page language comes from the
    // browser alone.
    let language = language(&s, &headers);
    s.web_login_rate.check("web-login").map_err(|retry_after| {
        super::ApiError(listmngr_core::Error::RateLimited { retry_after })
    })?;
    s.db.browser_delete_account(&session, &form.password)
        .await?;
    let mut response = html(&listmngr_web::AccountDeleted {
        shell: Shell::new(language, "web-title-deleted", Nav::None),
    });
    clear_cookie(&s, &mut response)?;
    Ok(response)
}
