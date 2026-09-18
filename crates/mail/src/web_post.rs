//! A post written on the web, composed as the message the list receives.
//!
//! The member's address goes in `From`, the list's posting address in
//! `To`, a fresh `Message-ID`, the parent's id in `In-Reply-To` and
//! `References` for a reply, and a `User-Agent` naming the web origin.
//! The body is `text/plain; charset=utf-8`, encoded by `mail-builder`.
use mail_builder::MessageBuilder;
use mail_builder::headers::text::Text;

/// What the web form supplies.
#[derive(Debug, Clone)]
pub struct WebPost<'a> {
    /// The poster's display name, possibly empty.
    pub from_name: &'a str,
    /// The poster's verified address.
    pub from_email: &'a str,
    /// The list's posting address.
    pub to: &'a str,
    pub subject: &'a str,
    pub body: &'a str,
    /// The parent's `Message-ID`, angle brackets included, for a reply.
    pub in_reply_to: Option<&'a str>,
    /// The new message's `Message-ID`, angle brackets included.
    pub message_id: &'a str,
    /// The `Date` header, seconds since the epoch.
    pub date_secs: i64,
}

/// The header value the web origin signs its posts with.
pub const USER_AGENT: &str = "listmngr-web";

/// The message, with network line ends.
/// # Errors
/// Writing to memory fails only on an internal builder error.
pub fn compose(post: &WebPost<'_>) -> std::io::Result<Vec<u8>> {
    let name = post.from_name.trim();
    let mut builder = MessageBuilder::new()
        .from(
            if name.is_empty() || name.eq_ignore_ascii_case(post.from_email) {
                mail_builder::headers::address::Address::from(post.from_email)
            } else {
                (name, post.from_email).into()
            },
        )
        .to(post.to)
        .subject(post.subject)
        .message_id(strip_brackets(post.message_id))
        .date(post.date_secs)
        .header("User-Agent", Text::new(USER_AGENT));
    if let Some(parent) = post.in_reply_to {
        builder = builder
            .in_reply_to(strip_brackets(parent))
            .references(strip_brackets(parent));
    }
    builder.text_body(post.body).write_to_vec()
}

/// `mail-builder` writes the brackets itself.
fn strip_brackets(id: &str) -> &str {
    id.trim()
        .strip_prefix('<')
        .and_then(|rest| rest.strip_suffix('>'))
        .unwrap_or_else(|| id.trim())
}
