//! The URLs `HyperKitty` and `Postorius` answered, redirected to the pages
//! that answer here, so the links in old mail, bookmarks and search
//! engines keep working after a migration (`docs/PLAN.md` §9).
//!
//! `HyperKitty` names a list by its posting address
//! (`/archives/list/dev@example.com/message/<hash>/`, and the same under
//! `/hyperkitty/`); `Postorius` by its list id
//! (`/postorius/lists/dev.example.com/`). Either spelling is taken. The
//! redirects are permanent and carry no state: a list or a hash that does
//! not exist is answered by the page redirected to.
use crate::AppState;
use axum::extract::Path;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Router, http::StatusCode};
use listmngr_core::ListId;

pub fn routes() -> Router<AppState> {
    let mut router = Router::new()
        .route("/postorius/", get(directory))
        .route("/postorius/lists/", get(directory))
        .route("/postorius/lists/{list}/", get(list_page));
    for prefix in ["/archives", "/hyperkitty"] {
        router = router
            .route(&format!("{prefix}/"), get(directory))
            .route(&format!("{prefix}/list/{{list}}/"), get(archive))
            .route(&format!("{prefix}/list/{{list}}/latest"), get(archive))
            .route(
                &format!("{prefix}/list/{{list}}/message/{{hash}}/"),
                get(message),
            )
            .route(
                &format!("{prefix}/list/{{list}}/thread/{{thread}}/"),
                get(thread),
            )
            .route(
                &format!("{prefix}/list/{{list}}/{{year}}/{{month}}/"),
                get(month),
            );
    }
    router
}

/// The list a `HyperKitty` or `Postorius` URL names: a posting address
/// (`dev@example.com`) or a list id (`dev.example.com`).
fn list_id(raw: &str) -> Option<ListId> {
    let id = raw.replacen('@', ".", 1);
    id.parse().ok()
}

fn redirect(to: &str) -> Response {
    Redirect::permanent(to).into_response()
}

fn not_found() -> Response {
    StatusCode::NOT_FOUND.into_response()
}

async fn directory() -> Response {
    redirect("/web")
}

async fn list_page(Path(list): Path<String>) -> Response {
    list_id(&list).map_or_else(not_found, |id| {
        redirect(&format!("/web/lists/{}", id.as_str()))
    })
}

async fn archive(Path(list): Path<String>) -> Response {
    list_id(&list).map_or_else(not_found, |id| {
        redirect(&format!("/web/lists/{}/archive", id.as_str()))
    })
}

async fn message(Path((list, hash)): Path<(String, String)>) -> Response {
    list_id(&list).map_or_else(not_found, |id| {
        serde_urlencoded::to_string([("message", hash.as_str())]).map_or_else(
            |_| not_found(),
            |query| redirect(&format!("/web/lists/{}/archive?{query}", id.as_str())),
        )
    })
}

async fn thread(Path((list, thread)): Path<(String, String)>) -> Response {
    let plain = !thread.is_empty() && thread.bytes().all(|b| b.is_ascii_alphanumeric());
    list_id(&list)
        .filter(|_| plain)
        .map_or_else(not_found, |id| {
            redirect(&format!(
                "/web/lists/{}/archive/thread/{thread}",
                id.as_str()
            ))
        })
}

async fn month(Path((list, year, month)): Path<(String, String, String)>) -> Response {
    let digits =
        |text: &str, count: usize| text.len() == count && text.bytes().all(|b| b.is_ascii_digit());
    let plain = digits(&year, 4) && (digits(&month, 2) || digits(&month, 1));
    list_id(&list)
        .filter(|_| plain)
        .map_or_else(not_found, |id| {
            redirect(&format!(
                "/web/lists/{}/archive/threads/{year}/{month}",
                id.as_str()
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_posting_address_or_a_list_id_names_the_list() {
        assert_eq!(
            list_id("dev@example.com").unwrap().as_str(),
            "dev.example.com"
        );
        assert_eq!(
            list_id("dev.example.com").unwrap().as_str(),
            "dev.example.com"
        );
        assert!(list_id("nodomain").is_none());
        assert!(list_id("").is_none());
    }
}
