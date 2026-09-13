//! Mailman's `/lists/{id}/digest`: the list's digest counters, and the
//! `send`/`bump`/`periodic` verbs behind `mailman digests`.
use crate::{
    ApiResult, AppState, ErrorResponse, JsonOrForm, audit_context, authorize_list, mailman_bool,
    parse_list_path, peer,
};
use axum::{
    Json, Router,
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::net::SocketAddr;

/// The counters Mailman shows for a list's digest.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct DigestResponse {
    pub volume: i32,
    pub next_digest_number: i64,
    pub self_link: String,
}

/// Mailman's verbs; every field is optional and defaults to false. Clients
/// spell booleans as `True`/`False` on forms.
#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DigestActionInput {
    /// Publish whatever is collected as one issue now.
    #[serde(default, deserialize_with = "mailman_bool")]
    send: bool,
    /// Advance the volume and restart the issue numbering, before any send.
    #[serde(default, deserialize_with = "mailman_bool")]
    bump: bool,
    /// Publish only if the list's size or daily trigger is due.
    #[serde(default, deserialize_with = "mailman_bool")]
    periodic: bool,
}

/// What a `POST` did.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct DigestActionResponse {
    /// Issues published by this request.
    pub published: usize,
    pub bumped: bool,
}

/// The list-scoped digest routes, mounted under both API prefixes.
pub fn routes() -> Router<AppState> {
    Router::new().route("/lists/{id}/digest", routing::get(get).post(post))
}

fn value(state: &AppState, list: &listmngr_core::MailingList) -> Value {
    let prefix = match state.flavor {
        crate::ApiFlavor::V1 => "/api/v1",
        crate::ApiFlavor::Compat31 => "/3.1",
    };
    let mut value = json!(DigestResponse {
        volume: list.volume,
        next_digest_number: list.next_digest_number,
        self_link: format!("{prefix}/lists/{}/digest", list.id),
    });
    if matches!(state.flavor, crate::ApiFlavor::Compat31) {
        value
            .as_object_mut()
            .expect("digest object")
            .insert("http_etag".into(), json!("phase1"));
    }
    value
}

#[utoipa::path(get, path = "/api/v1/lists/{id}/digest", params(("id" = String, Path)),
    responses((status = 200, description = "The list's digest volume and next issue number", body = DigestResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "List missing", body = ErrorResponse), (status = 409, description = "Conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn get(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = parse_list_path(s.flavor, &id)?;
    authorize_list(&s, &h, peer(c), "lists:read", &id).await?;
    let list = s.db.lists().get(&id).await?;
    Ok(Json(value(&s, &list)))
}

#[utoipa::path(post, path = "/api/v1/lists/{id}/digest", params(("id" = String, Path)),
    request_body(content((DigestActionInput = "application/json"), (DigestActionInput = "application/x-www-form-urlencoded")), description = "Any of `send`, `bump`, `periodic`"),
    responses((status = 202, description = "Bump applied and issues published as requested", body = DigestActionResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "List missing", body = ErrorResponse), (status = 409, description = "Conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn post(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(input): JsonOrForm<DigestActionInput>,
) -> ApiResult<Response> {
    let id = parse_list_path(s.flavor, &id)?;
    let addr = peer(c);
    let auth = authorize_list(&s, &h, addr, "lists:write", &id).await?;
    s.db.lists().get(&id).await?;
    if input.bump {
        s.db.digests()
            .bump_with_context(&id, &audit_context(&auth, addr))
            .await?;
    }
    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut published = 0;
    if input.send {
        published +=
            s.db.digests()
                .live()
                .flush(&id, now_ms, true, listmngr_db::digests::render)
                .await?;
    }
    if input.periodic {
        published +=
            s.db.digests()
                .live()
                .flush(&id, now_ms, false, listmngr_db::digests::render)
                .await?;
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(DigestActionResponse {
            published,
            bumped: input.bump,
        }),
    )
        .into_response())
}
