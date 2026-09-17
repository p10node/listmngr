//! Archive browser with repository-owned session/membership authorization:
//! the recent-posts list with the index's result count and highlights,
//! one thread as a tree (marking it seen for a signed-in reader), one post
//! with its stored attachments, the owner's reattach form, the opt-in
//! avatar proxy, and the browsing pages: overview, thread lists by
//! activity and month, sender pages keyed by an address digest, and the
//! Atom and RSS feeds.
use crate::{ApiError, ApiResult, AppState};
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Redirect, Response};
use listmngr_archive::render::{Addresses, Mode};
use listmngr_archive::threading::{Node, order};
use listmngr_core::{ArchiveRenderingMode, Error, ListId};
use listmngr_db::archive::ArchiveMessage;
use listmngr_db::archive::browse::{Poster, ThreadSelection, ThreadSummary, month_bounds};
use listmngr_db::web_sessions::WebSession;
use listmngr_web::{ArchiveLinks, Nav, PosterRow, Shell, ThreadRow};
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
impl Default for ArchiveQuery {
    fn default() -> Self {
        Self {
            page: 1,
            q: String::new(),
            thread: String::new(),
            message: None,
            attachment: None,
            format: ArchiveFormat::Html,
            saved: String::new(),
        }
    }
}
fn links(id: &ListId) -> ArchiveLinks {
    let base = format!("/web/lists/{}/archive", id.as_str());
    ArchiveLinks {
        overview_href: format!("{base}/overview"),
        threads_href: format!("{base}/threads"),
        posts_href: base.clone(),
        atom_href: format!("{base}/feed.atom"),
        rss_href: format!("{base}/feed.rss"),
    }
}
fn thread_link(id: &ListId, thread: &str) -> String {
    format!("/web/lists/{}/archive/thread/{thread}", id.as_str())
}
fn sender_link(id: &ListId, email: &str) -> String {
    format!(
        "/web/lists/{}/archive/senders/{}",
        id.as_str(),
        digest(email)
    )
}
/// The digest that names a sender's page and avatar, never the address.
fn digest(email: &str) -> String {
    use sha2::Digest as _;
    format!(
        "{:x}",
        sha2::Sha256::digest(email.trim().to_ascii_lowercase())
    )
}
fn when(date_ms: Option<i64>) -> String {
    date_ms
        .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
        .map(|date| date.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_default()
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
    browse_with(&s, &id, &query, &headers, false).await
}

/// `GET /web/lists/{id}/archive/thread/{thread}`: one thread as a tree,
/// at its canonical address; absent threads are not found.
pub(super) async fn thread_page(
    State(s): State<AppState>,
    Path((id, thread)): Path<(ListId, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    if thread.is_empty() || thread.len() > 200 {
        return Err(Error::Validation("archive query bounds".into()).into());
    }
    let query = ArchiveQuery {
        thread,
        ..ArchiveQuery::default()
    };
    browse_with(&s, &id, &query, &headers, true).await
}

async fn browse_with(
    s: &AppState,
    id: &ListId,
    query: &ArchiveQuery,
    headers: &HeaderMap,
    must_exist: bool,
) -> ApiResult<Response> {
    let session = archive_session(s, headers).await?;
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
    let (messages, total) = select_messages(s, id, query, session.as_ref(), tree).await?;
    if must_exist && messages.is_empty() {
        return Err(Error::NotFound("archive thread".into()).into());
    }
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
    if tree && let Some(session) = &session {
        s.db.archive()
            .browser_mark_viewed(id, session, &query.thread, super::now())
            .await?;
    }
    let viewer = viewer(s, id, headers, session.as_ref()).await?;
    let view = View {
        s,
        id,
        query,
        viewer: &viewer,
        tree,
        terms: if total.is_some() {
            listmngr_archive::render::terms(&query.q)
        } else {
            Vec::new()
        },
    };
    render(&view, &messages, total)
}

/// What the reader's session decides for every archive page.
struct Viewer {
    language: &'static str,
    addresses: Addresses,
    mode: Mode,
    owner: bool,
    csrf: Option<String>,
}

async fn viewer(
    s: &AppState,
    id: &ListId,
    headers: &HeaderMap,
    session: Option<&WebSession>,
) -> ApiResult<Viewer> {
    let language = language_for(s, headers, session).await?;
    let reader = session.and_then(|session| session.user_id);
    let addresses = if reader.is_some() {
        Addresses::Shown
    } else {
        Addresses::Obfuscated
    };
    let owner = match reader {
        Some(_) => s.db.browser_standing(reader, id).await?.administers(),
        None => false,
    };
    let list = s.db.lists().get(id).await?;
    let mode = match list.archive_rendering_mode {
        ArchiveRenderingMode::Markdown => Mode::Markdown,
        ArchiveRenderingMode::Text => Mode::Text,
    };
    Ok(Viewer {
        language,
        addresses,
        mode,
        owner,
        csrf: session.map(|session| session.csrf.clone()),
    })
}

async fn language_for(
    s: &AppState,
    headers: &HeaderMap,
    session: Option<&WebSession>,
) -> ApiResult<&'static str> {
    Ok(match session {
        Some(session) => super::reader_language(s, headers, session).await?,
        None => super::language(s, headers),
    })
}

/// The posts a page shows: one by permalink, one thread as a tree, a page
/// of search hits from the index (with the index's count of all matches),
/// or a page from the database.
async fn select_messages(
    s: &AppState,
    id: &ListId,
    query: &ArchiveQuery,
    session: Option<&WebSession>,
    tree: bool,
) -> ApiResult<(Vec<ArchiveMessage>, Option<usize>)> {
    let mut total = None;
    let messages = if let Some(hash) = &query.message {
        if query.page != 1 || !query.q.is_empty() || !query.thread.is_empty() {
            return Err(Error::Validation(
                "message permalink cannot include search, thread or pagination".into(),
            )
            .into());
        }
        vec![
            s.db.archive()
                .read_browser_message(id, session, hash)
                .await?,
        ]
    } else if tree {
        s.db.archive()
            .read_browser_thread(id, session, &query.thread)
            .await?
    } else if !query.q.is_empty()
        && query.format == ArchiveFormat::Html
        && let Some(index) = s.search_index()
    {
        // The index ranks; the archive's own authorization then reads each
        // hit, so a hit the reader may not see is simply not shown.
        let results = index.search(&listmngr_archive::search::Query {
            list: id.as_str(),
            text: &query.q,
            thread: (!query.thread.is_empty()).then_some(query.thread.as_str()),
            since_ms: None,
            until_ms: None,
            limit: 21,
            offset: usize::try_from(query.page - 1).unwrap_or(0) * 20,
        })?;
        total = Some(results.total);
        let mut messages = Vec::with_capacity(results.hits.len());
        for hit in results.hits {
            match s
                .db
                .archive()
                .read_browser_message(id, session, &hit.hash)
                .await
            {
                Ok(message) => messages.push(message),
                Err(Error::NotFound(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
        messages
    } else {
        s.db.archive()
            .read_browser(
                id,
                session,
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
    Ok((messages, total))
}

/// What one page render needs.
struct View<'a> {
    s: &'a AppState,
    id: &'a ListId,
    query: &'a ArchiveQuery,
    viewer: &'a Viewer,
    tree: bool,
    /// The search words to mark, when the index answered.
    terms: Vec<String>,
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
fn placement(tree: bool, messages: &[ArchiveMessage]) -> Vec<listmngr_archive::threading::Placed> {
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
    message: &ArchiveMessage,
    depth: usize,
) -> ApiResult<listmngr_web::ArchiveMessage> {
    let (id, language) = (view.id, view.viewer.language);
    let quoted = |count: usize| {
        listmngr_i18n::message(
            language,
            "web-archive-quoted",
            &[("count", &count.to_string())],
        )
    };
    let (attachments, unavailable) = attachments(id, message, language)?;
    let avatar = view
        .s
        .config
        .archive
        .gravatar
        .then(|| format!("/web/gravatar/{}", digest(&message.sender_email)));
    let addresses = view.viewer.addresses;
    let body_html =
        listmngr_archive::render::body_html(&message.body, view.viewer.mode, addresses, &quoted);
    Ok(listmngr_web::ArchiveMessage {
        hash: message.hash.clone(),
        subject: message.subject.clone(),
        subject_html: listmngr_archive::render::highlight(
            &listmngr_archive::render::escape(&message.subject),
            &view.terms,
        ),
        thread_href: link(id, "", &message.thread, 1)?,
        sender_href: sender_link(id, &message.sender_email),
        permalink: message_link(id, &message.hash)?,
        sender: message.sender_name.clone(),
        sender_email: listmngr_archive::render::sender_email(&message.sender_email, addresses),
        avatar,
        date: when(message.date_ms),
        in_reply_to: message
            .parent
            .as_ref()
            .map(|parent| message_link(id, parent))
            .transpose()?,
        depth: depth.min(8),
        body_html: listmngr_archive::render::highlight(&body_html, &view.terms),
        attachments,
        attachments_unavailable: unavailable,
    })
}

fn render(
    view: &View<'_>,
    messages: &[ArchiveMessage],
    total: Option<usize>,
) -> ApiResult<Response> {
    let (id, query, language) = (view.id, view.query, view.viewer.language);
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
    let reattach = match (view.viewer.owner, &view.viewer.csrf, &query.message) {
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
        links: links(id),
        results: total.map(|total| {
            listmngr_i18n::message(
                language,
                "web-archive-results",
                &[("count", &total.to_string()), ("query", &query.q)],
            )
        }),
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
    message: &ArchiveMessage,
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

async fn archive_session(s: &AppState, headers: &HeaderMap) -> ApiResult<Option<WebSession>> {
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PageQuery {
    #[serde(default = "first_page")]
    page: u32,
}

fn thread_row(id: &ListId, summary: &ThreadSummary) -> ThreadRow {
    ThreadRow {
        href: thread_link(id, &summary.thread),
        subject: summary.subject.clone(),
        posts: summary.posts,
        participants: summary.participants,
        last: when(Some(summary.last_ms)),
        last_sender: summary.last_sender.clone(),
        unread: summary.unread,
    }
}

fn poster_row(id: &ListId, poster: &Poster, addresses: Addresses) -> PosterRow {
    PosterRow {
        href: sender_link(id, &poster.email),
        name: if poster.name.is_empty() {
            listmngr_archive::render::sender_email(&poster.email, addresses)
        } else {
            poster.name.clone()
        },
        posts: poster.posts,
    }
}

/// `GET /web/lists/{id}/archive/overview`: figures, months, the latest
/// and the most active threads, the senders who posted most.
pub(super) async fn overview(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let session = archive_session(&s, &headers).await?;
    let overview =
        s.db.archive()
            .browser_overview(&id, session.as_ref(), super::now())
            .await?;
    let language = language_for(&s, &headers, session.as_ref()).await?;
    let addresses = if session.as_ref().is_some_and(|s| s.user_id.is_some()) {
        Addresses::Shown
    } else {
        Addresses::Obfuscated
    };
    let page = listmngr_web::ArchiveOverview {
        shell: Shell::titled(
            language,
            listmngr_i18n::message(
                language,
                "web-title-archive-overview",
                &[("list", id.as_str())],
            ),
            Nav::Lists,
        ),
        list: id.as_str().to_owned(),
        links: links(&id),
        posts: overview.posts,
        threads: overview.threads,
        participants: overview.participants,
        months: overview
            .months
            .iter()
            .map(|month| listmngr_web::MonthRow {
                label: format!("{:04}-{:02}", month.year, month.month),
                href: format!(
                    "/web/lists/{}/archive/threads/{:04}/{:02}",
                    id.as_str(),
                    month.year,
                    month.month
                ),
                posts: month.posts,
            })
            .collect(),
        recent: overview.recent.iter().map(|t| thread_row(&id, t)).collect(),
        active: overview.active.iter().map(|t| thread_row(&id, t)).collect(),
        top_posters: overview
            .top_posters
            .iter()
            .map(|p| poster_row(&id, p, addresses))
            .collect(),
    };
    Ok(super::html(&page))
}

/// `GET /web/lists/{id}/archive/threads`: threads by latest activity.
pub(super) async fn threads(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    Query(query): Query<PageQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    threads_page(&s, &id, &headers, query.page, None).await
}

/// `GET /web/lists/{id}/archive/threads/{year}/{month}`: the threads with
/// posts in one month.
pub(super) async fn threads_month(
    State(s): State<AppState>,
    Path((id, year, month)): Path<(ListId, i32, u32)>,
    Query(query): Query<PageQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    if !(1970..=9999).contains(&year) {
        return Err(Error::Validation("archive month".into()).into());
    }
    threads_page(&s, &id, &headers, query.page, Some((year, month))).await
}

async fn threads_page(
    s: &AppState,
    id: &ListId,
    headers: &HeaderMap,
    page: u32,
    month: Option<(i32, u32)>,
) -> ApiResult<Response> {
    if !(1..=5001).contains(&page) {
        return Err(Error::Validation("archive query bounds".into()).into());
    }
    let selection = match month {
        Some((year, month)) => {
            let (from_ms, until_ms) = month_bounds(year, month)?;
            ThreadSelection::Between { from_ms, until_ms }
        }
        None => ThreadSelection::Latest,
    };
    let session = archive_session(s, headers).await?;
    let rows =
        s.db.archive()
            .browser_threads(id, session.as_ref(), selection, i64::from(page - 1) * 20)
            .await?;
    let language = language_for(s, headers, session.as_ref()).await?;
    let base = format!("/web/lists/{}/archive/threads", id.as_str());
    let (heading, base) = match month {
        Some((year, month)) => (
            listmngr_i18n::message(
                language,
                "web-archive-month-threads",
                &[("month", &format!("{year:04}-{month:02}"))],
            ),
            format!("{base}/{year:04}/{month:02}"),
        ),
        None => (
            listmngr_i18n::message(language, "web-archive-latest-threads", &[]),
            base,
        ),
    };
    let page_view = listmngr_web::ArchiveThreads {
        shell: Shell::titled(
            language,
            listmngr_i18n::message(
                language,
                "web-title-archive-threads",
                &[("list", id.as_str())],
            ),
            Nav::Lists,
        ),
        heading,
        links: links(id),
        threads: rows.iter().take(20).map(|t| thread_row(id, t)).collect(),
        previous: (page > 1).then(|| format!("{base}?page={}", page - 1)),
        next: (rows.len() > 20 && page < 5001).then(|| format!("{base}?page={}", page + 1)),
    };
    Ok(super::html(&page_view))
}

/// `GET /web/lists/{id}/archive/senders/{digest}`: one sender's posts,
/// found by the digest of the address.
pub(super) async fn sender(
    State(s): State<AppState>,
    Path((id, wanted)): Path<(ListId, String)>,
    Query(query): Query<PageQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    if wanted.len() != 64 || !wanted.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::Validation("sender digest".into()).into());
    }
    if !(1..=5001).contains(&query.page) {
        return Err(Error::Validation("archive query bounds".into()).into());
    }
    let session = archive_session(&s, &headers).await?;
    let senders =
        s.db.archive()
            .browser_senders(&id, session.as_ref())
            .await?;
    let wanted = wanted.to_ascii_lowercase();
    let matching: Vec<&Poster> = senders
        .iter()
        .filter(|p| digest(&p.email) == wanted)
        .collect();
    let Some(first) = matching.first() else {
        return Err(Error::NotFound("archive sender".into()).into());
    };
    let email = first.email.clone();
    let name = matching
        .iter()
        .find(|p| !p.name.is_empty())
        .map(|p| p.name.clone());
    let posts: i64 = matching.iter().map(|p| p.posts).sum();
    let messages =
        s.db.archive()
            .browser_sender_posts(
                &id,
                session.as_ref(),
                &email,
                i64::from(query.page - 1) * 20,
            )
            .await?;
    let viewer = viewer(&s, &id, &headers, session.as_ref()).await?;
    let default_query = ArchiveQuery::default();
    let view = View {
        s: &s,
        id: &id,
        query: &default_query,
        viewer: &viewer,
        tree: false,
        terms: Vec::new(),
    };
    let language = viewer.language;
    let shown = listmngr_archive::render::sender_email(&email, viewer.addresses);
    let base = format!("/web/lists/{}/archive/senders/{wanted}", id.as_str());
    let page = listmngr_web::ArchiveSender {
        shell: Shell::titled(
            language,
            listmngr_i18n::message(
                language,
                "web-title-archive-sender",
                &[("list", id.as_str())],
            ),
            Nav::Lists,
        ),
        heading: listmngr_i18n::message(
            language,
            "web-archive-sender-title",
            &[("name", name.as_deref().unwrap_or(&shown))],
        ),
        email: shown,
        count: listmngr_i18n::message(
            language,
            "web-archive-sender-posts",
            &[("count", &posts.to_string())],
        ),
        links: links(&id),
        messages: messages
            .iter()
            .take(20)
            .map(|message| message_view(&view, message, 0))
            .collect::<ApiResult<Vec<_>>>()?,
        previous: (query.page > 1).then(|| format!("{base}?page={}", query.page - 1)),
        next: (messages.len() > 20 && query.page < 5001)
            .then(|| format!("{base}?page={}", query.page + 1)),
    };
    let response = super::html(&page);
    Ok(if s.config.archive.gravatar {
        with_images(response)
    } else {
        response
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FeedKind {
    Atom,
    Rss,
}

/// `GET /web/lists/{id}/archive/feed.atom`
pub(super) async fn feed_atom(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    feed(&s, &id, &headers, FeedKind::Atom).await
}

/// `GET /web/lists/{id}/archive/feed.rss`
pub(super) async fn feed_rss(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    feed(&s, &id, &headers, FeedKind::Rss).await
}

/// The latest twenty posts as a feed: subjects, senders' names (never an
/// address), a text summary with addresses obfuscated, absolute links.
async fn feed(
    s: &AppState,
    id: &ListId,
    headers: &HeaderMap,
    kind: FeedKind,
) -> ApiResult<Response> {
    use askama::Template as _;
    use listmngr_archive::render::obfuscate;
    let session = archive_session(s, headers).await?;
    let messages =
        s.db.archive()
            .browser_latest_posts(id, session.as_ref())
            .await?;
    let origin = s.config.site.base_url.trim_end_matches('/');
    let updated = messages
        .iter()
        .filter_map(|m| m.date_ms)
        .max()
        .unwrap_or_else(super::now);
    let stamp = |ms: i64| match kind {
        FeedKind::Atom => rfc3339(ms),
        FeedKind::Rss => rfc2822(ms),
    };
    let mut entries = Vec::with_capacity(messages.len());
    for message in &messages {
        entries.push(listmngr_web::FeedEntry {
            title: message.subject.clone(),
            link: format!("{origin}{}", message_link(id, &message.hash)?),
            date: stamp(message.date_ms.unwrap_or(updated)),
            author: if message.sender_name.is_empty() {
                obfuscate(&message.sender_email)
            } else {
                message.sender_name.clone()
            },
            summary: obfuscate(&message.body).chars().take(500).collect(),
        });
    }
    let title = format!("{} archive", id.as_str());
    let site = format!("{origin}/web/lists/{}/archive", id.as_str());
    let (content_type, body) = match kind {
        FeedKind::Atom => (
            "application/atom+xml; charset=utf-8",
            listmngr_web::AtomFeed {
                title,
                site,
                updated: stamp(updated),
                entries,
            }
            .render(),
        ),
        FeedKind::Rss => (
            "application/rss+xml; charset=utf-8",
            listmngr_web::RssFeed {
                title,
                site,
                updated: stamp(updated),
                entries,
            }
            .render(),
        ),
    };
    let body = body.map_err(|error| ApiError(Error::Validation(error.to_string())))?;
    Ok((
        [
            (header::CONTENT_TYPE, content_type),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        body,
    )
        .into_response())
}

fn rfc3339(ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn rfc2822(ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .unwrap_or_default()
        .to_rfc2822()
}
