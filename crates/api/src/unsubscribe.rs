//! RFC 8058 one-click unsubscribe. `POST /unsubscribe/{list_id}?token=…`
//! with the body `List-Unsubscribe=One-Click` removes the membership the
//! token names without any further interaction, as the RFC requires; `GET`
//! renders a zero-JS page whose form posts the same request for a person
//! who followed the link. The token is the only credential: no session, no
//! CSRF cookie, and the response never echoes the address.
use crate::{ApiError, ApiResult, AppState};
use axum::{
    Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use listmngr_core::{Error, ListId};
use listmngr_db::AuditContext;
use listmngr_web::escape;
use serde::Deserialize;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/unsubscribe/{list_id}", get(confirm).post(unsubscribe))
        .layer(DefaultBodyLimit::max(1024))
}

#[derive(Debug, Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

fn page(title: &str, body: &str) -> Response {
    let mut response = Html(listmngr_web::document(title, body)).into_response();
    for (name, value) in [
        ("cache-control", "no-store"),
        ("referrer-policy", "no-referrer"),
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
        (
            "content-security-policy",
            "default-src 'none'; style-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'",
        ),
    ] {
        response.headers_mut().insert(
            header::HeaderName::from_static(name),
            header::HeaderValue::from_static(value),
        );
    }
    response
}

fn token_of(query: &TokenQuery) -> ApiResult<&str> {
    query
        .token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty() && token.len() <= 128)
        .ok_or_else(|| ApiError(Error::Validation("unsubscribe token".into())))
}

/// The list named in the path, and the membership the token names on it.
async fn lookup(
    state: &AppState,
    list_id: &str,
    token: &str,
) -> ApiResult<(listmngr_core::MailingList, listmngr_core::Member)> {
    let list_id: ListId = list_id
        .parse()
        .map_err(|_| ApiError(Error::NotFound("list".into())))?;
    let list = state
        .db
        .lists()
        .get(&list_id)
        .await
        .map_err(|_| ApiError(Error::NotFound("list".into())))?;
    let member_id = state
        .db
        .one_click()
        .signer()
        .await?
        .verify(&list_id, token, now_secs())
        .ok_or_else(|| ApiError(Error::NotFound("unsubscribe link".into())))?;
    let member = state
        .db
        .members()
        .get(member_id)
        .await
        .map_err(|_| ApiError(Error::NotFound("unsubscribe link".into())))?;
    if member.list_id != list_id || member.role != listmngr_core::MemberRole::Member {
        return Err(ApiError(Error::NotFound("unsubscribe link".into())));
    }
    Ok((list, member))
}

async fn confirm(
    State(state): State<AppState>,
    Path(list_id): Path<String>,
    Query(query): Query<TokenQuery>,
) -> ApiResult<Response> {
    let token = token_of(&query)?;
    let (list, _) = lookup(&state, &list_id, token).await?;
    let body = format!(
        "<p>Confirm that you want to leave the <strong>{}</strong> mailing list ({}).</p><form method=\"post\" action=\"/unsubscribe/{}?token={}\"><input type=\"hidden\" name=\"List-Unsubscribe\" value=\"One-Click\"><button type=\"submit\">Unsubscribe</button></form>",
        escape(&list.display_name),
        escape(&list.id.posting_address()),
        escape(list.id.as_str()),
        escape(token)
    );
    Ok(page("Unsubscribe", &body))
}

async fn unsubscribe(
    State(state): State<AppState>,
    Path(list_id): Path<String>,
    Query(query): Query<TokenQuery>,
    headers: HeaderMap,
    connect: axum::extract::ConnectInfo<std::net::SocketAddr>,
    body: String,
) -> ApiResult<Response> {
    let token = token_of(&query)?;
    // RFC 8058 §3.2: the body is exactly this pair; a browser form sends the
    // same pair.
    let form_ok = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_none_or(|value| value.starts_with("application/x-www-form-urlencoded"));
    let pairs: Vec<(String, String)> = serde_urlencoded::from_str(body.trim())
        .map_err(|_| ApiError(Error::Validation("one-click body".into())))?;
    if !form_ok || pairs.len() != 1 || pairs[0].0 != "List-Unsubscribe" || pairs[0].1 != "One-Click"
    {
        return Err(ApiError(Error::Validation("one-click body".into())));
    }
    state
        .pre_auth_rate
        .check(&format!("one-click:{}", connect.0.ip()))
        .map_err(|retry_after| ApiError(Error::RateLimited { retry_after }))?;
    let (list, _) = lookup(&state, &list_id, token).await?;
    state
        .db
        .one_click()
        .redeem(
            &list.id,
            token,
            now_secs(),
            &AuditContext::new(None, None, Some(connect.0.ip())),
        )
        .await?;
    let body = format!(
        "<p>You have been unsubscribed from the <strong>{}</strong> mailing list ({}). No further mail from this list will be sent to you.</p>",
        escape(&list.display_name),
        escape(&list.id.posting_address())
    );
    let mut response = page("Unsubscribed", &body);
    *response.status_mut() = StatusCode::OK;
    Ok(response)
}
