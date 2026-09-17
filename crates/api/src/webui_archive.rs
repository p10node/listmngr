//! Archive browser with repository-owned session/membership authorization:
//! the recent-posts list, one thread as a tree, one post with its stored
//! attachments, the owner's reattach form, and the opt-in avatar proxy.
use crate::{ApiError, ApiResult, AppState};
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Redirect, Response};
use listmngr_archive::render::{Addresses, Mode};
use listmngr_archive::threading::{Node, order};
use listmngr_core::{ArchiveRenderingMode, Error, ListId};
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
    #[serde(default, skip_serializing_if = "String::is_empty")]
    saved: String,
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

/// Archive pages may show avatars from this origin; nothing else changes.
fn with_images(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; img-src 'self'; style-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'",
        ),
    );
    response
}

pub(super) async fn browse(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    Query(query): Query<ArchiveQuery>,
    headers: HeaderMap,
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
    let tree = query.message.is_none()
        && !query.thread.is_empty()
        && query.q.is_empty()
        && query.format == ArchiveFormat::Html;
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
    } else if tree {
        s.db.archive()
            .read_browser_thread(&id, session.as_ref(), &query.thread)
            .await?
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
        // The legacy MIME projection, kept for links made before attachments
        // were stored; new pages link the stored rows.
        let raw = listmngr_mail::attachments::content(&messages[0].raw, usize::from(index))
            .map_err(|_| Error::Validation("attachment MIME limits or invalid message".into()))?
            .ok_or_else(|| Error::NotFound("archive attachment".into()))?;
        return Ok(download(
            "application/octet-stream",
            &format!("attachment-{index}.bin"),
            raw,
        ));
    }
    let language = match &session {
        Some(session) => super::reader_language(&s, &headers, session).await?,
        None => super::language(&s, &headers),
    };
    let reader = session.as_ref().and_then(|session| session.user_id);
    let addresses = if reader.is_some() {
        Addresses::Shown
    } else {
        Addresses::Obfuscated
    };
    let owner = match reader {
        Some(_) => s.db.browser_standing(reader, &id).await?.administers(),
        None => false,
    };
    let list = s.db.lists().get(&id).await?;
    let mode = match list.archive_rendering_mode {
        ArchiveRenderingMode::Markdown => Mode::Markdown,
        ArchiveRenderingMode::Text => Mode::Text,
    };
    let view = View {
        s: &s,
        id: &id,
        query: &query,
        language,
        addresses,
        mode,
        owner,
        tree,
        csrf: session.as_ref().map(|session| session.csrf.clone()),
    };
    render(&view, &messages)
}

/// What one page render needs.
struct View<'a> {
    s: &'a AppState,
    id: &'a ListId,
    query: &'a ArchiveQuery,
    language: &'static str,
    addresses: Addresses,
    mode: Mode,
    owner: bool,
    tree: bool,
    csrf: Option<String>,
}

fn download(content_type: &str, filename: &str, bytes: Vec<u8>) -> Response {
    let name: String = filename
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .take(120)
        .collect();
    let name = if name.is_empty() {
        "attachment".to_owned()
    } else {
        name
    };
    (
        [
            (header::CONTENT_TYPE, content_type.to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
        ],
        bytes,
    )
        .into_response()
}

/// Types a browser might render as a document are served as bytes.
fn safe_content_type(declared: &str) -> &str {
    let lower = declared.trim().to_ascii_lowercase();
    if lower.starts_with("text/html")
        || lower.starts_with("image/svg")
        || lower.contains("xml")
        || lower.contains("javascript")
        || lower.contains("ecmascript")
        || lower.is_empty()
        || !lower.contains('/')
    {
        "application/octet-stream"
    } else {
        declared
    }
}

/// The posts of a page in display order with their depths: the tree
/// order for a thread view, the stored order (all roots) otherwise.
fn placement(
    tree: bool,
    messages: &[listmngr_db::archive::ArchiveMessage],
) -> Vec<listmngr_archive::threading::Placed> {
    if tree {
        return order(
            &messages
                .iter()
                .map(|m| Node {
                    hash: m.hash.clone(),
                    parent: m.parent.clone(),
                    date_ms: m.date_ms.unwrap_or(0),
                })
                .collect::<Vec<_>>(),
        );
    }
    messages
        .iter()
        .take(20)
        .map(|m| listmngr_archive::threading::Placed {
            hash: m.hash.clone(),
            depth: 0,
        })
        .collect()
}

/// One post as the page shows it.
fn message_view(
    view: &View<'_>,
    message: &listmngr_db::archive::ArchiveMessage,
    depth: usize,
) -> ApiResult<listmngr_web::ArchiveMessage> {
    let (id, language) = (view.id, view.language);
    let quoted = |count: usize| {
        listmngr_i18n::message(
            language,
            "web-archive-quoted",
            &[("count", &count.to_string())],
        )
    };
    let (attachments, unavailable) = attachments(id, message, language)?;
    let avatar = view.s.config.archive.gravatar.then(|| {
        use sha2::Digest as _;
        let digest = sha2::Sha256::digest(message.sender_email.trim().to_ascii_lowercase());
        format!("/web/gravatar/{digest:x}")
    });
    Ok(listmngr_web::ArchiveMessage {
        hash: message.hash.clone(),
        subject: message.subject.clone(),
        thread_href: link(id, "", &message.thread, 1)?,
        permalink: message_link(id, &message.hash)?,
        sender: message.sender_name.clone(),
        sender_email: listmngr_archive::render::sender_email(&message.sender_email, view.addresses),
        avatar,
        date: message
            .date_ms
            .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
            .map(|date| date.format("%Y-%m-%d %H:%M UTC").to_string())
            .unwrap_or_default(),
        in_reply_to: message
            .parent
            .as_ref()
            .map(|parent| message_link(id, parent))
            .transpose()?,
        depth: depth.min(8),
        body_html: listmngr_archive::render::body_html(
            &message.body,
            view.mode,
            view.addresses,
            &quoted,
        ),
        attachments,
        attachments_unavailable: unavailable,
    })
}

fn render(
    view: &View<'_>,
    messages: &[listmngr_db::archive::ArchiveMessage],
) -> ApiResult<Response> {
    let (id, query, language) = (view.id, view.query, view.language);
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
    for place in placement(view.tree, messages) {
        if let Some(message) = messages.iter().find(|m| m.hash == place.hash) {
            rendered.push(message_view(view, message, place.depth)?);
        }
    }
    let reattach = match (view.owner, &view.csrf, &query.message) {
        (true, Some(csrf), Some(hash)) => Some(listmngr_web::ReattachForm {
            action: format!("/web/lists/{}/archive/reattach", id.as_str()),
            csrf: csrf.clone(),
            message: hash.clone(),
            parent: messages
                .first()
                .and_then(|m| m.parent.clone())
                .unwrap_or_default(),
        }),
        _ => None,
    };
    let notice = match query.saved.as_str() {
        "reattached" => Some(listmngr_i18n::message(
            language,
            "web-archive-reattached",
            &[],
        )),
        "cycle" => Some(listmngr_i18n::message(
            language,
            "web-archive-reattach-refused",
            &[],
        )),
        _ => None,
    };
    let paged = !view.tree;
    let page = listmngr_web::Archive {
        shell: Shell::titled(
            language,
            listmngr_i18n::message(language, "web-title-archive", &[("list", id.as_str())]),
            Nav::Lists,
        ),
        query: query.q.clone(),
        thread: query.thread.clone(),
        all_href: link(id, &query.q, "", 1)?,
        download_href: download_link(id, query)?,
        tree: view.tree,
        messages: rendered,
        previous: if paged && query.page > 1 {
            Some(link(id, &query.q, &query.thread, query.page - 1)?)
        } else {
            None
        },
        next: if paged && messages.len() > 20 && query.page < 5001 {
            Some(link(id, &query.q, &query.thread, query.page + 1)?)
        } else {
            None
        },
        reattach,
        notice,
    };
    let response = super::html(&page);
    Ok(if view.s.config.archive.gravatar {
        with_images(response)
    } else {
        response
    })
}

/// Attachment downloads of one message: the stored rows when the post was
/// indexed with them, else the MIME projection; and whether that could not
/// be produced at all.
fn attachments(
    id: &ListId,
    message: &listmngr_db::archive::ArchiveMessage,
    language: &str,
) -> ApiResult<(Vec<listmngr_web::ArchiveAttachment>, bool)> {
    if !message.attachments.is_empty() {
        let links = message
            .attachments
            .iter()
            .map(|stored| listmngr_web::ArchiveAttachment {
                href: format!(
                    "/web/lists/{}/archive/attachments/{}/{}",
                    id.as_str(),
                    message.hash,
                    stored.position
                ),
                label: listmngr_i18n::message(
                    language,
                    "web-archive-attachment",
                    &[("name", &stored.filename)],
                ),
            })
            .collect();
        return Ok((links, false));
    }
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

/// `GET /web/lists/{id}/archive/attachments/{hash}/{position}`: a stored
/// attachment, as a download, never as a document.
pub(super) async fn attachment(
    State(s): State<AppState>,
    Path((id, hash, position)): Path<(ListId, String, i64)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let session = archive_session(&s, &headers).await?;
    if !(0..=64).contains(&position) {
        return Err(Error::NotFound("archive attachment".into()).into());
    }
    let stored =
        s.db.archive()
            .read_browser_attachment(&id, session.as_ref(), &hash, position)
            .await?;
    Ok(download(
        safe_content_type(&stored.content_type),
        &stored.filename,
        stored.content,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReattachInput {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    parent: String,
}

/// `POST /web/lists/{id}/archive/reattach`: the owner moves a post under
/// another, or makes it a root, and its replies follow.
pub(super) async fn reattach(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    headers: HeaderMap,
    Form(form): Form<ReattachInput>,
) -> ApiResult<Response> {
    let session = super::write_session(&s, &headers, &form.csrf).await?;
    super::privileged(&s, &session).await?;
    let parent = form.parent.trim();
    let parent = (!parent.is_empty()).then_some(parent);
    let back = message_link(&id, &form.message)?;
    match s
        .db
        .archive()
        .browser_reattach(&session, &id, form.message.trim(), parent)
        .await
    {
        Ok(()) => Ok(Redirect::to(&format!("{back}&saved=reattached")).into_response()),
        Err(Error::Validation(_)) => {
            Ok(Redirect::to(&format!("{back}&saved=cycle")).into_response())
        }
        Err(error) => Err(error.into()),
    }
}

/// `GET /web/gravatar/{hash}`: the sender's Gravatar, fetched by this
/// server and cached, only when `[archive] gravatar` is on.
pub(super) async fn gravatar(
    State(s): State<AppState>,
    Path(hash): Path<String>,
) -> ApiResult<Response> {
    if !s.config.archive.gravatar {
        return Err(Error::NotFound("avatar".into()).into());
    }
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::Validation("avatar hash".into()).into());
    }
    let hash = hash.to_ascii_lowercase();
    if let Some(entry) = s.avatars.get(&hash)
        && entry.0.elapsed() < std::time::Duration::from_secs(3600)
    {
        return Ok(avatar_response(&entry.1, entry.2.clone()));
    }
    let url = format!("{}{hash}?d=identicon&s=80", s.config.archive.gravatar_url);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| ApiError(Error::NotFound("avatar".into())))?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|_| ApiError(Error::NotFound("avatar".into())))?;
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    if !response.status().is_success() || !content_type.starts_with("image/") {
        return Err(Error::NotFound("avatar".into()).into());
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| ApiError(Error::NotFound("avatar".into())))?;
    if bytes.len() > 262_144 {
        return Err(Error::NotFound("avatar".into()).into());
    }
    if s.avatars.len() > 2000 {
        s.avatars.clear();
    }
    s.avatars.insert(
        hash,
        (
            std::time::Instant::now(),
            content_type.clone(),
            bytes.clone(),
        ),
    );
    Ok(avatar_response(&content_type, bytes))
}

fn avatar_response(content_type: &str, bytes: bytes::Bytes) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type.to_owned()),
            (header::CACHE_CONTROL, "public, max-age=3600".to_owned()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
        ],
        bytes,
    )
        .into_response()
}

async fn archive_session(
    s: &AppState,
    headers: &HeaderMap,
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
    query.saved = String::new();
    let encoded =
        serde_urlencoded::to_string(query).map_err(|error| Error::Validation(error.to_string()))?;
    Ok(format!("/web/lists/{}/archive?{encoded}", id.as_str()))
}

fn message_link(id: &ListId, hash: &str) -> ApiResult<String> {
    let encoded = serde_urlencoded::to_string([("message", hash)])
        .map_err(|error| Error::Validation(error.to_string()))?;
    Ok(format!("/web/lists/{}/archive?{encoded}", id.as_str()))
}
