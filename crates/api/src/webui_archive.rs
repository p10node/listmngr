//! Archive browser with repository-owned session/membership authorization.
use crate::{ApiError, ApiResult, AppState};
use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use listmngr_core::{Error, ListId};
use listmngr_web::{Nav, Shell};
use serde::{Deserialize, Serialize};

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
    Ok(format!("/web/lists/{}/archive?{encoded}", id.as_str()))
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
    let language = match &session {
        Some(session) => super::reader_language(&s, &headers, session).await?,
        None => super::language(&s, &headers),
    };
    render(&id, &query, &messages, language)
}

fn render(
    id: &ListId,
    query: &ArchiveQuery,
    messages: &[listmngr_db::archive::ArchiveMessage],
    language: &str,
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
    let mut rendered = Vec::new();
    for message in messages.iter().take(20) {
        let (attachments, unavailable) = attachments(id, message, language)?;
        rendered.push(listmngr_web::ArchiveMessage {
            subject: message.subject.clone(),
            thread_href: link(id, &query.q, &message.thread, 1)?,
            permalink: message_link(id, &message.hash)?,
            body: message.body.clone(),
            attachments,
            attachments_unavailable: unavailable,
        });
    }
    Ok(super::html(&listmngr_web::Archive {
        shell: Shell::titled(
            language,
            listmngr_i18n::message(language, "web-title-archive", &[("list", id.as_str())]),
            Nav::Lists,
        ),
        query: query.q.clone(),
        thread: query.thread.clone(),
        all_href: link(id, &query.q, "", 1)?,
        download_href: download_link(id, query)?,
        messages: rendered,
        previous: if query.page > 1 {
            Some(link(id, &query.q, &query.thread, query.page - 1)?)
        } else {
            None
        },
        next: if messages.len() > 20 && query.page < 5001 {
            Some(link(id, &query.q, &query.thread, query.page + 1)?)
        } else {
            None
        },
    }))
}

/// Attachment downloads of one message, and whether the list could not be
/// produced at all because the MIME structure is invalid or over the limits.
fn attachments(
    id: &ListId,
    message: &listmngr_db::archive::ArchiveMessage,
    language: &str,
) -> ApiResult<(Vec<listmngr_web::ArchiveAttachment>, bool)> {
    let Ok(names) = listmngr_mail::attachments::names(&message.raw) else {
        return Ok((Vec::new(), true));
    };
    let permalink = message_link(id, &message.hash)?;
    let links = names
        .iter()
        .enumerate()
        .map(|(index, name)| listmngr_web::ArchiveAttachment {
            href: format!("{permalink}&attachment={index}"),
            label: listmngr_i18n::message(language, "web-archive-attachment", &[("name", name)]),
        })
        .collect();
    Ok((links, false))
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
    Ok(format!("/web/lists/{}/archive?{encoded}", id.as_str()))
}

fn message_link(id: &ListId, hash: &str) -> ApiResult<String> {
    let encoded = serde_urlencoded::to_string([("message", hash)])
        .map_err(|error| Error::Validation(error.to_string()))?;
    Ok(format!("/web/lists/{}/archive?{encoded}", id.as_str()))
}
