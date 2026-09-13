//! Read-only list-scoped direct SMTP bounce metadata.
use crate::{
    ApiResult, AppState, BouncePageResponse, ErrorResponse, PageQuery, authorize_list,
    page_response, page_window, parse_list_path, peer,
};
use axum::{
    Json,
    extract::{ConnectInfo, Path, Query, State},
    http::HeaderMap,
};
use listmngr_core::Error;
use serde_json::Value;
use std::net::SocketAddr;

#[utoipa::path(get, path = "/api/v1/lists/{id}/bounces", params(("id" = String, Path), PageQuery),
    responses((status = 200, description = "Direct permanent SMTP failure metadata; no inbound DSNs or scoring", body = BouncePageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "List missing", body = ErrorResponse), (status = 409, description = "Conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn list(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = parse_list_path(s.flavor, &id)?;
    authorize_list(&s, &h, peer(c), "lists:read", &id).await?;
    let total = usize::try_from(s.db.bounces().count(&id).await?)
        .map_err(|_| Error::Validation("bounce count exceeds platform range".into()))?;
    let (start, end) = page_window(total, &q)?;
    let offset = i64::try_from(start).map_err(|_| Error::Validation("offset too large".into()))?;
    let rows = if start == end {
        Vec::new()
    } else {
        s.db.bounces()
            .list(
                &id,
                i64::try_from(end - start).expect("bounded page"),
                offset,
            )
            .await?
    };
    let entries: Vec<Value> = rows
        .iter()
        .map(|row| serde_json::to_value(row).expect("event serializes"))
        .collect();
    Ok(Json(page_response(s.flavor, &entries, start, total)))
}
