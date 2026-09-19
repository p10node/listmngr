//! Mailman's `/lists/{id}/requests`: undecided subscription and
//! unsubscription requests, and the moderator verbs on them.
use crate::{
    ApiResult, AppState, ErrorResponse, JsonOrForm, PageQuery, RequestPageResponse, audit_context,
    authorize_list, page_response, page_window, parse_list_path, peer,
};
use axum::{
    Json, Router,
    extract::{ConnectInfo, Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing,
};
use listmngr_core::Error;
use listmngr_db::workflows::{
    PendingRequest, RequestDecision, RequestFilter, SubscriptionAction, TokenOwner,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::net::SocketAddr;

/// One entry, in Mailman's shape: the request id is the `token`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct RequestResponse {
    pub email: String,
    /// Empty for a public request; set when an operator supplied one.
    pub display_name: String,
    pub list_id: String,
    pub token: String,
    pub token_owner: String,
    /// `subscription` or `unsubscription`.
    #[serde(rename = "type")]
    pub kind: String,
    pub request_date: String,
    pub self_link: String,
    pub http_etag: String,
}

#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
pub struct RequestQuery {
    /// `subscriber` or `moderator`.
    token_owner: Option<String>,
    /// `subscription` or `unsubscription`.
    request_type: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DecisionInput {
    /// `accept`, `reject`, `discard` or `defer`.
    action: String,
    #[serde(default)]
    reason: Option<String>,
}

/// The list-scoped request routes, mounted under both API prefixes.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/lists/{id}/requests", routing::get(list))
        .route("/lists/{id}/requests/count", routing::get(count))
        .route(
            "/lists/{id}/requests/{token}",
            routing::get(get).post(decide),
        )
}

const fn kind(action: SubscriptionAction) -> &'static str {
    match action {
        SubscriptionAction::Join => "subscription",
        SubscriptionAction::Leave => "unsubscription",
    }
}

fn value(state: &AppState, request: &PendingRequest) -> Value {
    let prefix = match state.flavor {
        crate::ApiFlavor::V1 => "/api/v1",
        crate::ApiFlavor::Compat31 => "/3.1",
    };
    let token_owner = match request.token_owner {
        TokenOwner::Subscriber => "subscriber",
        TokenOwner::Moderator => "moderator",
    };
    let request_date = chrono::DateTime::from_timestamp_millis(request.requested_at)
        .map(|value| value.to_rfc3339())
        .unwrap_or_default();
    let etag = format!(
        "{:x}",
        Sha256::digest(format!(
            "{}|{}|{token_owner}",
            request.id, request.requested_at
        ))
    );
    let mut value = json!(RequestResponse {
        email: request.email.clone(),
        display_name: request.display_name.clone(),
        list_id: request.list_id.to_string(),
        token: request.id.clone(),
        token_owner: token_owner.into(),
        kind: kind(request.action).into(),
        request_date: request_date.clone(),
        self_link: format!("{prefix}/lists/{}/requests/{}", request.list_id, request.id),
        http_etag: etag,
    });
    // Mailman names the request's time `when`; mailmanclient reads it.
    if matches!(state.flavor, crate::ApiFlavor::Compat31)
        && let Some(object) = value.as_object_mut()
    {
        object.insert("when".into(), json!(request_date));
    }
    value
}

fn filter_of(query: &RequestQuery) -> ApiResult<RequestFilter> {
    let token_owner = match query.token_owner.as_deref() {
        None => None,
        Some("subscriber") => Some(TokenOwner::Subscriber),
        Some("moderator") => Some(TokenOwner::Moderator),
        Some(_) => return Err(Error::Validation("token_owner".into()).into()),
    };
    let action = match query.request_type.as_deref() {
        None => None,
        Some("subscription") => Some(SubscriptionAction::Join),
        Some("unsubscription") => Some(SubscriptionAction::Leave),
        Some(_) => return Err(Error::Validation("request_type".into()).into()),
    };
    Ok(RequestFilter {
        token_owner,
        action,
    })
}

#[utoipa::path(get, path = "/api/v1/lists/{id}/requests", params(("id" = String, Path), PageQuery, RequestQuery),
    responses((status = 200, description = "Undecided subscription requests, oldest first", body = RequestPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn list(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(page_query): Query<PageQuery>,
    Query(query): Query<RequestQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = parse_list_path(s.flavor, &id)?;
    authorize_list(&s, &h, peer(c), "moderation", &id).await?;
    s.db.lists().get(&id).await?;
    let requests = s.db.workflows().pending(&id, filter_of(&query)?).await?;
    let (start, end) = page_window(requests.len(), &page_query)?;
    let entries: Vec<Value> = requests[start..end]
        .iter()
        .map(|request| value(&s, request))
        .collect();
    Ok(Json(page_response(
        s.flavor,
        &entries,
        start,
        requests.len(),
    )))
}

#[utoipa::path(get, path = "/api/v1/lists/{id}/requests/count", params(("id" = String, Path), RequestQuery),
    responses((status = 200, description = "Number of undecided requests", body = crate::CountResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn count(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<RequestQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = parse_list_path(s.flavor, &id)?;
    authorize_list(&s, &h, peer(c), "moderation", &id).await?;
    s.db.lists().get(&id).await?;
    let requests = s.db.workflows().pending(&id, filter_of(&query)?).await?;
    Ok(Json(json!({ "count": requests.len() })))
}

/// The request named by the URL, only when it belongs to the list in it.
async fn owned(
    s: &AppState,
    list: &listmngr_core::ListId,
    token: &str,
) -> ApiResult<PendingRequest> {
    let request = s.db.workflows().get(token).await?;
    if request.list_id != *list {
        return Err(Error::NotFound("subscription request".into()).into());
    }
    Ok(request)
}

#[utoipa::path(get, path = "/api/v1/lists/{id}/requests/{token}", params(("id" = String, Path), ("token" = String, Path)),
    responses((status = 200, description = "One undecided request", body = RequestResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn get(
    State(s): State<AppState>,
    Path((id, token)): Path<(String, String)>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = parse_list_path(s.flavor, &id)?;
    authorize_list(&s, &h, peer(c), "moderation", &id).await?;
    Ok(Json(value(&s, &owned(&s, &id, &token).await?)))
}

#[utoipa::path(post, path = "/api/v1/lists/{id}/requests/{token}", params(("id" = String, Path), ("token" = String, Path)),
    request_body(content((DecisionInput = "application/json"), (DecisionInput = "application/x-www-form-urlencoded"))),
    responses((status = 204, description = "Decision applied"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn decide(
    State(s): State<AppState>,
    Path((id, token)): Path<(String, String)>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(input): JsonOrForm<DecisionInput>,
) -> ApiResult<StatusCode> {
    let id = parse_list_path(s.flavor, &id)?;
    let addr = peer(c);
    let auth = authorize_list(&s, &h, addr, "moderation", &id).await?;
    let decision = match input.action.as_str() {
        "accept" => RequestDecision::Accept,
        "reject" => RequestDecision::Reject,
        "discard" => RequestDecision::Discard,
        "defer" => RequestDecision::Defer,
        other => {
            return Err(Error::Validation(format!(
                "request action {other:?} is not one of accept, reject, discard, defer"
            ))
            .into());
        }
    };
    owned(&s, &id, &token).await?;
    s.db.workflows()
        .decide(
            &token,
            decision,
            input.reason.as_deref().unwrap_or(""),
            &audit_context(&auth, addr),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
