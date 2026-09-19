//! The owner's archive administration: `GET …/archive/admin` with the
//! list's categories and the hidden posts, and the three CSRF-checked
//! forms behind it — `POST …/archive/categories` (add, rename, remove),
//! `POST …/archive/hide` (a post or a thread, on or off) and
//! `POST …/archive/delete`. Every one needs the owner's live authority,
//! which the archive repository checks inside the writing transaction.
use crate::{ApiResult, AppState};
use axum::extract::{Form, Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use listmngr_core::{Error, ListId};
use listmngr_db::archive::admin::{CategoryChange, Scope};
use listmngr_web::{AdminCategory, ArchiveAdmin, HiddenRow, Nav, Shell};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Notice {
    #[serde(default)]
    saved: String,
}

fn notice(language: &'static str, saved: &str) -> Option<String> {
    match saved {
        "hidden" => Some("web-archive-hidden"),
        "shown" => Some("web-archive-shown"),
        "deleted" => Some("web-archive-deleted"),
        "category-added" => Some("web-archive-category-added"),
        "category-renamed" => Some("web-archive-category-renamed"),
        "category-removed" => Some("web-archive-category-removed"),
        "category-refused" => Some("web-archive-category-refused"),
        _ => None,
    }
    .map(|id| listmngr_i18n::message(language, id, &[]))
}

/// `GET /web/lists/{id}/archive/admin`.
pub(super) async fn page(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    Query(query): Query<Notice>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let session = super::load(&s, &headers).await?;
    super::privileged(&s, &session).await?;
    let language = super::reader_language(&s, &headers, &session).await?;
    let view = s.db.archive().browser_administration(&session, &id).await?;
    let base = format!("/web/lists/{}/archive", id.as_str());
    let page = ArchiveAdmin {
        shell: Shell::titled(
            language,
            listmngr_i18n::message(language, "web-archive-admin-title", &[]),
            Nav::Lists,
        ),
        links: super::archive::links(&id, true),
        categories: view
            .categories
            .iter()
            .map(|row| AdminCategory {
                href: format!("{base}/categories/{}", row.name),
                name: row.name.clone(),
                threads: row.threads,
            })
            .collect(),
        hidden: view
            .hidden
            .iter()
            .map(|post| HiddenRow {
                thread_href: super::archive::thread_link(&id, &post.thread),
                hash: post.hash.clone(),
                subject: post.subject.clone(),
                sender: post.sender.clone(),
            })
            .collect(),
        notice: notice(language, &query.saved),
        csrf: session.csrf.clone(),
        base,
    };
    Ok(super::html(&page))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CategoryInput {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    op: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    to: String,
}

/// `POST /web/lists/{id}/archive/categories`: the list's own categories.
/// A name that cannot be used sends the owner back with a refusal rather
/// than an error page, as the tag form does.
pub(super) async fn categories(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    headers: HeaderMap,
    Form(form): Form<CategoryInput>,
) -> ApiResult<Response> {
    let session = super::write_session(&s, &headers, &form.csrf).await?;
    super::privileged(&s, &session).await?;
    if form.name.len() > 200 || form.to.len() > 200 {
        return Err(Error::Validation("archive category".into()).into());
    }
    let (change, saved) = match form.op.as_str() {
        "add" => (CategoryChange::Add(&form.name), "category-added"),
        "rename" => (
            CategoryChange::Rename {
                from: &form.name,
                to: &form.to,
            },
            "category-renamed",
        ),
        "remove" => (CategoryChange::Remove(&form.name), "category-removed"),
        _ => return Err(Error::Validation("archive category operation".into()).into()),
    };
    let saved = match s
        .db
        .archive()
        .browser_categories(&session, &id, change)
        .await
    {
        Ok(()) => saved,
        Err(Error::Validation(_)) => "category-refused",
        Err(error) => return Err(error.into()),
    };
    let back = format!("/web/lists/{}/archive/admin?saved={saved}", id.as_str());
    Ok(Redirect::to(&back).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HideInput {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    target: String,
    #[serde(default)]
    on: u8,
}

/// `POST /web/lists/{id}/archive/hide`: take a post or a thread off the
/// reading pages (`on=1`) or put it back (`on=0`).
pub(super) async fn hide(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    headers: HeaderMap,
    Form(form): Form<HideInput>,
) -> ApiResult<Response> {
    let session = super::write_session(&s, &headers, &form.csrf).await?;
    super::privileged(&s, &session).await?;
    let scope = Scope::parse(&form.scope)?;
    let hidden = match form.on {
        0 => false,
        1 => true,
        _ => return Err(Error::Validation("archive admin state".into()).into()),
    };
    let target = form.target.trim();
    s.db.archive()
        .browser_hide(&session, &id, scope, target, hidden, super::now())
        .await?;
    // A hidden post is no longer readable, so hiding lands on the page
    // that lists it and can put it back; showing lands on the post again.
    let back = if hidden {
        format!("/web/lists/{}/archive/admin?saved=hidden", id.as_str())
    } else {
        match scope {
            Scope::Message => format!("{}&saved=shown", super::archive::message_link(&id, target)?),
            Scope::Thread => format!("{}?saved=shown", super::archive::thread_link(&id, target)),
        }
    };
    Ok(Redirect::to(&back).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeleteInput {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    target: String,
}

/// `POST /web/lists/{id}/archive/delete`: a post or a thread goes for
/// good. The thread the post was in may be gone or re-rooted afterwards,
/// so the owner lands on the thread list.
pub(super) async fn delete(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    headers: HeaderMap,
    Form(form): Form<DeleteInput>,
) -> ApiResult<Response> {
    let session = super::write_session(&s, &headers, &form.csrf).await?;
    super::privileged(&s, &session).await?;
    let scope = Scope::parse(&form.scope)?;
    s.db.archive()
        .browser_delete(&session, &id, scope, form.target.trim())
        .await?;
    let back = format!("/web/lists/{}/archive/threads?saved=deleted", id.as_str());
    Ok(Redirect::to(&back).into_response())
}
