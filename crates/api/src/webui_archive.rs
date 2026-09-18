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
use listmngr_core::{ArchiveRenderingMode, Error, ListId, UserId};
use listmngr_db::archive::ArchiveMessage;
use listmngr_db::archive::browse::{Poster, ThreadSelection, ThreadSummary, month_bounds};
use listmngr_db::archive::interact::{ThreadMeta, VoteSummary};
use listmngr_db::web_sessions::WebSession;
use listmngr_web::{ArchiveLinks, Nav, PosterRow, Shell, TagLink, ThreadRow};
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
pub(super) fn links(id: &ListId, signed_in: bool) -> ArchiveLinks {
    let base = format!("/web/lists/{}/archive", id.as_str());
    ArchiveLinks {
        overview_href: format!("{base}/overview"),
        threads_href: format!("{base}/threads"),
        posts_href: base.clone(),
        atom_href: format!("{base}/feed.atom"),
        rss_href: format!("{base}/feed.rss"),
        favorites_href: signed_in.then(|| format!("{base}/favorites")),
        post_href: signed_in.then(|| format!("{base}/post")),
    }
}
pub(super) fn thread_link(id: &ListId, thread: &str) -> String {
    format!("/web/lists/{}/archive/thread/{thread}", id.as_str())
}
/// A tag or a category as a link to its thread list.
fn label_link(id: &ListId, kind: &str, name: &str) -> TagLink {
    TagLink {
        name: name.to_owned(),
        href: format!("/web/lists/{}/archive/{kind}/{name}", id.as_str()),
        removable: false,
        remove_label: String::new(),
    }
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
    Query(query): Query<SavedQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    if thread.is_empty() || thread.len() > 200 || query.saved.len() > 40 {
        return Err(Error::Validation("archive query bounds".into()).into());
    }
    let query = ArchiveQuery {
        thread,
        saved: query.saved,
        ..ArchiveQuery::default()
    };
    browse_with(&s, &id, &query, &headers, true).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SavedQuery {
    #[serde(default)]
    saved: String,
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
    let reader = session.as_ref().and_then(|session| session.user_id);
    let (votes, meta) = interactions(s, id, query, reader, tree, &messages).await?;
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
        reader,
        votes,
        meta,
    };
    render(&view, &messages, total)
}

/// The scores of the page's posts and, on a thread page, the thread's
/// tags, category and favourite mark; nothing for an mbox download.
async fn interactions(
    s: &AppState,
    id: &ListId,
    query: &ArchiveQuery,
    reader: Option<UserId>,
    tree: bool,
    messages: &[ArchiveMessage],
) -> ApiResult<(Vec<VoteSummary>, Option<ThreadMeta>)> {
    if query.format != ArchiveFormat::Html {
        return Ok((Vec::new(), None));
    }
    let hashes: Vec<String> = messages.iter().map(|m| m.hash.clone()).collect();
    let votes = s.db.archive().browser_votes(id, reader, &hashes).await?;
    let meta = if tree {
        Some(
            s.db.archive()
                .browser_thread_meta(id, reader, &query.thread)
                .await?,
        )
    } else {
        None
    };
    Ok((votes, meta))
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
    /// The signed-in reader.
    reader: Option<UserId>,
    /// The page's posts' scores.
    votes: Vec<VoteSummary>,
    /// A thread page's tags, category and favourite mark.
    meta: Option<ThreadMeta>,
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
    let vote = view.votes.iter().find(|v| v.hash == message.hash);
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
        score: vote.map_or(0, |v| v.score),
        own_vote: vote.map_or(0, |v| v.own),
        vote: view
            .viewer
            .csrf
            .as_ref()
            .map(|csrf| listmngr_web::VoteForm {
                action: format!("/web/lists/{}/archive/vote", id.as_str()),
                csrf: csrf.clone(),
            }),
        reply_href: view.viewer.csrf.as_ref().map(|_| {
            format!(
                "/web/lists/{}/archive/post?reply={}",
                id.as_str(),
                message.hash
            )
        }),
    })
}

/// A thread page's tags, category and favourite mark as the reader may
/// act on them: the tagger or an owner removes a tag, an owner files the
/// thread, any signed-in reader keeps it.
fn meta_view(view: &View<'_>, meta: &ThreadMeta) -> listmngr_web::ThreadMetaView {
    let (id, language) = (view.id, view.viewer.language);
    let base = format!("/web/lists/{}/archive", id.as_str());
    listmngr_web::ThreadMetaView {
        thread: view.query.thread.clone(),
        csrf: view.viewer.csrf.clone(),
        tag_action: format!("{base}/tags"),
        category_action: format!("{base}/category"),
        favorite_action: format!("{base}/favorite"),
        tags: meta
            .tags
            .iter()
            .map(|tag| TagLink {
                removable: view.viewer.owner || view.reader == Some(tag.user_id),
                remove_label: listmngr_i18n::message(
                    language,
                    "web-archive-remove-tag",
                    &[("tag", &tag.tag)],
                ),
                ..label_link(id, "tags", &tag.tag)
            })
            .collect(),
        category: meta
            .category
            .as_deref()
            .map(|name| label_link(id, "categories", name)),
        categories: if view.viewer.owner {
            meta.categories
                .iter()
                .map(|name| listmngr_web::CategoryOption {
                    selected: meta.category.as_deref() == Some(name.as_str()),
                    name: name.clone(),
                })
                .collect()
        } else {
            Vec::new()
        },
        favorite: meta.favorite,
    }
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
        "reattached" => Some("web-archive-reattached"),
        "cycle" => Some("web-archive-reattach-refused"),
        "voted" => Some("web-archive-voted"),
        "tagged" => Some("web-archive-tagged"),
        "untagged" => Some("web-archive-untagged"),
        "tag-refused" => Some("web-archive-tag-refused"),
        "categorized" => Some("web-archive-categorized"),
        "favorited" => Some("web-archive-favorited"),
        "unfavorited" => Some("web-archive-unfavorited"),
        "posted" => Some("web-archive-posted"),
        _ => None,
    }
    .map(|id| listmngr_i18n::message(language, id, &[]));
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
        links: links(id, view.viewer.csrf.is_some()),
        results: total.map(|total| {
            listmngr_i18n::message(
                language,
                "web-archive-results",
                &[("count", &total.to_string()), ("query", &query.q)],
            )
        }),
        meta: view.meta.as_ref().map(|meta| meta_view(view, meta)),
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

pub(super) fn message_link(id: &ListId, hash: &str) -> ApiResult<String> {
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
        category: summary
            .category
            .as_deref()
            .map(|name| label_link(id, "categories", name)),
        tags: summary
            .tags
            .iter()
            .map(|tag| label_link(id, "tags", tag))
            .collect(),
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
        links: links(&id, session.as_ref().is_some_and(|s| s.user_id.is_some())),
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
    threads_page(&s, &id, &headers, query.page, Listing::Latest).await
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
    threads_page(&s, &id, &headers, query.page, Listing::Month(year, month)).await
}

/// `GET /web/lists/{id}/archive/favorites`: the signed-in reader's
/// favourite threads.
pub(super) async fn favorites(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    Query(query): Query<PageQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    threads_page(&s, &id, &headers, query.page, Listing::Favorites).await
}

/// `GET /web/lists/{id}/archive/tags/{tag}`: the threads carrying a tag.
pub(super) async fn tagged(
    State(s): State<AppState>,
    Path((id, tag)): Path<(ListId, String)>,
    Query(query): Query<PageQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let tag = listmngr_db::archive::interact::normalize_tag(&tag)?;
    threads_page(&s, &id, &headers, query.page, Listing::Tagged(tag)).await
}

/// `GET /web/lists/{id}/archive/categories/{name}`: the threads filed
/// under one of the list's categories.
pub(super) async fn in_category(
    State(s): State<AppState>,
    Path((id, name)): Path<(ListId, String)>,
    Query(query): Query<PageQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    if name.is_empty() || name.len() > 60 {
        return Err(Error::Validation("archive category".into()).into());
    }
    threads_page(&s, &id, &headers, query.page, Listing::InCategory(name)).await
}

/// Which thread list a page shows.
enum Listing {
    Latest,
    Month(i32, u32),
    Favorites,
    Tagged(String),
    InCategory(String),
}

async fn threads_page(
    s: &AppState,
    id: &ListId,
    headers: &HeaderMap,
    page: u32,
    listing: Listing,
) -> ApiResult<Response> {
    if !(1..=5001).contains(&page) {
        return Err(Error::Validation("archive query bounds".into()).into());
    }
    let session = match listing {
        Listing::Favorites => Some(super::load(s, headers).await?),
        _ => archive_session(s, headers).await?,
    };
    let language = language_for(s, headers, session.as_ref()).await?;
    let base = format!("/web/lists/{}/archive", id.as_str());
    let (selection, heading, base) = match listing {
        Listing::Latest => (
            ThreadSelection::Latest,
            listmngr_i18n::message(language, "web-archive-latest-threads", &[]),
            format!("{base}/threads"),
        ),
        Listing::Month(year, month) => {
            let (from_ms, until_ms) = month_bounds(year, month)?;
            (
                ThreadSelection::Between { from_ms, until_ms },
                listmngr_i18n::message(
                    language,
                    "web-archive-month-threads",
                    &[("month", &format!("{year:04}-{month:02}"))],
                ),
                format!("{base}/threads/{year:04}/{month:02}"),
            )
        }
        Listing::Favorites => {
            let user = session
                .as_ref()
                .and_then(|session| session.user_id)
                .ok_or(Error::Authentication)?;
            (
                ThreadSelection::Favorites(user),
                listmngr_i18n::message(language, "web-archive-favorites-title", &[]),
                format!("{base}/favorites"),
            )
        }
        Listing::Tagged(tag) => (
            ThreadSelection::Tagged(tag.clone()),
            listmngr_i18n::message(language, "web-archive-tag-threads", &[("tag", &tag)]),
            format!("{base}/tags/{tag}"),
        ),
        Listing::InCategory(name) => {
            if !s.db.archive().categories(id).await?.contains(&name) {
                return Err(Error::NotFound("archive category".into()).into());
            }
            (
                ThreadSelection::InCategory(name.clone()),
                listmngr_i18n::message(
                    language,
                    "web-archive-category-threads",
                    &[("category", &name)],
                ),
                format!("{base}/categories/{name}"),
            )
        }
    };
    let rows =
        s.db.archive()
            .browser_threads(id, session.as_ref(), selection, i64::from(page - 1) * 20)
            .await?;
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
        links: links(id, session.as_ref().is_some_and(|s| s.user_id.is_some())),
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
    let reader = session.as_ref().and_then(|session| session.user_id);
    let (votes, meta) = interactions(&s, &id, &default_query, reader, false, &messages).await?;
    let view = View {
        s: &s,
        id: &id,
        query: &default_query,
        viewer: &viewer,
        tree: false,
        terms: Vec::new(),
        reader,
        votes,
        meta,
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
        links: links(&id, viewer.csrf.is_some()),
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
