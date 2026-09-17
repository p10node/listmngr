//! Data portability and erasure: the reader's own export, and a server
//! owner's export or erasure of any account.
use super::{
    ApiResult, AppState, Form, HeaderMap, Path, Redirect, Response, State, load, privileged,
    write_session,
};
use axum::{http::header, response::IntoResponse};
use listmngr_core::{Error, UserId};
use serde::Deserialize;

/// The export as a JSON download.
fn download(value: &serde_json::Value, name: &str) -> Response {
    let body = serde_json::to_vec_pretty(value).unwrap_or_default();
    (
        [
            (header::CONTENT_TYPE, "application/json; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                &format!("attachment; filename=\"{name}\""),
            ),
        ],
        body,
    )
        .into_response()
}

/// `GET /web/account/export.json`.
pub(super) async fn own_export(State(s): State<AppState>, h: HeaderMap) -> ApiResult<Response> {
    let session = load(&s, &h).await?;
    let value = s.db.browser_export_own(&session).await?;
    Ok(download(&value, "listmngr-account.json"))
}

/// `GET /web/admin/users/{id}/export.json`.
pub(super) async fn admin_export(
    State(s): State<AppState>,
    Path(id): Path<UserId>,
    h: HeaderMap,
) -> ApiResult<Response> {
    let session = load(&s, &h).await?;
    privileged(&s, &session).await?;
    let value = s.db.browser_export_user(&session, id).await?;
    Ok(download(&value, &format!("listmngr-account-{id}.json")))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EraseForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    confirm: String,
}

/// `POST /web/admin/users/{id}/erase`: the account's address typed back,
/// then everything of theirs goes in one transaction.
pub(super) async fn erase(
    State(s): State<AppState>,
    Path(id): Path<UserId>,
    h: HeaderMap,
    Form(form): Form<EraseForm>,
) -> ApiResult<Response> {
    let session = write_session(&s, &h, &form.csrf).await?;
    privileged(&s, &session).await?;
    let detail = s.db.browser_user(&session, id).await?;
    let typed = form.confirm.trim().to_ascii_lowercase();
    let matches = !typed.is_empty()
        && detail
            .addresses
            .iter()
            .any(|address| address.email == typed);
    if !matches {
        return Ok(Redirect::to(&format!("/web/admin/users/{id}?erase=mismatch")).into_response());
    }
    match s.db.browser_erase_user(&session, id).await {
        Ok(_) => Ok(Redirect::to("/web/admin/users?saved=erased").into_response()),
        Err(Error::Validation(_)) => {
            Ok(Redirect::to(&format!("/web/admin/users/{id}?erase=last-owner")).into_response())
        }
        Err(error) => Err(error.into()),
    }
}
