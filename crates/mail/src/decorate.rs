//! Mailman's `decorate`: the list's header and footer text, added to the
//! copy that goes to subscribers (Mailman decorates at delivery, after the
//! archive and digest copies were taken).
//!
//! The strategy is `mailman/handlers/decorate.py`'s, over the stored bytes:
//! a single `text/plain` body is decoded and concatenated with the text,
//! re-encoded as UTF-8 with its RFC 3676 parameters kept; a
//! `multipart/mixed` gets inline `text/plain` parts spliced first and last
//! without touching the existing parts; anything else is wrapped in a new
//! `multipart/mixed` whose inner part carries the original content headers
//! and body verbatim while the message headers stay outside.
use crate::{Error, Result, cook, encoding};
use mail_parser::{MessageParser, MimeHeaders, PartType};
use sha2::{Digest, Sha256};

const MAX_MESSAGE_BYTES: usize = 32 * 1024 * 1024;

fn is_content_field(name: &str) -> bool {
    name.starts_with("content-")
}

/// Header lines of a block (with or without its blank line) that `keep`
/// selects, without the blank line.
fn fields(block: &[u8], keep: impl Fn(&str) -> bool) -> Vec<u8> {
    let end = cook::header_body_split(block).map_or(block.len(), |(blank, _)| blank);
    cook::strip_fields(&block[..end], |name| !keep(name))
}

/// One generated `text/plain; charset=utf-8` part, delimiter excluded.
fn text_part(text: &str, newline: &[u8]) -> Vec<u8> {
    let (encoding, body) = encoding::text_body(text, newline);
    let mut out = Vec::with_capacity(body.len() + 128);
    out.extend_from_slice(b"Content-Type: text/plain; charset=utf-8");
    out.extend_from_slice(newline);
    out.extend_from_slice(b"Content-Transfer-Encoding: ");
    out.extend_from_slice(encoding.as_bytes());
    out.extend_from_slice(newline);
    out.extend_from_slice(b"Content-Disposition: inline");
    out.extend_from_slice(newline);
    out.extend_from_slice(newline);
    out.extend_from_slice(&body);
    out
}

/// Byte offset of the first line in `body` that starts with `prefix`.
fn line_starting_with(body: &[u8], prefix: &[u8]) -> Option<usize> {
    let mut cursor = 0;
    for line in body.split_inclusive(|b| *b == b'\n') {
        if line.starts_with(prefix) {
            return Some(cursor);
        }
        cursor += line.len();
    }
    None
}

/// Concatenate header, body and footer the way Mailman does for plain text.
fn joined(header: &str, body: &str, footer: &str) -> String {
    let mut text = String::with_capacity(header.len() + body.len() + footer.len() + 2);
    text.push_str(header);
    if !header.is_empty() && !header.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(body);
    if !footer.is_empty() && !body.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(footer);
    text
}

/// `format`/`delsp` of the original `Content-Type`, re-emitted as parameters.
fn rfc_3676_parameters(ct: Option<&mail_parser::ContentType<'_>>) -> String {
    let mut parameters = String::new();
    if let Some(ct) = ct {
        for name in ["format", "delsp"] {
            if let Some(value) = ct.attribute(name)
                && value.bytes().all(|b| b.is_ascii_alphanumeric())
            {
                parameters.push_str("; ");
                parameters.push_str(name);
                parameters.push('=');
                parameters.push_str(value);
            }
        }
    }
    parameters
}

fn concatenate(raw: &[u8], body: &str, header: &str, footer: &str) -> Result<Vec<u8>> {
    let (blank_start, body_start) = cook::header_body_split(raw).ok_or(Error::CorruptMessage)?;
    let newline = cook::newline_style(&raw[..body_start]);
    let message = MessageParser::default()
        .parse(raw)
        .ok_or(Error::CorruptMessage)?;
    let parameters = rfc_3676_parameters(message.parts[0].content_type());
    let text = joined(header, &body.replace("\r\n", "\n"), footer);
    let (encoding, encoded) = encoding::text_body(&text, newline);
    let mut out = fields(&raw[..blank_start], |name| {
        name != "content-type" && name != "content-transfer-encoding"
    });
    out.extend_from_slice(b"Content-Type: text/plain; charset=utf-8");
    out.extend_from_slice(parameters.as_bytes());
    out.extend_from_slice(newline);
    out.extend_from_slice(b"Content-Transfer-Encoding: ");
    out.extend_from_slice(encoding.as_bytes());
    out.extend_from_slice(newline);
    out.extend_from_slice(newline);
    out.extend_from_slice(&encoded);
    Ok(out)
}

fn splice_mixed(raw: &[u8], boundary: &str, header: &str, footer: &str) -> Result<Vec<u8>> {
    let (_, body_start) = cook::header_body_split(raw).ok_or(Error::CorruptMessage)?;
    let newline = cook::newline_style(&raw[..body_start]);
    let body = &raw[body_start..];
    let delimiter = format!("--{boundary}");
    let closing = format!("--{boundary}--");
    let first = line_starting_with(body, delimiter.as_bytes()).ok_or(Error::CorruptMessage)?;
    let last = line_starting_with(body, closing.as_bytes()).ok_or(Error::CorruptMessage)?;
    let mut out = Vec::with_capacity(raw.len() + header.len() + footer.len() + 256);
    out.extend_from_slice(&raw[..body_start]);
    out.extend_from_slice(&body[..first]);
    if !header.is_empty() {
        out.extend_from_slice(delimiter.as_bytes());
        out.extend_from_slice(newline);
        out.extend_from_slice(&text_part(header, newline));
        out.extend_from_slice(newline);
    }
    out.extend_from_slice(&body[first..last]);
    if !footer.is_empty() {
        out.extend_from_slice(delimiter.as_bytes());
        out.extend_from_slice(newline);
        out.extend_from_slice(&text_part(footer, newline));
        out.extend_from_slice(newline);
    }
    out.extend_from_slice(&body[last..]);
    Ok(out)
}

/// A boundary the original bytes cannot contain.
fn fresh_boundary(raw: &[u8]) -> String {
    let digest = Sha256::digest(raw);
    let mut boundary = format!(
        "=_listmngr_{:x}",
        u64::from_be_bytes(digest[..8].try_into().expect("8 bytes"))
    );
    while raw
        .windows(boundary.len())
        .any(|window| window == boundary.as_bytes())
    {
        boundary.push('_');
    }
    boundary
}

fn wrap(raw: &[u8], header: &str, footer: &str) -> Result<Vec<u8>> {
    let (blank_start, body_start) = cook::header_body_split(raw).ok_or(Error::CorruptMessage)?;
    let newline = cook::newline_style(&raw[..body_start]);
    let boundary = fresh_boundary(raw);
    let mut out = fields(&raw[..blank_start], |name| !is_content_field(name));
    out.extend_from_slice(
        format!("Content-Type: multipart/mixed; boundary=\"{boundary}\"").as_bytes(),
    );
    out.extend_from_slice(newline);
    out.extend_from_slice(newline);
    let delimiter = format!("--{boundary}");
    if !header.is_empty() {
        out.extend_from_slice(delimiter.as_bytes());
        out.extend_from_slice(newline);
        out.extend_from_slice(&text_part(header, newline));
        out.extend_from_slice(newline);
    }
    out.extend_from_slice(delimiter.as_bytes());
    out.extend_from_slice(newline);
    let mut inner = fields(&raw[..blank_start], is_content_field);
    if inner.is_empty() {
        // A message without content headers is plain text by default.
        inner.extend_from_slice(b"Content-Type: text/plain");
        inner.extend_from_slice(newline);
    }
    out.extend_from_slice(&inner);
    out.extend_from_slice(newline);
    out.extend_from_slice(&raw[body_start..]);
    if !raw.ends_with(b"\n") {
        out.extend_from_slice(newline);
    }
    if !footer.is_empty() {
        out.extend_from_slice(delimiter.as_bytes());
        out.extend_from_slice(newline);
        out.extend_from_slice(&text_part(footer, newline));
        out.extend_from_slice(newline);
    }
    out.extend_from_slice(delimiter.as_bytes());
    out.extend_from_slice(b"--");
    out.extend_from_slice(newline);
    Ok(out)
}

/// Add `header` and `footer` (already expanded) to the message. Whitespace-only
/// text counts as empty, as Mailman's `decorate()` treats a blank template.
/// # Errors
/// Returns [`Error::CorruptMessage`] when the message does not parse.
pub fn decorate(raw: &[u8], header: &str, footer: &str) -> Result<Vec<u8>> {
    let header = if header.trim().is_empty() { "" } else { header };
    let footer = if footer.trim().is_empty() { "" } else { footer };
    if header.is_empty() && footer.is_empty() {
        return Ok(raw.to_vec());
    }
    if raw.len() > MAX_MESSAGE_BYTES {
        return Err(Error::CorruptMessage);
    }
    let message = MessageParser::default()
        .parse(raw)
        .ok_or(Error::CorruptMessage)?;
    let root = &message.parts[0];
    let content_type = root.content_type();
    let ctype = content_type.map_or(("text", "plain"), |ct| {
        (ct.ctype(), ct.subtype().unwrap_or(""))
    });
    let ctype = (ctype.0.to_ascii_lowercase(), ctype.1.to_ascii_lowercase());
    // Mailman decodes the body with its declared charset and wraps instead
    // when that fails; a charset without a decoder is such a failure.
    let decodable = !root.is_encoding_problem
        && content_type
            .and_then(|ct| ct.attribute("charset"))
            .is_none_or(|charset| {
                charset.eq_ignore_ascii_case("us-ascii")
                    || charset.eq_ignore_ascii_case("utf-8")
                    || mail_parser::decoders::charsets::map::charset_decoder(charset.as_bytes())
                        .is_some()
            });
    match (&root.body, ctype.0.as_str(), ctype.1.as_str()) {
        (PartType::Text(text), "text", "plain") if decodable => {
            let text = text.to_string();
            concatenate(raw, &text, header, footer)
        }
        (PartType::Multipart(_), "multipart", "mixed") => {
            let boundary = content_type
                .and_then(|ct| ct.attribute("boundary"))
                .ok_or(Error::CorruptMessage)?
                .to_owned();
            splice_mixed(raw, &boundary, header, footer)
        }
        _ => wrap(raw, header, footer),
    }
}
