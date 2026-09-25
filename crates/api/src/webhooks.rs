//! Webhooks over REST (`/webhooks`, under both prefixes): create one and
//! be shown its secret once, read and change it, rotate its secret, ping
//! it, delete it, and read what it was owed. Every route needs the
//! `webhooks` scope (`admin` implies it); a token bound to a list sees
//! and makes only that list's webhooks, and a token bound to a domain
//! none.
use crate::{
    ApiError, ApiResult, AppState, DeliveryPageResponse, EmptyMutationInput, ErrorResponse,
    PageQuery, WebhookPageResponse, audit_context, authenticate_for_authorization,
    finish_authorization, page_response, page_window, peer,
};
use axum::{
    Json, Router,
    extract::{ConnectInfo, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing,
};
use listmngr_core::{Error, ListId, WebhookId};
use listmngr_db::{Delivery, NewWebhook, TokenAuth, Webhook, WebhookPatch};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::net::SocketAddr;

/// The scope every webhook route needs.
pub const SCOPE: &str = "webhooks";

/// The webhook routes, mounted under both API prefixes.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/webhooks", routing::get(list).post(create))
        .route(
            "/webhooks/{id}",
            routing::get(get).patch(patch).delete(delete),
        )
        .route("/webhooks/{id}/rotate", routing::post(rotate))
        .route("/webhooks/{id}/ping", routing::post(ping))
        .route("/webhooks/{id}/deliveries", routing::get(deliveries))
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WebhookInput {
    /// The `https://` URL the site posts to.
    pub url: String,
    /// Audit actions to post: `*`, a prefix such as `member.*`, or an
    /// action such as `list.config`.
    pub events: Vec<String>,
    /// Only this list's events; a list-bound token's list when omitted.
    #[serde(default)]
    pub list_id: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WebhookPatchInput {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub events: Option<Vec<String>>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct WebhookResponse {
    pub id: String,
    pub url: String,
    pub description: String,
    pub events: Vec<String>,
    pub list_id: Option<String>,
    pub enabled: bool,
    /// The first eight hex digits of the secret's SHA-256.
    pub secret_fingerprint: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub self_link: String,
    /// On creation and rotation only: the secret, shown this once and
    /// never stored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct DeliveryResponse {
    pub id: String,
    pub webhook_id: String,
    pub event: String,
    pub list_id: Option<String>,
    /// `pending`, `delivered` or `failed`.
    pub state: String,
    pub attempts: i64,
    pub next_attempt_at: i64,
    pub last_status: Option<i64>,
    pub last_error: Option<String>,
    pub created_at: i64,
    pub finished_at: Option<i64>,
    /// What was, or will be, posted.
    #[schema(value_type = Object)]
    pub payload: Value,
}

const fn prefix(state: &AppState) -> &'static str {
    match state.flavor {
        crate::ApiFlavor::V1 => "/api/v1",
        crate::ApiFlavor::Compat31 => "/3.1",
    }
}

fn value(state: &AppState, webhook: &Webhook, secret: Option<String>) -> WebhookResponse {
    WebhookResponse {
        id: webhook.id.to_string(),
        url: webhook.url.clone(),
        description: webhook.description.clone(),
        events: webhook.events.clone(),
        list_id: webhook.list_id.as_ref().map(ToString::to_string),
        enabled: webhook.enabled,
        secret_fingerprint: webhook.secret_fingerprint.clone(),
        created_at: webhook.created_at,
        updated_at: webhook.updated_at,
        self_link: format!("{}/webhooks/{}", prefix(state), webhook.id),
        secret,
    }
}

fn delivery_value(delivery: &Delivery) -> DeliveryResponse {
    DeliveryResponse {
        id: delivery.id.clone(),
        webhook_id: delivery.webhook_id.to_string(),
        event: delivery.event.clone(),
        list_id: delivery.list_id.clone(),
        state: serde_json::to_value(delivery.state)
            .ok()
            .and_then(|state| state.as_str().map(ToOwned::to_owned))
            .unwrap_or_default(),
        attempts: delivery.attempts,
        next_attempt_at: delivery.next_attempt_at,
        last_status: delivery.last_status,
        last_error: delivery.last_error.clone(),
        created_at: delivery.created_at,
        finished_at: delivery.finished_at,
        payload: delivery.payload.clone(),
    }
}

/// The scope, and the list a bound token is confined to. A token bound
/// to a domain has no webhooks to see.
async fn authorize(
    state: &AppState,
    headers: &HeaderMap,
    addr: SocketAddr,
) -> ApiResult<(TokenAuth, Option<ListId>)> {
    let auth = authenticate_for_authorization(state, headers, addr, SCOPE).await?;
    if auth.domain_id.is_some() {
        return Err(ApiError(Error::Forbidden(SCOPE.into())));
    }
    let bound = auth.list_id.clone();
    Ok((finish_authorization(state, auth).await?, bound))
}

fn parse_id(id: &str) -> ApiResult<WebhookId> {
    id.parse()
        .map_err(|_| ApiError(Error::Validation("webhook id".into())))
}

/// A webhook a bound token may not see is, to it, not there.
async fn fetch(state: &AppState, id: WebhookId, bound: Option<&ListId>) -> ApiResult<Webhook> {
    let webhook = state.db.webhooks().get(id).await?;
    if let Some(list) = bound
        && webhook.list_id.as_ref() != Some(list)
    {
        return Err(ApiError(Error::NotFound(format!("webhook {id}"))));
    }
    Ok(webhook)
}

#[utoipa::path(get, path = "/api/v1/webhooks", params(PageQuery),
    responses((status = 200, description = "The site's webhooks, or the token's list's", body = WebhookPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope or domain-bound token", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn list(
    State(s): State<AppState>,
    Query(q): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let (_, bound) = authorize(&s, &h, peer(c)).await?;
    let rows = s.db.webhooks().list(bound.as_ref()).await?;
    let (start, end) = page_window(rows.len(), &q)?;
    let entries: Vec<Value> = rows[start..end]
        .iter()
        .map(|webhook| serde_json::json!(value(&s, webhook, None)))
        .collect();
    Ok(Json(page_response(s.flavor, &entries, start, rows.len())))
}

#[utoipa::path(post, path = "/api/v1/webhooks", request_body = WebhookInput,
    responses((status = 201, description = "Created, with the secret shown this once", body = WebhookResponse), (status = 400, description = "Invalid URL, events or description, or no signing key configured", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope, or a list-bound token naming another list", body = ErrorResponse), (status = 404, description = "List missing", body = ErrorResponse), (status = 409, description = "Conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn create(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    Json(input): Json<WebhookInput>,
) -> ApiResult<Response> {
    let addr = peer(c);
    let (auth, bound) = authorize(&s, &h, addr).await?;
    let list_id: Option<ListId> = input.list_id.as_deref().map(str::parse).transpose()?;
    let list_id = match (bound, list_id) {
        (Some(bound), Some(list)) if list != bound => {
            return Err(ApiError(Error::Forbidden(SCOPE.into())));
        }
        (Some(bound), _) => Some(bound),
        (None, list) => list,
    };
    let (webhook, secret) =
        s.db.webhooks()
            .create_with_context(
                NewWebhook {
                    url: input.url,
                    events: input.events,
                    list_id,
                    description: input.description.unwrap_or_default(),
                },
                &audit_context(&auth, addr),
            )
            .await?;
    let body = value(&s, &webhook, Some(secret));
    Ok((
        StatusCode::CREATED,
        [(header::LOCATION, body.self_link.clone())],
        Json(body),
    )
        .into_response())
}

#[utoipa::path(get, path = "/api/v1/webhooks/{id}", params(("id" = String, Path)),
    responses((status = 200, description = "The webhook, never its secret", body = WebhookResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Webhook missing, or another list's", body = ErrorResponse), (status = 409, description = "Conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn get(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<WebhookResponse>> {
    let (_, bound) = authorize(&s, &h, peer(c)).await?;
    let webhook = fetch(&s, parse_id(&id)?, bound.as_ref()).await?;
    Ok(Json(value(&s, &webhook, None)))
}

#[utoipa::path(patch, path = "/api/v1/webhooks/{id}", params(("id" = String, Path)), request_body = WebhookPatchInput,
    responses((status = 200, description = "The webhook as changed", body = WebhookResponse), (status = 400, description = "Invalid URL, events or description", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Webhook missing, or another list's", body = ErrorResponse), (status = 409, description = "Conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn patch(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    Json(input): Json<WebhookPatchInput>,
) -> ApiResult<Json<WebhookResponse>> {
    let addr = peer(c);
    let (auth, bound) = authorize(&s, &h, addr).await?;
    let id = parse_id(&id)?;
    fetch(&s, id, bound.as_ref()).await?;
    let webhook =
        s.db.webhooks()
            .update_with_context(
                id,
                WebhookPatch {
                    url: input.url,
                    events: input.events,
                    description: input.description,
                    enabled: input.enabled,
                },
                &audit_context(&auth, addr),
            )
            .await?;
    Ok(Json(value(&s, &webhook, None)))
}

#[utoipa::path(delete, path = "/api/v1/webhooks/{id}", params(("id" = String, Path)),
    responses((status = 204, description = "Deleted with every delivery it was owed"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Webhook missing, or another list's", body = ErrorResponse), (status = 409, description = "Conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn delete(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<StatusCode> {
    let addr = peer(c);
    let (auth, bound) = authorize(&s, &h, addr).await?;
    let id = parse_id(&id)?;
    fetch(&s, id, bound.as_ref()).await?;
    s.db.webhooks()
        .delete_with_context(id, &audit_context(&auth, addr))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/api/v1/webhooks/{id}/rotate", params(("id" = String, Path)),
    request_body(content = EmptyMutationInput, content_type = "application/json"),
    responses((status = 200, description = "The webhook with its new secret, shown this once", body = WebhookResponse), (status = 400, description = "Invalid request, or no signing key configured", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Webhook missing, or another list's", body = ErrorResponse), (status = 409, description = "Conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn rotate(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    _body: Option<Json<EmptyMutationInput>>,
) -> ApiResult<Json<WebhookResponse>> {
    let addr = peer(c);
    let (auth, bound) = authorize(&s, &h, addr).await?;
    let id = parse_id(&id)?;
    fetch(&s, id, bound.as_ref()).await?;
    let secret =
        s.db.webhooks()
            .rotate_with_context(id, &audit_context(&auth, addr))
            .await?;
    let webhook = s.db.webhooks().get(id).await?;
    Ok(Json(value(&s, &webhook, Some(secret))))
}

#[utoipa::path(post, path = "/api/v1/webhooks/{id}/ping", params(("id" = String, Path)),
    request_body(content = EmptyMutationInput, content_type = "application/json"),
    responses((status = 201, description = "A ping delivery queued for the runner", body = DeliveryResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Webhook missing, or another list's", body = ErrorResponse), (status = 409, description = "Conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn ping(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    _body: Option<Json<EmptyMutationInput>>,
) -> ApiResult<Response> {
    let addr = peer(c);
    let (auth, bound) = authorize(&s, &h, addr).await?;
    let id = parse_id(&id)?;
    fetch(&s, id, bound.as_ref()).await?;
    let delivery =
        s.db.webhooks()
            .ping_with_context(id, &audit_context(&auth, addr))
            .await?;
    Ok((StatusCode::CREATED, Json(delivery_value(&delivery))).into_response())
}

#[utoipa::path(get, path = "/api/v1/webhooks/{id}/deliveries", params(("id" = String, Path), PageQuery),
    responses((status = 200, description = "The webhook's deliveries, newest first, the last thousand at most", body = DeliveryPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Webhook missing, or another list's", body = ErrorResponse), (status = 409, description = "Conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn deliveries(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let (_, bound) = authorize(&s, &h, peer(c)).await?;
    let id = parse_id(&id)?;
    fetch(&s, id, bound.as_ref()).await?;
    let rows = s.db.webhooks().deliveries(id, 1000).await?;
    let (start, end) = page_window(rows.len(), &q)?;
    let entries: Vec<Value> = rows[start..end]
        .iter()
        .map(|delivery| serde_json::json!(delivery_value(delivery)))
        .collect();
    Ok(Json(page_response(s.flavor, &entries, start, rows.len())))
}
