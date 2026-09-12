//! Per-recipient touches applied to a cooked subscriber copy before signing.
//!
//! Today: the RFC 8058 one-click unsubscribe pair. Mailman only emits these
//! for personalized deliveries, because the HTTPS URI must name the
//! recipient.
use crate::{Result, cook};
use listmngr_core::MailingList;

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
