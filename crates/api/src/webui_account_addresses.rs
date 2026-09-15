//! The reader's own addresses: add and prove, choose the primary, let go.
use super::{
    ApiResult, AppState, Csrf, Form, HeaderMap, IntoResponse, Path, Redirect, Response, Shell,
    State, html, load, now, reader_language, write_session,
};
use listmngr_core::Error;
use listmngr_web::Nav;
use serde::Deserialize;

pub(super) async fn index(State(s): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    let session = load(&s, &headers).await?;
    let language = reader_language(&s, &headers, &session).await?;
    let addresses =
        s.db.browser_addresses(&session)
            .await?
            .into_iter()
            .map(|item| listmngr_web::AddressRow {
                id: item.id,
                email: item.email,
                verified: item.verified,
                primary: item.primary,
            })
            .collect();
    Ok(html(&listmngr_web::Addresses {
        shell: Shell::new(language, "web-title-addresses", Nav::Account),
        csrf: session.csrf.clone(),
        addresses,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AddForm {
    #[serde(default)]
    csrf: String,
    email: String,
}

pub(super) async fn add(
    State(s): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<AddForm>,
) -> ApiResult<Response> {
    let session = write_session(&s, &headers, &form.csrf).await?;
    if form.email.len() > 320 {
        return Err(Error::Validation("field too long".into()).into());
    }
    let language = reader_language(&s, &headers, &session).await?;
    s.db.browser_add_address(&session, &form.email, language, now())
        .await?;
    Ok(Redirect::to("/web/account/addresses").into_response())
}

fn checked_id(id: &str) -> ApiResult<()> {
    if id.len() > 64 || !id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return Err(Error::NotFound("address".into()).into());
    }
    Ok(())
}

pub(super) async fn primary(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(form): Form<Csrf>,
) -> ApiResult<Response> {
    checked_id(&id)?;
    let session = write_session(&s, &headers, &form.csrf).await?;
    s.db.browser_primary_address(&session, &id).await?;
    Ok(Redirect::to("/web/account/addresses").into_response())
}

pub(super) async fn remove(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(form): Form<Csrf>,
) -> ApiResult<Response> {
    checked_id(&id)?;
    let session = write_session(&s, &headers, &form.csrf).await?;
    s.db.browser_remove_address(&session, &id).await?;
    Ok(Redirect::to("/web/account/addresses").into_response())
}
