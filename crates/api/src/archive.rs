//! Native archive routes. All projections share repository policy/ownership gates.
use super::{
    ApiResult, AppState, ConnectInfo, Deserialize, HeaderMap, HeaderValue, Html, IntoResponse,
    Json, ListId, Path, Query, Response, Router, SocketAddr, State, authenticate_for_authorization,
    finish_authorization, get, header, peer,
};
use askama::Template;
#[derive(Debug, Default, Deserialize)]
pub struct ArchiveQuery {
    q: Option<String>,
    count: Option<i64>,
    offset: Option<i64>,
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/archives/{list}/messages", get(messages))
        .route("/api/v1/archives/{list}/threads/{thread}", get(thread))
        .route("/api/v1/archives/{list}/export.mbox", get(export))
        .route("/archives/{list}", get(page))
        .route("/archives/{list}/thread/{thread}", get(thread_page))
}
async fn read(
    state: &AppState,
    headers: &HeaderMap,
    addr: SocketAddr,
    list: &ListId,
    thread: Option<&str>,
    query: &ArchiveQuery,
) -> ApiResult<Vec<listmngr_db::archive::ArchiveMessage>> {
    let auth = if headers.contains_key(header::AUTHORIZATION) {
        Some(authenticate_for_authorization(state, headers, addr, "members:read").await?)
    } else {
        None
    };
    let rows = state
        .db
        .archive()
        .read(
            list,
            auth.as_ref(),
            thread,
            query.q.as_deref().unwrap_or(""),
            query.count.unwrap_or(50),
            query.offset.unwrap_or(0),
        )
        .await?;
    if let Some(auth) = auth {
        finish_authorization(state, auth).await?;
    }
    Ok(rows)
}
fn secured(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
        .headers_mut()
        .insert(header::VARY, HeaderValue::from_static("Authorization"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; style-src 'self'; base-uri 'none'; frame-ancestors 'none'",
        ),
    );
    response
}
async fn messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    Path(list): Path<ListId>,
    Query(query): Query<ArchiveQuery>,
) -> ApiResult<Response> {
    Ok(secured(
        Json(read(&state, &headers, peer(connect), &list, None, &query).await?).into_response(),
    ))
}
async fn thread(
    State(state): State<AppState>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    Path((list, thread)): Path<(ListId, String)>,
    Query(query): Query<ArchiveQuery>,
) -> ApiResult<Response> {
    Ok(secured(
        Json(
            read(
                &state,
                &headers,
                peer(connect),
                &list,
                Some(&thread),
                &query,
            )
            .await?,
        )
        .into_response(),
    ))
}
async fn export(
    State(state): State<AppState>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    Path(list): Path<ListId>,
    Query(query): Query<ArchiveQuery>,
) -> ApiResult<Response> {
    let rows = read(&state, &headers, peer(connect), &list, None, &query).await?;
    Ok(secured(
        (
            [
                (header::CONTENT_TYPE, "application/mbox"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=archive.mbox",
                ),
            ],
            listmngr_archive::mbox(&rows),
        )
            .into_response(),
    ))
}
fn render(list: &ListId, rows: &[listmngr_db::archive::ArchiveMessage]) -> Response {
    let entries = rows
        .iter()
        .map(|row| listmngr_web::ArchiveCompatEntry {
            // Hashes are base32 identifiers from the parser; the template
            // escapes them unconditionally all the same.
            href: format!("/archives/{}/thread/{}", list.as_str(), row.thread),
            subject: row.subject.clone(),
            body: row.body.clone(),
        })
        .collect();
    let page = listmngr_web::ArchiveCompat {
        list: list.to_string(),
        entries,
    };
    secured(Html(page.render().expect("template renders")).into_response())
}
async fn page(
    State(state): State<AppState>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    Path(list): Path<ListId>,
    Query(query): Query<ArchiveQuery>,
) -> ApiResult<Response> {
    Ok(render(
        &list,
        &read(&state, &headers, peer(connect), &list, None, &query).await?,
    ))
}
async fn thread_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    Path((list, thread)): Path<(ListId, String)>,
    Query(query): Query<ArchiveQuery>,
) -> ApiResult<Response> {
    Ok(render(
        &list,
        &read(
            &state,
            &headers,
            peer(connect),
            &list,
            Some(&thread),
            &query,
        )
        .await?,
    ))
}
