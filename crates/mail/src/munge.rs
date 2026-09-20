//! Mailman's `munge_from` and `wrap_message` DMARC mitigations over a
//! post's bytes. Bounded rewriting only: no DNS policy or authentication
//! claim is made here — the `dmarc-mitigation` rule decided.
use crate::{Error, Result};
use mail_builder::headers::{Header, address::Address};
use sha2::{Digest as _, Sha256};

// Deliberately reject obsolete/comments/group/local-literal syntax rather than
// accepting a permissive parser's partial mailbox and leaking the original From.
fn mailbox(value: &str) -> Option<(String, String)> {
    let value = value.trim();
    let email = if let Some((phrase, rest)) = value.split_once('<') {
        let email = rest.strip_suffix('>')?;
        let phrase = phrase.trim();
        if phrase.starts_with('"') {
            let inner = phrase.strip_prefix('"')?.strip_suffix('"')?;
            let mut escaped = false;
            for c in inner.chars() {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    return None;
                }
            }
            if escaped {
                return None;
            }
        } else if phrase.chars().any(|c| "()<>@,;:\\\"".contains(c)) {
            return None;
        }
        email
    } else {
        value
    };
    if !crate::owner::safe_mailbox(email) {
        return None;
    }
    let raw = format!("From: {value}\r\n\r\n");
    let parsed = mail_parser::MessageParser::default().parse(raw.as_bytes())?;
    let addresses = parsed.from()?;
    if addresses.iter().count() != 1 {
        return None;
    }
    let author = addresses.first()?;
    if author.address()? != email {
        return None;
    }
    let name = author.name().unwrap_or("");
    if name.chars().any(char::is_control) {
        return None;
    }
    Some((name.into(), email.into()))
}

fn reply_mailboxes(value: &str) -> Option<Vec<String>> {
    let mut quoted = false;
    let mut escaped = false;
    let mut start = 0;
    let mut values = Vec::new();
    for (i, c) in value.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if quoted && c == '\\' {
            escaped = true;
        } else if c == '"' {
            quoted = !quoted;
        } else if c == ',' && !quoted {
            values.push(mailbox(&value[start..i])?.1);
            start = i + 1;
        }
    }
    if quoted || escaped {
        return None;
    }
    values.push(mailbox(&value[start..])?.1);
    Some(values)
}

/// A post's header block as mitigation reads it: every field (lowercase
/// name, unfolded value) and every raw line with the field it belongs to,
/// so kept fields can be copied as they were folded.
struct Block {
    fields: Vec<(String, String)>,
    /// (byte range of the raw line, index into `fields`).
    lines: Vec<(std::ops::Range<usize>, usize)>,
    /// Where the blank line that ends the header block starts.
    blank: usize,
    /// Whether the header block uses CRLF line endings.
    crlf: bool,
}

fn block(raw: &[u8], posting_address: &str) -> Result<Block> {
    let (blank, body) = super::cook::header_body_split(raw).ok_or(Error::UnsafeHeaderContent)?;
    if body > crate::MAX_HEADER_BYTES || !crate::owner::safe_mailbox(posting_address) {
        return Err(Error::UnsafeHeaderContent);
    }
    let mut fields: Vec<(String, String)> = Vec::new();
    let mut lines = Vec::new();
    let mut cursor = 0;
    for line in raw[..blank].split_inclusive(|b| *b == b'\n') {
        let range = cursor..cursor + line.len();
        cursor = range.end;
        let text = std::str::from_utf8(line.strip_suffix(b"\n").unwrap_or(line))
            .map_err(|_| Error::UnsafeHeaderContent)?;
        let text = text.strip_suffix('\r').unwrap_or(text);
        if text.len() > crate::MAX_HEADER_LINE_BYTES
            || text.chars().any(|c| c.is_control() && c != '\t')
        {
            return Err(Error::UnsafeHeaderContent);
        }
        if text.starts_with([' ', '\t']) {
            let (_, value) = fields.last_mut().ok_or(Error::UnsafeHeaderContent)?;
            value.push(' ');
            value.push_str(text.trim());
        } else {
            let (name, value) = text.split_once(':').ok_or(Error::UnsafeHeaderContent)?;
            if name.is_empty() || !name.bytes().all(|b| (33..=126).contains(&b)) {
                return Err(Error::UnsafeHeaderContent);
            }
            fields.push((name.to_ascii_lowercase(), value.trim().into()));
        }
        lines.push((range, fields.len() - 1));
    }
    Ok(Block {
        fields,
        lines,
        blank,
        crlf: raw[..body].contains(&b'\r'),
    })
}

/// The one author mailbox of the post, or a refusal: a missing, doubled
/// or unsafe `From` fails closed rather than leaking through.
fn author(block: &Block) -> Result<(String, String)> {
    let from: Vec<_> = block
        .fields
        .iter()
        .filter(|(name, _)| name == "from")
        .collect();
    if from.len() != 1 {
        return Err(Error::UnsafeHeaderContent);
    }
    mailbox(&from[0].1).ok_or(Error::UnsafeHeaderContent)
}

/// The list-addressed `From` and the `Reply-To` that keeps the author
/// reachable, as CRLF header lines: `Name (address) via list`, the post's
/// own `Reply-To` when it is well formed, else the author.
fn identity(
    block: &Block,
    posting_address: &str,
    (name, email): (String, String),
) -> Result<Vec<u8>> {
    // Already list-addressed publication needs no further attribution wrapper.
    // This is alignment, not trust in a user-supplied mitigation marker.
    let attribution = if email.eq_ignore_ascii_case(posting_address) {
        name
    } else if name.is_empty() {
        format!("{email} via {posting_address}")
    } else {
        format!("{name} ({email}) via {posting_address}")
    };
    let reply: Vec<_> = block
        .fields
        .iter()
        .filter(|(name, _)| name == "reply-to")
        .collect();
    let replies = if reply.len() == 1 {
        reply_mailboxes(&reply[0].1)
    } else {
        None
    }
    .unwrap_or_else(|| vec![email]);
    // The MIME builder quotes/encodes and folds application-generated identity.
    // Only its headers are rendered: the original MIME body is never rebuilt.
    let mut generated = Vec::new();
    generated.extend_from_slice(b"From: ");
    Address::new_address(Some(attribution), posting_address).write_header(&mut generated, 6)?;
    generated.extend_from_slice(b"Reply-To: ");
    Address::new_list(replies.into_iter().map(Address::from).collect())
        .write_header(&mut generated, 10)?;
    Ok(generated)
}

/// Append generated CRLF lines in the block's own line endings.
fn push_generated(output: &mut Vec<u8>, generated: &[u8], crlf: bool) {
    if crlf {
        output.extend_from_slice(generated);
    } else {
        output.extend(generated.iter().copied().filter(|b| *b != b'\r'));
    }
}

/// Mailman's `munge_from`: `From` becomes the list with the author named,
/// the author stays reachable through `Reply-To`, `Sender` goes; the body
/// is untouched.
/// # Errors
/// Returns `UnsafeHeaderContent` for a header block that cannot be read
/// safely or an author that cannot be named.
pub fn rewrite(raw: &[u8], posting_address: &str) -> Result<Vec<u8>> {
    let block = block(raw, posting_address)?;
    let generated = identity(&block, posting_address, author(&block)?)?;
    let mut output = Vec::with_capacity(raw.len());
    for (range, field) in &block.lines {
        if !matches!(
            block.fields[*field].0.as_str(),
            "from" | "sender" | "reply-to"
        ) {
            output.extend_from_slice(&raw[range.clone()]);
        }
    }
    push_generated(&mut output, &generated, block.crlf);
    output.extend_from_slice(&raw[block.blank..]);
    Ok(output)
}

/// Mailman's wrapper keeps these headers on the outer message
/// (`mailman/handlers/dmarc.py` `KEEPERS`), plus `Cc`, which Mailman adds
/// back beside the author it puts in `Reply-To`, and `X-BeenThere`, the
/// loop history the `loop` rule reads on whatever it is handed.
fn kept_outside(name: &str) -> bool {
    matches!(
        name,
        "archived-at"
            | "cc"
            | "date"
            | "in-reply-to"
            | "precedence"
            | "references"
            | "subject"
            | "to"
            | "x-beenthere"
    ) || name.starts_with("list-")
        || name.starts_with("x-mailman-")
}

/// Mailman's `wrap_message`: the post, whole and unchanged, inside a new
/// list-addressed message that keeps only the headers a reader's client
/// threads and displays by, with `text` (wrapped at seventy columns) above
/// it as an inline `text/plain` part when there is one. A post already
/// from the list is left alone.
/// # Errors
/// As [`rewrite`].
pub fn wrap(raw: &[u8], posting_address: &str, mail_host: &str, text: &str) -> Result<Vec<u8>> {
    let block = block(raw, posting_address)?;
    let author = author(&block)?;
    if author.1.eq_ignore_ascii_case(posting_address) {
        return Ok(raw.to_vec());
    }
    let generated = identity(&block, posting_address, author)?;
    let eol: &[u8] = if block.crlf { b"\r\n" } else { b"\n" };
    let mut output = Vec::with_capacity(raw.len() + text.len() + 512);
    for (range, field) in &block.lines {
        if kept_outside(&block.fields[*field].0) {
            output.extend_from_slice(&raw[range.clone()]);
        }
    }
    let digest = format!("{:x}", Sha256::digest(raw));
    let line = |output: &mut Vec<u8>, text: &str| {
        output.extend_from_slice(text.as_bytes());
        output.extend_from_slice(eol);
    };
    line(&mut output, "MIME-Version: 1.0");
    line(
        &mut output,
        &format!("Message-ID: <wrapped-{}@{mail_host}>", &digest[..32]),
    );
    push_generated(&mut output, &generated, block.crlf);
    let inner_encoding = if raw.is_ascii() { "7bit" } else { "8bit" };
    let text = text.trim_end();
    if text.is_empty() {
        line(&mut output, "Content-Type: message/rfc822");
        line(
            &mut output,
            &format!("Content-Transfer-Encoding: {inner_encoding}"),
        );
        line(&mut output, "Content-Disposition: inline");
        line(&mut output, "");
        output.extend_from_slice(raw);
        return Ok(output);
    }
    let mut boundary = format!("=_wrap-{}", &digest[32..]);
    while raw
        .windows(boundary.len())
        .any(|w| w == boundary.as_bytes())
    {
        boundary.push('=');
    }
    line(
        &mut output,
        &format!("Content-Type: multipart/mixed; boundary=\"{boundary}\""),
    );
    line(&mut output, "");
    line(&mut output, &format!("--{boundary}"));
    line(&mut output, "Content-Type: text/plain; charset=utf-8");
    line(
        &mut output,
        &format!(
            "Content-Transfer-Encoding: {}",
            if text.is_ascii() { "7bit" } else { "8bit" }
        ),
    );
    line(&mut output, "Content-Disposition: inline");
    line(&mut output, "");
    for wrapped in crate::digest::wrap(text, 70).lines() {
        line(&mut output, wrapped);
    }
    line(&mut output, &format!("--{boundary}"));
    line(&mut output, "Content-Type: message/rfc822");
    line(
        &mut output,
        &format!("Content-Transfer-Encoding: {inner_encoding}"),
    );
    line(&mut output, "Content-Disposition: inline");
    line(&mut output, "");
    output.extend_from_slice(raw);
    // The line break before a boundary belongs to the boundary, so the post
    // keeps its own last one.
    output.extend_from_slice(eol);
    line(&mut output, &format!("--{boundary}--"));
    Ok(output)
}
