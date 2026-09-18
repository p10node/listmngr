//! Readers' interactions with the archive: `POST …/archive/vote`,
//! `…/archive/tags`, `…/archive/category` and `…/archive/favorite`, each
//! a CSRF-checked form from a signed-in session, applied by the archive
//! repository under the list's policy, then a redirect back to the post
//! or the thread with a notice.
use crate::{ApiResult, AppState};
use axum::extract::{Form, Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use listmngr_core::{Error, ListId};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct VoteInput {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    hash: String,
    #[serde(default)]
    value: i32,
}

/// `POST /web/lists/{id}/archive/vote`: 1, -1, or 0 to take a vote back.
pub(super) async fn vote(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    headers: HeaderMap,
    Form(form): Form<VoteInput>,
) -> ApiResult<Response> {
    let session = super::write_session(&s, &headers, &form.csrf).await?;
    s.db.archive()
        .browser_vote(&session, &id, form.hash.trim(), form.value, super::now())
        .await?;
    let back = super::archive::message_link(&id, form.hash.trim())?;
    Ok(Redirect::to(&format!("{back}&saved=voted")).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TagInput {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    thread: String,
    #[serde(default)]
    tag: String,
    #[serde(default)]
    op: String,
}

/// `POST /web/lists/{id}/archive/tags`: add a tag to a thread, or remove
/// one (`op=remove`). A tag that normalises to nothing sends the reader
/// back with a refusal notice.
pub(super) async fn tag(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    headers: HeaderMap,
    Form(form): Form<TagInput>,
) -> ApiResult<Response> {
    let session = super::write_session(&s, &headers, &form.csrf).await?;
    let thread = form.thread.trim();
    if thread.is_empty() || thread.len() > 200 || form.tag.len() > 200 {
        return Err(Error::Validation("archive thread bounds".into()).into());
    }
    let add = form.op != "remove";
    let back = super::archive::thread_link(&id, thread);
    let saved = match s
        .db
        .archive()
        .browser_tag(&session, &id, thread, &form.tag, add, super::now())
        .await
    {
        Ok(_) if add => "tagged",
        Ok(_) => "untagged",
        Err(Error::Validation(_)) => "tag-refused",
        Err(error) => return Err(error.into()),
    };
    Ok(Redirect::to(&format!("{back}?saved={saved}")).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CategoryInput {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    thread: String,
    #[serde(default)]
    category: String,
}

/// `POST /web/lists/{id}/archive/category`: an owner files a thread under
/// one of the list's categories, or under none.
pub(super) async fn category(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    headers: HeaderMap,
    Form(form): Form<CategoryInput>,
) -> ApiResult<Response> {
    let session = super::write_session(&s, &headers, &form.csrf).await?;
    super::privileged(&s, &session).await?;
    let thread = form.thread.trim();
    let category = form.category.trim();
    if category.len() > 60 {
        return Err(Error::Validation("archive category".into()).into());
    }
    s.db.archive()
        .browser_set_category(
            &session,
            &id,
            thread,
            (!category.is_empty()).then_some(category),
        )
        .await?;
    let back = super::archive::thread_link(&id, thread);
    Ok(Redirect::to(&format!("{back}?saved=categorized")).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FavoriteInput {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    thread: String,
    #[serde(default)]
    on: u8,
}

/// `POST /web/lists/{id}/archive/favorite`: keep a thread among the
/// reader's favourites (`on=1`) or drop it (`on=0`).
pub(super) async fn favorite(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    headers: HeaderMap,
    Form(form): Form<FavoriteInput>,
) -> ApiResult<Response> {
    let session = super::write_session(&s, &headers, &form.csrf).await?;
    let thread = form.thread.trim();
    let on = match form.on {
        0 => false,
        1 => true,
        _ => return Err(Error::Validation("archive favourite".into()).into()),
    };
    s.db.archive()
        .browser_favorite(&session, &id, thread, on, super::now())
        .await?;
    let back = super::archive::thread_link(&id, thread);
    let saved = if on { "favorited" } else { "unfavorited" };
    Ok(Redirect::to(&format!("{back}?saved={saved}")).into_response())
}
