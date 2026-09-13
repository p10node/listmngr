//! Per-recipient touches applied to a cooked subscriber copy before signing:
//! Mailman's personalization.
//!
//! The `$user_*` decoration placeholders, the `To:` rewrite of
//! `personalize = full`, and the RFC 8058 one-click unsubscribe pair, which
//! Mailman only emits for personalized deliveries because the HTTPS URI must
//! name the recipient.
use crate::templates::Placeholders;
use crate::{Result, cook};
use listmngr_core::MailingList;

/// What is known about one recipient of a personalized delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipient {
    /// The address the copy is delivered to.
    pub email: String,
    /// The address as the member wrote it (`user_delivered_to`).
    pub delivered_to: String,
    pub display_name: String,
    /// The negotiated language tag.
    pub language: String,
}

/// Mailman's `decorate` extras for a personalized copy: `$user_email`,
/// `$user_address`, `$user_delivered_to`, `$user_name`, `$user_language`
/// and `$member`, on top of the list placeholders.
#[must_use]
pub fn placeholders(list: &MailingList, recipient: &Recipient) -> Placeholders {
    crate::templates::list_placeholders(list)
        .set("user_email", recipient.email.clone())
        .set("user_address", recipient.email.clone())
        .set("user_delivered_to", recipient.delivered_to.clone())
        .set("user_name", recipient.display_name.clone())
        .set("user_language", recipient.language.clone())
        .set(
            "member",
            cook::format_mailbox(Some(&recipient.display_name), &recipient.email),
        )
}

/// `personalize = full`: the recipient becomes the only `To:`.
/// # Errors
/// Returns an error for a message with no header/body boundary or an
/// address that cannot be a header value.
pub fn rewrite_to(cooked: &[u8], recipient: &Recipient) -> Result<Vec<u8>> {
    let stripped = cook::strip_named_headers(cooked, &["to"])?;
    cook::append_headers(
        &stripped,
        &[(
            "To".to_owned(),
            cook::format_mailbox(Some(&recipient.display_name), &recipient.email),
        )],
    )
}

/// Replace `List-Unsubscribe` with the recipient's HTTPS URI followed by the
/// mailto, and add `List-Unsubscribe-Post: List-Unsubscribe=One-Click`.
/// A list that suppresses RFC 2369 headers gets nothing.
/// # Errors
/// Returns an error for a message with no header/body boundary or an unsafe URL.
pub fn one_click_unsubscribe(cooked: &[u8], list: &MailingList, url: &str) -> Result<Vec<u8>> {
    if !list.alter_messages.include_rfc2369_headers {
        return Ok(cooked.to_vec());
    }
    let stripped =
        cook::strip_named_headers(cooked, &["list-unsubscribe", "list-unsubscribe-post"])?;
    cook::append_headers(
        &stripped,
        &[
            (
                "List-Unsubscribe".to_owned(),
                format!("<{url}>, <mailto:{}>", list.id.leave_address()),
            ),
            (
                "List-Unsubscribe-Post".to_owned(),
                "List-Unsubscribe=One-Click".to_owned(),
            ),
        ],
    )
}
