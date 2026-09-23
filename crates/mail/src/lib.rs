#![forbid(unsafe_code)]
//! Mail metadata, immutable raw-message storage, and the LMTP/SMTP transport
//! protocols. Binding sockets, policy, and durable intake are the caller's
//! responsibility; see [`lmtp::LmtpHandler`] and [`smtp`].

use sha1::{Digest, Sha1};
mod metadata;
pub use metadata::{
    MAX_HEADER_BYTES, MAX_HEADER_LINE_BYTES, header_value, parse_message_id,
    parse_optional_message_id,
};
mod store;
pub use store::FsMessageStore;
mod cook;
mod munge;
pub use cook::{cook_headers, cook_individual_post, cook_post, header_body_split};
pub mod arc;
pub mod attachments;
pub mod authenticity;
pub mod bounce;
pub mod commands;
pub mod decorate;
pub mod digest;
pub mod dkim;
pub mod dsn;
pub mod encoding;
pub mod facts;
pub mod handlers;
pub mod html_text;
pub mod list_headers;
pub mod lmtp;
pub mod mime_delete;
pub mod mta;
pub mod nntp;
pub mod owner;
pub mod personalize;
pub mod reply_to;
pub mod smtp;
pub mod templates;
pub mod templates_mailman;
mod templates_vi;
pub mod topics;
pub mod visible_recipients;
pub mod web_post;

/// Mail helper failures. Message contents are never included in diagnostics.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid digest input")]
    InvalidDigest,
    #[error("invalid Message-ID")]
    InvalidMessageId,
    #[error("invalid store key")]
    InvalidStoreKey,
    #[error("stored message integrity check failed")]
    CorruptMessage,
    #[error("unsafe message store path or permissions")]
    UnsafeStorePath,
    #[error("unsafe header name, value, or subject prefix")]
    UnsafeHeaderContent,
    /// A pipeline handler ended processing with a disposition for the post.
    #[error("{handler}: {reason}")]
    Refused {
        handler: &'static str,
        reason: String,
        refusal: listmngr_pipeline::handlers::Refusal,
    },
    #[error("message store I/O failure")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

fn validate_id(id: &str) -> Result<()> {
    let (left, right) = id.split_once('@').ok_or(Error::InvalidMessageId)?;
    let atom = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-/=?^_`{|}~".contains(&b))
    };
    if id.len() > 998 || !left.split('.').all(atom) || !right.split('.').all(atom) {
        return Err(Error::InvalidMessageId);
    }
    Ok(())
}

/// Mailman's uppercase base32 SHA-1 archive identifier (not a security digest).
/// Case is preserved; a surrounding angle-bracket pair is removed.
/// # Errors
/// Returns an error for unsupported Message-ID syntax.
pub fn message_id_hash(value: &str) -> Result<String> {
    let id = value
        .strip_prefix('<')
        .and_then(|v| v.strip_suffix('>'))
        .unwrap_or_else(|| value.trim_matches([' ', '\t']));
    validate_id(id)?;
    let digest = Sha1::digest(id.as_bytes());
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut output = String::with_capacity(32);
    for chunk in digest.chunks_exact(5) {
        let bits = chunk
            .iter()
            .fold(0_u64, |acc, byte| (acc << 8) | u64::from(*byte));
        for shift in (0..8).rev() {
            output.push(char::from(alphabet[((bits >> (shift * 5)) & 31) as usize]));
        }
    }
    Ok(output)
}
