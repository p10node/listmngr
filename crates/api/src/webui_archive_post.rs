//! Posting from the web: `GET …/archive/post[?reply=<hash>]` shows the
//! form to a signed-in member with a verified address on the list (the
//! reply form quotes the parent), and `POST …/archive/post` composes the
//! message from that address and injects it into the `in` queue with a
//! context naming the web origin, which the posting chain admits the way
//! an `Approved:` post is admitted for an unmoderated member.
use crate::{ApiResult, AppState};
use axum::extract::{Form, Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use listmngr_core::{Error, ListId};
use listmngr_db::Poster;
use listmngr_db::archive::ArchiveMessage;
use listmngr_db::mail_queue::{NewMessage, Queue};
use listmngr_db::web_sessions::WebSession;
use listmngr_web::{Nav, Shell};
use serde::Deserialize;

/// The longest subject and body the form takes.
const MAX_SUBJECT: usize = 200;
const MAX_BODY: usize = 64 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PostQuery {
    #[serde(default)]
    reply: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PostInput {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    reply: String,
    #[serde(default)]
    subject: String,
    #[serde(default)]
    body: String,
}

/// Who posts, from where, and what they reply to.
struct Draft {
    poster: Poster,
    language: &'static str,
    parent: Option<ArchiveMessage>,
}

/// Everything a form needs before it is shown or accepted: a live session
/// admitted by the archive's policy, a posting address, and the parent
/// when replying (absent parents are not found).
async fn draft(
    s: &AppState,
    id: &ListId,
    headers: &HeaderMap,
    session: &WebSession,
    reply: &str,
) -> ApiResult<Draft> {
    if reply.len() > 200 {
        return Err(Error::Validation("archive message hash bounds".into()).into());
    }
    s.db.archive().browser_authorize(id, Some(session)).await?;
    let poster = s.db.browser_poster(session, id).await?;
    let language = super::reader_language(s, headers, session).await?;
    let parent = if reply.is_empty() {
        None
    } else {
        Some(
            s.db.archive()
                .read_browser_message(id, Some(session), reply)
                .await?,
        )
    };
    Ok(Draft {
        poster,
        language,
        parent,
    })
}

/// The parent's subject without the list's prefix, as a reply subject.
fn reply_subject(prefix: &str, subject: &str) -> String {
    let prefix = prefix.trim();
    let bare = if !prefix.is_empty() && subject.trim_start().starts_with(prefix) {
        subject.trim_start()[prefix.len()..].trim_start()
    } else {
        subject.trim()
    };
    if bare.len() >= 3 && bare[..3].eq_ignore_ascii_case("re:") {
        bare.to_owned()
    } else {
        format!("Re: {bare}")
    }
}

/// The parent's body, each line quoted.
fn quoted(body: &str) -> String {
    body.lines()
        .map(|line| format!("> {}", line.trim_end_matches('\r')))
        .collect::<Vec<_>>()
        .join("\n")
}

fn form_page(
    id: &ListId,
    draft: &Draft,
    csrf: &str,
    subject: String,
    body: String,
    error: Option<&str>,
) -> listmngr_web::ArchivePost {
    let language = draft.language;
    let base = format!("/web/lists/{}/archive", id.as_str());
    listmngr_web::ArchivePost {
        shell: Shell::titled(
            language,
            listmngr_i18n::message(language, "web-title-archive-post", &[("list", id.as_str())]),
            Nav::Lists,
        ),
        links: super::archive::links(id, true),
        heading: listmngr_i18n::message(
            language,
            if draft.parent.is_some() {
                "web-archive-reply"
            } else {
                "web-archive-new-thread"
            },
            &[],
        ),
        action: format!("{base}/post"),
        csrf: csrf.to_owned(),
        sender: draft.poster.email.clone(),
        reply: draft
            .parent
            .as_ref()
            .map(|parent| parent.hash.clone())
            .unwrap_or_default(),
        subject,
        body,
        error: error.map(|id| listmngr_i18n::message(language, id, &[])),
    }
}

/// `GET /web/lists/{id}/archive/post`: the form, quoting the parent when
/// replying.
pub(super) async fn form(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    Query(query): Query<PostQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let session = super::load(&s, &headers).await?;
    let draft = draft(&s, &id, &headers, &session, &query.reply).await?;
    let (subject, body) = match &draft.parent {
        Some(parent) => {
            let prefix = s.db.lists().get(&id).await?.subject_prefix;
            (
                reply_subject(&prefix, &parent.subject),
                quoted(&parent.body),
            )
        }
        None => (String::new(), String::new()),
    };
    Ok(super::html(&form_page(
        &id,
        &draft,
        &session.csrf,
        subject,
        body,
        None,
    )))
}

/// Why a submission is refused, as a catalog id.
fn refusal(subject: &str, body: &str) -> Option<&'static str> {
    if subject.trim().is_empty() {
        Some("web-archive-post-no-subject")
    } else if subject.contains(['\r', '\n']) {
        Some("web-archive-post-subject-lines")
    } else if subject.trim().len() > MAX_SUBJECT {
        Some("web-archive-post-subject-long")
    } else if body.trim().is_empty() {
        Some("web-archive-post-no-body")
    } else if body.len() > MAX_BODY {
        Some("web-archive-post-body-long")
    } else {
        None
    }
}

/// `POST /web/lists/{id}/archive/post`: compose and inject.
pub(super) async fn submit(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    headers: HeaderMap,
    Form(input): Form<PostInput>,
) -> ApiResult<Response> {
    let session = super::write_session(&s, &headers, &input.csrf).await?;
    let draft = draft(&s, &id, &headers, &session, input.reply.trim()).await?;
    if let Some(error) = refusal(&input.subject, &input.body) {
        let page = form_page(
            &id,
            &draft,
            &session.csrf,
            input.subject,
            input.body,
            Some(error),
        );
        return Ok(super::inline_refusal(super::html(&page)));
    }
    let in_reply_to = match &draft.parent {
        Some(parent) => Some(
            listmngr_mail::parse_message_id(&parent.raw)
                .map_err(|_| Error::Validation("the parent has no Message-ID".into()))?,
        ),
        None => None,
    };
    let now = chrono::Utc::now();
    let message_id = format!("<web.{}@{}>", uuid::Uuid::now_v7().simple(), id.mail_host());
    let raw = listmngr_mail::web_post::compose(&listmngr_mail::web_post::WebPost {
        from_name: &draft.poster.display_name,
        from_email: &draft.poster.email,
        to: &id.posting_address(),
        subject: input.subject.trim(),
        body: &input.body.replace("\r\n", "\n"),
        in_reply_to: in_reply_to.as_deref(),
        message_id: &message_id,
        date_secs: now.timestamp(),
    })
    .map_err(|error| Error::Validation(error.to_string()))?;
    let hash = listmngr_mail::message_id_hash(&message_id)
        .map_err(|_| Error::Validation("message id".into()))?;
    let reply_hash = draft.parent.as_ref().map(|parent| parent.hash.clone());
    s.db.mail_queue()
        .enqueue(
            NewMessage {
                raw,
                external_id: message_id,
                context: serde_json::json!({
                    "version": 1,
                    "list_id": id.as_str(),
                    "envelope_sender": draft.poster.email,
                    "message_id_hash": hash,
                    "web_post": {
                        "user_id": draft.poster.user_id.to_string(),
                        "address": draft.poster.email,
                        "reply": reply_hash,
                    },
                })
                .to_string(),
                queue: Queue::In,
                max_attempts: 5,
            },
            now.timestamp_millis(),
        )
        .await?;
    let back = draft.parent.as_ref().map_or_else(
        || format!("/web/lists/{}/archive?saved=posted", id.as_str()),
        |parent| {
            format!(
                "{}?saved=posted",
                super::archive::thread_link(&id, &parent.thread)
            )
        },
    );
    Ok(Redirect::to(&back).into_response())
}
