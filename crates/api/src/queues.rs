//! Mailman's `/queues`: the runner queues, the jobs waiting in each, and
//! injection of a message into the `in` queue, on both API prefixes.
use crate::{
    ApiError, ApiResult, AppState, ErrorResponse, JsonOrForm, QueuePageResponse,
    authenticate_for_authorization, authorize_list, finish_authorization, is_unbound,
    parse_list_path, peer,
};
use axum::{
    Json, Router,
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing,
};
use listmngr_core::Error;
use listmngr_db::mail_queue::{JobId, NewMessage, Queue};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::net::SocketAddr;

/// Jobs listed per queue, as Mailman lists queue files.
const LISTED_JOBS: i64 = 1000;

/// Injected messages are bounded like the CLI's `queue inject`.
const MAX_TEXT_BYTES: usize = 10 * 1024 * 1024;

/// One queue, in Mailman's shape: `files` are the waiting job ids.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct QueueResponse {
    pub name: String,
    /// Where the queue lives; jobs are database rows, not files.
    pub directory: String,
    /// Ids of the jobs still waiting, oldest first, at most 1000.
    pub files: Vec<String>,
    /// Every waiting job, counted.
    pub count: i64,
    pub self_link: String,
}

/// Mailman's `inject`: a list and the raw message text.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct InjectInput {
    list_id: String,
    /// The complete RFC 5322 message; `From` supplies the envelope sender.
    text: String,
}

/// One job's metadata; the raw message is never served here.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct JobResponse {
    pub id: String,
    pub queue: String,
    pub state: String,
    pub attempts: i64,
    pub max_attempts: i64,
    /// Milliseconds since the epoch the job becomes due.
    pub run_after: i64,
    pub last_error: String,
    pub self_link: String,
}

/// The queue routes, mounted under both API prefixes.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/queues", routing::get(list))
        .route("/queues/{name}", routing::get(get).post(inject))
        .route("/queues/{name}/{id}", routing::get(job))
}

const fn prefix(state: &AppState) -> &'static str {
    match state.flavor {
        crate::ApiFlavor::V1 => "/api/v1",
        crate::ApiFlavor::Compat31 => "/3.1",
    }
}

fn queue_named(name: &str) -> ApiResult<Queue> {
    Queue::from_name(name).ok_or_else(|| Error::NotFound(format!("queue {name}")).into())
}

/// Queues are server operation: the scope must be held by a token bound to
/// no list or domain.
async fn authorize_site(
    state: &AppState,
    headers: &HeaderMap,
    addr: SocketAddr,
    scope: &str,
) -> ApiResult<listmngr_db::TokenAuth> {
    let auth = authenticate_for_authorization(state, headers, addr, scope).await?;
    if !is_unbound(&auth) {
        return Err(ApiError(Error::Forbidden(scope.into())));
    }
    finish_authorization(state, auth).await
}

async fn queue_value(state: &AppState, queue: Queue) -> ApiResult<Value> {
    let files: Vec<String> = state
        .db
        .mail_queue()
        .pending_ids(queue, LISTED_JOBS)
        .await?
        .iter()
        .map(|id| id.0.to_string())
        .collect();
    let count = state.db.mail_queue().pending_count(queue).await?;
    let mut value = json!(QueueResponse {
        name: queue.name().into(),
        directory: format!("queue_jobs/{}", queue.name()),
        files,
        count,
        self_link: format!("{}/queues/{}", prefix(state), queue.name()),
    });
    if matches!(state.flavor, crate::ApiFlavor::Compat31) {
        value
            .as_object_mut()
            .expect("queue object")
            .insert("http_etag".into(), json!("phase1"));
    }
    Ok(value)
}

#[utoipa::path(get, path = "/api/v1/queues",
    responses((status = 200, description = "Every queue with its waiting jobs", body = QueuePageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope or bound token", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn list(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize_site(&s, &h, peer(c), "system:read").await?;
    let mut entries = Vec::with_capacity(Queue::ALL.len());
    for queue in Queue::ALL {
        entries.push(queue_value(&s, queue).await?);
    }
    Ok(Json(crate::page_response(
        s.flavor,
        &entries,
        0,
        entries.len(),
    )))
}

#[utoipa::path(get, path = "/api/v1/queues/{name}", params(("name" = String, Path, description = "Queue name, e.g. `in`")),
    responses((status = 200, description = "The queue and its waiting jobs", body = QueueResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope or bound token", body = ErrorResponse), (status = 404, description = "No such queue", body = ErrorResponse), (status = 409, description = "Conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn get(
    State(s): State<AppState>,
    Path(name): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize_site(&s, &h, peer(c), "system:read").await?;
    let queue = queue_named(&name)?;
    Ok(Json(queue_value(&s, queue).await?))
}

#[utoipa::path(get, path = "/api/v1/queues/{name}/{id}", params(("name" = String, Path, description = "Queue name"), ("id" = String, Path, description = "Job id")),
    responses((status = 200, description = "The job's metadata", body = JobResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope or bound token", body = ErrorResponse), (status = 404, description = "No such queue or job in it", body = ErrorResponse), (status = 409, description = "Conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn job(
    State(s): State<AppState>,
    Path((name, id)): Path<(String, String)>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize_site(&s, &h, peer(c), "system:read").await?;
    let queue = queue_named(&name)?;
    let id: uuid::Uuid = id
        .parse()
        .map_err(|_| Error::NotFound(format!("queue job {id}")))?;
    let job = s.db.mail_queue().job(JobId(id)).await?;
    if job.queue != queue {
        return Err(Error::NotFound(format!("queue job {id}")).into());
    }
    let state = serde_json::to_value(job.state).expect("job state serializes");
    let mut value = json!(JobResponse {
        id: id.to_string(),
        queue: queue.name().into(),
        state: state.as_str().expect("job state string").into(),
        attempts: job.attempts,
        max_attempts: job.max_attempts,
        run_after: job.run_after,
        last_error: job.last_error,
        self_link: format!("{}/queues/{}/{id}", prefix(&s), queue.name()),
    });
    if matches!(s.flavor, crate::ApiFlavor::Compat31) {
        value
            .as_object_mut()
            .expect("job object")
            .insert("http_etag".into(), json!("phase1"));
    }
    Ok(Json(value))
}

/// The mailbox in the message's `From`, as the CLI's `--sender` would be.
fn envelope_sender(raw: &[u8]) -> ApiResult<String> {
    let from = listmngr_mail::header_value(raw, "From")
        .ok_or_else(|| Error::Validation("message has no From header".into()))?;
    let inner = from
        .rsplit_once('<')
        .and_then(|(_, rest)| rest.split_once('>'))
        .map_or(from.as_str(), |(address, _)| address)
        .trim();
    Ok(listmngr_core::Address::new(inner, String::new())
        .map_err(|_| Error::Validation("From is not a mailbox".into()))?
        .email)
}

#[utoipa::path(post, path = "/api/v1/queues/{name}", params(("name" = String, Path, description = "Only `in` accepts injection")),
    request_body(content((InjectInput = "application/json"), (InjectInput = "application/x-www-form-urlencoded"))),
    responses((status = 201, description = "Message stored in the queue; the job's location follows", body = JobResponse), (status = 400, description = "Invalid message, missing From or Message-ID, oversized text, or a queue that takes no injection", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "No such queue or list", body = ErrorResponse), (status = 409, description = "Conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn inject(
    State(s): State<AppState>,
    Path(name): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(input): JsonOrForm<InjectInput>,
) -> ApiResult<Response> {
    let queue = queue_named(&name)?;
    let list = parse_list_path(s.flavor, &input.list_id)?;
    authorize_list(&s, &h, peer(c), "lists:write", &list).await?;
    if queue != Queue::In {
        return Err(Error::Validation("only the in queue accepts injected messages".into()).into());
    }
    s.db.lists().get(&list).await?;
    if input.text.len() > MAX_TEXT_BYTES {
        return Err(Error::Validation("message exceeds intake limit".into()).into());
    }
    // Mailman's text is a Python string; store it with network line ends.
    let raw = input
        .text
        .replace("\r\n", "\n")
        .replace('\n', "\r\n")
        .into_bytes();
    let sender = envelope_sender(&raw)?;
    let external_id = listmngr_mail::parse_message_id(&raw)
        .map_err(|_| Error::Validation("invalid message metadata".into()))?;
    let hash = listmngr_mail::message_id_hash(&external_id)
        .map_err(|_| Error::Validation("invalid message metadata".into()))?;
    let job = s
        .db
        .mail_queue()
        .enqueue(
            NewMessage {
                raw,
                external_id,
                context: json!({"version": 1, "list_id": list, "envelope_sender": sender, "message_id_hash": hash}).to_string(),
                queue: Queue::In,
                max_attempts: 5,
            },
            chrono::Utc::now().timestamp_millis(),
        )
        .await?;
    let location = format!("{}/queues/in/{}", prefix(&s), job.id.0);
    Ok((
        StatusCode::CREATED,
        [(header::LOCATION, location.clone())],
        Json(json!({"id": job.id.0.to_string(), "queue": "in", "self_link": location})),
    )
        .into_response())
}
