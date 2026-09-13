//! Mailman's `rfc-2369` handler: the `List-*` header set a post carries.
//!
//! With the list's switches (`include_rfc2369_headers`, `allow_list_posts`),
//! the archive URLs when the site's base URL is known, and `List-Owner`,
//! which RFC 2369 defines and Mailman leaves out.
use crate::cook;
use listmngr_core::{ArchivePolicy, MailingList};

/// `{base}/archives/list/{list_id}/` — `HyperKitty`'s list URL shape, so
/// `List-Archive` links survive a migration.
#[must_use]
pub fn archive_list_url(base_url: &str, list: &MailingList) -> String {
    format!(
        "{}/archives/list/{}/",
        base_url.trim_end_matches('/'),
        list.id
    )
}

/// `{base}/archives/list/{list_id}/message/{hash}/` — `HyperKitty`'s permalink.
#[must_use]
pub fn archive_message_url(base_url: &str, list: &MailingList, hash: &str) -> String {
    format!("{}message/{hash}/", archive_list_url(base_url, list))
}

/// Mailman's `List-Id`: `"description" <list_id>` when the list has a
/// description (RFC 2047 encoded when non-ASCII), else `<list_id>`.
#[must_use]
pub fn list_id_header(list: &MailingList) -> String {
    let description = list.description.trim();
    if description.is_empty() || description.contains(['\r', '\n']) {
        format!("<{}>", list.id)
    } else {
        format!("{} <{}>", cook::phrase(description), list.id)
    }
}

/// The RFC 2369 headers for a post with `message_id` (used for
/// `Archived-At`), in Mailman's order. Empty when the list switches them off.
#[must_use]
pub fn rfc2369(
    list: &MailingList,
    base_url: Option<&str>,
    message_id: Option<&str>,
) -> Vec<(String, String)> {
    if !list.alter_messages.include_rfc2369_headers {
        return Vec::new();
    }
    let id = &list.id;
    let mut headers = vec![
        ("List-Id".to_owned(), list_id_header(list)),
        (
            "List-Help".to_owned(),
            format!("<mailto:{}?subject=help>", id.request_address()),
        ),
        (
            "List-Unsubscribe".to_owned(),
            format!("<mailto:{}>", id.leave_address()),
        ),
        (
            "List-Subscribe".to_owned(),
            format!("<mailto:{}>", id.join_address()),
        ),
        (
            "List-Post".to_owned(),
            if list.alter_messages.allow_list_posts {
                format!("<mailto:{}>", id.posting_address())
            } else {
                "NO".to_owned()
            },
        ),
        (
            "List-Owner".to_owned(),
            format!("<mailto:{}>", id.owner_address()),
        ),
    ];
    if list.archive_policy != ArchivePolicy::Never
        && let Some(base) = base_url.map(str::trim).filter(|base| !base.is_empty())
    {
        headers.push((
            "List-Archive".to_owned(),
            format!("<{}>", archive_list_url(base, list)),
        ));
        if let Some(hash) = message_id.and_then(|value| crate::message_id_hash(value).ok()) {
            headers.push((
                "Archived-At".to_owned(),
                format!("<{}>", archive_message_url(base, list, &hash)),
            ));
        }
    }
    headers
}
