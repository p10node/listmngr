//! Archive browser with repository-owned session/membership authorization.
use crate::{ApiError, ApiResult, AppState};
use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use listmngr_core::{Error, ListId};
use listmngr_web::escape;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum ArchiveFormat {
    #[default]
    Html,
    Mbox,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ArchiveQuery {
    #[serde(default = "first_page")]
    page: u32,
    #[serde(default)]
    q: String,
    #[serde(default)]
    thread: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    attachment: Option<u16>,
    #[serde(default)]
    format: ArchiveFormat,
}
const fn first_page() -> u32 {
    1
}
fn link(id: &ListId, q: &str, thread: &str, page: u32) -> ApiResult<String> {
    let encoded =
        serde_urlencoded::to_string([("q", q), ("thread", thread), ("page", &page.to_string())])
            .map_err(|error| Error::Validation(error.to_string()))?;
    Ok(format!(
        "/web/lists/{}/archive?{}",
        escape(id.as_str()),
        escape(&encoded)
    ))
}

pub(super) async fn browse(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    Query(query): Query<ArchiveQuery>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Response> {
    let session = archive_session(&s, &headers).await?;
    if query.attachment.is_some()
        && (query.message.is_none() || query.format != ArchiveFormat::Html)
    {
        return Err(Error::Validation(
            "attachment requires a message permalink and no format override".into(),
        )
        .into());
    }
    if !(1..=5001).contains(&query.page) || query.q.len() > 200 || query.thread.len() > 200 {
        return Err(Error::Validation("archive query bounds".into()).into());
    }
    let messages = if let Some(hash) = &query.message {
        if query.page != 1 || !query.q.is_empty() || !query.thread.is_empty() {
            return Err(Error::Validation(
                "message permalink cannot include search, thread or pagination".into(),
            )
            .into());
        }
        vec![
            s.db.archive()
                .read_browser_message(&id, session.as_ref(), hash)
                .await?,
        ]
    } else {
        s.db.archive()
            .read_browser(
                &id,
                session.as_ref(),
                Some(&query.thread),
                &query.q,
                if query.format == ArchiveFormat::Mbox {
                    20
                } else {
                    21
                },
                i64::from(query.page - 1) * 20,
            )
            .await?
    };
    if let Some(index) = query.attachment {
        let raw = listmngr_mail::attachments::content(&messages[0].raw, usize::from(index))
            .map_err(|_| Error::Validation("attachment MIME limits or invalid message".into()))?
            .ok_or_else(|| Error::NotFound("archive attachment".into()))?;
        return Ok((
            [
                (header::CONTENT_TYPE, "application/octet-stream".to_owned()),
                (
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=attachment-{index}.bin"),
                ),
            ],
            raw,
        )
            .into_response());
    }
    render(&id, &query, &messages)
}

fn render(
    id: &ListId,
    query: &ArchiveQuery,
    messages: &[listmngr_db::archive::ArchiveMessage],
) -> ApiResult<Response> {
    if query.format == ArchiveFormat::Mbox {
        return Ok((
            [
                (header::CONTENT_TYPE, "application/mbox"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=archive.mbox",
                ),
            ],
            listmngr_archive::mbox(messages),
        )
            .into_response());
    }
    let mut body = format!(
        "<form method=\"get\"><label>Search archive <input name=\"q\" value=\"{}\" maxlength=\"200\"></label><input type=\"hidden\" name=\"thread\" value=\"{}\"><button>Search</button></form><p><a href=\"{}\">All threads</a></p><p><a href=\"{}\">Download this selection (mbox)</a></p><p>Downloads contain at most 20 messages, not a complete archive backup.</p>",
        escape(&query.q),
        escape(&query.thread),
        link(id, &query.q, "", 1)?,
        download_link(id, query)?,
    );
    if messages.is_empty() {
        body.push_str("<p>No messages found.</p>");
    }
    for message in messages.iter().take(20) {
        write!(
            body,
            "<article><h2>{}</h2><p><a href=\"{}\">View thread</a> · <a href=\"{}\">Permanent link</a></p><pre>{}</pre>{}</article>",
            escape(&message.subject),
            link(id, &query.q, &message.thread, 1)?,
            message_link(id, &message.hash)?,
            escape(&message.body),
            attachment_links(id, message)?,
        )
        .unwrap();
    }
    if query.page > 1 {
        write!(
            body,
            "<a href=\"{}\">Previous</a> ",
            link(id, &query.q, &query.thread, query.page - 1)?
        )
        .unwrap();
    }
    if messages.len() > 20 && query.page < 5001 {
        write!(
            body,
            "<a href=\"{}\">Next</a>",
            link(id, &query.q, &query.thread, query.page + 1)?
        )
        .unwrap();
    }
    Ok(super::page(&format!("Archive: {id}"), &body))
}

fn attachment_links(
    id: &ListId,
    message: &listmngr_db::archive::ArchiveMessage,
) -> ApiResult<String> {
    let Ok(names) = listmngr_mail::attachments::names(&message.raw) else {
        return Ok(
            "<p>Attachments unavailable: MIME invalid or exceeds attachment limits.</p>".into(),
        );
    };
    let mut links = String::new();
    for (index, name) in names.iter().enumerate() {
        write!(
            links,
            "<p><a href=\"{}&amp;attachment={}\">Download attachment: {}</a></p>",
            message_link(id, &message.hash)?,
            index,
            escape(name)
        )
        .expect("format HTML");
    }
    Ok(links)
}

async fn archive_session(
    s: &AppState,
    headers: &axum::http::HeaderMap,
) -> ApiResult<Option<listmngr_db::web_sessions::WebSession>> {
    match super::load(s, headers).await {
        Ok(session) => Ok(Some(session)),
        Err(ApiError(Error::Authentication)) => Ok(None),
        Err(error) => Err(error),
    }
}

fn download_link(id: &ListId, query: &ArchiveQuery) -> ApiResult<String> {
    let mut query = query.clone();
    query.format = ArchiveFormat::Mbox;
    let encoded =
        serde_urlencoded::to_string(query).map_err(|error| Error::Validation(error.to_string()))?;
    Ok(format!(
        "/web/lists/{}/archive?{}",
        escape(id.as_str()),
        escape(&encoded)
    ))
}

fn message_link(id: &ListId, hash: &str) -> ApiResult<String> {
    let encoded = serde_urlencoded::to_string([("message", hash)])
        .map_err(|error| Error::Validation(error.to_string()))?;
    Ok(format!(
        "/web/lists/{}/archive?{}",
        escape(id.as_str()),
        escape(&encoded)
    ))
}
