//! List-scoped ban resources for posting/public join; global administration is not exposed.
use crate::{
    ApiResult, AppState, BanPageResponse, ErrorResponse, JsonOrForm, PageQuery, audit_context,
    authorize_list, page_response, page_window, parse_list_path, peer,
};
use axum::{
    Json, Router,
    extract::{ConnectInfo, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing,
};
use listmngr_core::Error;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::net::SocketAddr;

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BanInput {
    /// Exact mailbox or a case-sensitive Rust regex beginning with ^.
    email: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct BanResponse {
    pub email: String,
    pub self_link: String,
}

/// The list-scoped ban routes, mounted under both API prefixes.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/lists/{id}/bans", routing::get(list).post(create))
        .route("/lists/{id}/bans/{email}", routing::get(get).delete(delete))
}

fn value(state: &AppState, id: &listmngr_core::ListId, email: &str) -> Value {
    let query = serde_urlencoded::to_string([("email", email)]).expect("string query");
    let encoded = query
        .strip_prefix("email=")
        .expect("email key")
        .replace('+', "%20");
    let prefix = match state.flavor {
        crate::ApiFlavor::V1 => "/api/v1",
        crate::ApiFlavor::Compat31 => "/3.1",
    };
    json!(BanResponse {
        email: email.into(),
        self_link: format!("{prefix}/lists/{id}/bans/{encoded}")
    })
}

#[utoipa::path(get, path = "/api/v1/lists/{id}/bans/{email}", params(("id" = String, Path), ("email" = String, Path, description = "Percent-encoded stored mailbox or regex")),
    responses((status = 200, description = "Stored list-local ban; not effective matching status", body = BanResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "List or ban missing", body = ErrorResponse), (status = 409, description = "Conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn get(
    State(s): State<AppState>,
    Path((id, email)): Path<(String, String)>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = parse_list_path(s.flavor, &id)?;
    authorize_list(&s, &h, peer(c), "lists:read", &id).await?;
    let email = s.db.bans().get(&id, &email).await?;
    Ok(Json(value(&s, &id, &email)))
}

#[utoipa::path(get, path = "/api/v1/lists/{id}/bans", params(("id" = String, Path), PageQuery),
    responses((status = 200, description = "Bounded list ban collection", body = BanPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "List missing", body = ErrorResponse), (status = 409, description = "Conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn list(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = parse_list_path(s.flavor, &id)?;
    authorize_list(&s, &h, peer(c), "lists:read", &id).await?;
    let total = usize::try_from(s.db.bans().count(&id).await?)
        .map_err(|_| Error::Validation("ban count exceeds platform range".into()))?;
    let (start, end) = page_window(total, &q)?;
    let offset = i64::try_from(start).map_err(|_| Error::Validation("offset too large".into()))?;
    let rows = if end == start {
        Vec::new()
    } else {
        s.db.bans()
            .list(
                &id,
                i64::try_from(end - start).expect("bounded page"),
                offset,
            )
            .await?
    };
    let entries: Vec<Value> = rows.iter().map(|email| value(&s, &id, email)).collect();
    Ok(Json(page_response(s.flavor, &entries, start, total)))
}

#[utoipa::path(post, path = "/api/v1/lists/{id}/bans", params(("id" = String, Path)),
    request_body(content((BanInput = "application/json"), (BanInput = "application/x-www-form-urlencoded"))),
    responses((status = 201, description = "Ban created", body = BanResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "List missing", body = ErrorResponse), (status = 409, description = "Ban exists", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn create(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(input): JsonOrForm<BanInput>,
) -> ApiResult<Response> {
    let id = parse_list_path(s.flavor, &id)?;
    let addr = peer(c);
    let auth = authorize_list(&s, &h, addr, "lists:write", &id).await?;
    let email =
        s.db.bans()
            .create(&id, &input.email, &audit_context(&auth, addr))
            .await?;
    let body = value(&s, &id, &email);
    let location = body["self_link"].as_str().expect("link string").to_owned();
    Ok((
        StatusCode::CREATED,
        [(header::LOCATION, location)],
        Json(body),
    )
        .into_response())
}

#[utoipa::path(delete, path = "/api/v1/lists/{id}/bans/{email}", params(("id" = String, Path), ("email" = String, Path, description = "Percent-encoded mailbox or regex")),
    responses((status = 204, description = "Ban removed"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "List or ban missing", body = ErrorResponse), (status = 409, description = "Conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn delete(
    State(s): State<AppState>,
    Path((id, email)): Path<(String, String)>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<StatusCode> {
    let id = parse_list_path(s.flavor, &id)?;
    let addr = peer(c);
    let auth = authorize_list(&s, &h, addr, "lists:write", &id).await?;
    s.db.bans()
        .delete(&id, &email, &audit_context(&auth, addr))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
