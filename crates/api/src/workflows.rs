//! Public confirm-only workflow routes.
//!
//! Unknown lists, membership and throttling
//! have the same request response. No token is returned to the request initiator.
use crate::AppState;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::StatusCode,
    routing::post,
};
use listmngr_core::{Error, ListId};
use listmngr_db::workflows::SubscriptionAction;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    email: String,
    action: SubscriptionAction,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Confirmation {
    token: String,
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/public/lists/{list_id}/subscription", post(request))
        .route("/api/v1/public/lists/{list_id}/confirm", post(confirm))
        .layer(DefaultBodyLimit::max(2048))
}
async fn request(
    State(state): State<AppState>,
    Path(list): Path<ListId>,
    Json(input): Json<Request>,
) -> Result<(StatusCode, Json<serde_json::Value>), StatusCode> {
    // A single global bucket bounds unauthenticated work and limiter cardinality.
    // Durable DB notice budgets additionally apply across all server processes.
    state
        .pre_auth_rate
        .check("public-workflows")
        .map_err(|_| StatusCode::TOO_MANY_REQUESTS)?;
    state
        .db
        .workflows()
        .request(
            &list,
            &input.email,
            input.action,
            chrono::Utc::now().timestamp_millis(),
        )
        .await
        .map_err(|error| status(&error))?;
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({"message":"If eligible, confirmation instructions will be sent."})),
    ))
}
async fn confirm(
    State(state): State<AppState>,
    Path(list): Path<ListId>,
    Json(input): Json<Confirmation>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    state
        .pre_auth_rate
        .check("public-workflows")
        .map_err(|_| StatusCode::TOO_MANY_REQUESTS)?;
    state
        .db
        .workflows()
        .confirm(&list, &input.token, chrono::Utc::now().timestamp_millis())
        .await
        .map_err(|error| status(&error))?;
    Ok(Json(
        serde_json::json!({"message":"Confirmation completed."}),
    ))
}
const fn status(error: &Error) -> StatusCode {
    match error {
        Error::Validation(_) => StatusCode::BAD_REQUEST,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}
