use crate::{Error, Result};
use mail_builder::encoders::Base64Encoder;

/// Locate the header/body boundary: the byte range of the first blank line.
///
/// Returns `(blank_line_start, body_start)`. The header block is
/// `raw[..blank_line_start]`; the body (untouched) is `raw[body_start..]`.
#[must_use]
pub fn header_body_split(raw: &[u8]) -> Option<(usize, usize)> {
    let mut cursor = 0usize;
    for line in raw.split_inclusive(|b| *b == b'\n') {
        let end = cursor + line.len();
        let stripped = line.strip_suffix(b"\n")?;
        let stripped = stripped.strip_suffix(b"\r").unwrap_or(stripped);
        if stripped.is_empty() {
            return Some((cursor, end));
        }
        cursor = end;
    }
    None
}

/// A header *value* (or subject prefix) may contain most bytes but never a
/// bare CR or LF: either would splice a new header line, end the header
/// block early, or otherwise desynchronize the message a caller did not write.
fn is_safe_value(value: &str) -> bool {
    !value.bytes().any(|b| b == b'\r' || b == b'\n')
}

/// A header *name* must additionally exclude `:` and whitespace/control bytes
/// (RFC 5322 `field-name` is `1*ftext`, printable US-ASCII excluding `:`).
fn is_safe_header_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| (33..=126).contains(&b) && b != b':')
}

pub fn newline_style(header_block: &[u8]) -> &'static [u8] {
    if header_block.contains(&b'\r') {
        b"\r\n"
    } else {
        b"\n"
    }
}

// Only the Unicode-prefix path decodes/re-encodes Subject. Other fields and
// the MIME body are never rendered through the MIME builder.
fn rewrite_unicode_subject(header_block: &[u8], prefix: &str, newline: &[u8]) -> Result<Vec<u8>> {
    let mut output = Vec::with_capacity(header_block.len());
    let mut lines = header_block.split_inclusive(|b| *b == b'\n').peekable();
    while let Some(line) = lines.next() {
        if !line
            .get(..8)
            .is_some_and(|h| h.eq_ignore_ascii_case(b"subject:"))
        {
            output.extend_from_slice(line);
            continue;
        }
        let mut field = line.to_vec();
        while lines
            .peek()
            .is_some_and(|l| l.starts_with(b" ") || l.starts_with(b"\t"))
        {
            field.extend_from_slice(lines.next().unwrap());
        }
        let parsed = mail_parser::MessageParser::default()
            .parse_headers(&field)
            .ok_or(Error::UnsafeHeaderContent)?;
        let existing = parsed.subject().unwrap_or("");
        let subject = if existing.starts_with(prefix) {
            existing.to_owned()
        } else {
            format!("{prefix}{existing}")
        };
        let mut generated = b"Subject: ".to_vec();
        // Text::write_header's Q encoding can exceed RFC2047's 75-byte word
        // limit at multibyte boundaries. Use the pinned builder's base64 codec
        // with UTF-8-aligned chunks: 42 bytes -> at most 68 bytes per word,
        // 77 including "Subject: ", 69 on continuation lines (excluding CRLF).
        let mut rest = subject.as_str();
        while !rest.is_empty() {
            let mut end = rest.len().min(42);
            while !rest.is_char_boundary(end) {
                end -= 1;
            }
            generated.extend_from_slice(b"=?utf-8?B?");
            Base64Encoder::new().encode_to_writer(&rest.as_bytes()[..end], &mut generated)?;
            generated.extend_from_slice(b"?=\r\n");
            rest = &rest[end..];
            if !rest.is_empty() {
                generated.push(b'\t');
            }
        }
        if newline == b"\n" {
            output.extend(generated.into_iter().filter(|b| *b != b'\r'));
        } else {
            output.extend_from_slice(&generated);
        }
    }
    Ok(output)
}

/// Prepend `prefix` to an existing, single-line `Subject:` header value, unless
/// it is already present. Folded or absent Subject headers are left untouched.
fn rewrite_subject(header_block: &[u8], prefix: &str, newline: &[u8]) -> Vec<u8> {
    if prefix.is_empty() {
        return header_block.to_vec();
    }
    let mut output = Vec::with_capacity(header_block.len() + prefix.len());
    let mut cursor = 0usize;
    while cursor < header_block.len() {
        let rest = &header_block[cursor..];
        let line_len = rest
            .iter()
            .position(|b| *b == b'\n')
            .map_or(rest.len(), |i| i + 1);
        let line = &rest[..line_len];
        let body = line
            .strip_suffix(b"\r\n")
            .or_else(|| line.strip_suffix(b"\n"))
            .unwrap_or(line);
        let is_subject = body
            .get(..8)
            .is_some_and(|h| h.eq_ignore_ascii_case(b"subject:"));
        if is_subject {
            let value_bytes = &body[8..];
            let trimmed_start = value_bytes
                .iter()
                .position(|b| !b.is_ascii_whitespace())
                .unwrap_or(value_bytes.len());
            let existing = &value_bytes[trimmed_start..];
            if !existing.starts_with(prefix.as_bytes()) {
                output.extend_from_slice(b"Subject: ");
                output.extend_from_slice(prefix.as_bytes());
                output.extend_from_slice(existing);
                output.extend_from_slice(newline);
                cursor += line_len;
                continue;
            }
        }
        output.extend_from_slice(line);
        cursor += line_len;
    }
    output
}

/// Signature fields that redistribution invalidates: header removal/addition
/// and subject rewriting break the original DKIM/ARC signatures, so they are
/// dropped rather than left to fail verification downstream.
fn is_signature_header(name: &str) -> bool {
    matches!(
        name,
        "dkim-signature"
            | "domainkey-signature"
            | "arc-seal"
            | "arc-message-signature"
            | "arc-authentication-results"
    )
}

/// List controls, private recipients and moderator-only fields that must not
/// travel to subscribers.
fn is_control_header(name: &str) -> bool {
    name.starts_with("list-")
        || matches!(
            name,
            "x-beenthere"
                | "bcc"
                | "resent-bcc"
                | "approved"
                | "approve"
                | "x-approved"
                | "x-approve"
                | "x-list-received-date"
                | "x-mailman-approved-at"
                | "precedence"
                | "return-path"
                | "x-approval"
                | "x-confirm"
                | "x-list-administrivia"
                | "x-mailman-version"
        )
}

/// Remove whole fields (including every folded continuation) whose lowercased
/// name satisfies `drop`, never touching MIME bytes.
pub fn strip_fields(header_block: &[u8], drop: impl Fn(&str) -> bool) -> Vec<u8> {
    let mut output = Vec::with_capacity(header_block.len());
    let mut keep = false;
    for line in header_block.split_inclusive(|b| *b == b'\n') {
        if !line.starts_with(b" ") && !line.starts_with(b"\t") {
            let name = line.split(|b| *b == b':').next().unwrap_or_default();
            let name = String::from_utf8_lossy(name).to_ascii_lowercase();
            keep = !drop(&name);
        }
        if keep {
            output.extend_from_slice(line);
        }
    }
    output
}

/// Remove whole fields, including every folded continuation, never MIME bytes.
/// Redistribution replaces list controls and conservatively drops old DKIM/ARC
/// signatures: header removal/addition and subject rewriting can invalidate them.
/// Callers must inspect loop markers before cooking and carry validated history
/// forward explicitly in additions.
fn redistribution_headers(header_block: &[u8]) -> Vec<u8> {
    strip_fields(header_block, |name| {
        is_control_header(name) || is_signature_header(name)
    })
}

/// Pipeline primitive: drop list controls, private recipients and
/// moderator-only fields (the `cleanse` handler).
/// # Errors
/// Returns an error if `raw` has no header/body boundary.
pub fn strip_control_headers(raw: &[u8]) -> Result<Vec<u8>> {
    let (blank_start, _) = header_body_split(raw).ok_or(Error::InvalidMessageId)?;
    let mut output = strip_fields(&raw[..blank_start], is_control_header);
    output.extend_from_slice(&raw[blank_start..]);
    Ok(output)
}

/// Pipeline primitive: drop the original DKIM/ARC signatures (the
/// `cleanse-dkim` handler).
/// # Errors
/// Returns an error if `raw` has no header/body boundary.
pub fn strip_signature_headers(raw: &[u8]) -> Result<Vec<u8>> {
    let (blank_start, _) = header_body_split(raw).ok_or(Error::InvalidMessageId)?;
    let mut output = strip_fields(&raw[..blank_start], is_signature_header);
    output.extend_from_slice(&raw[blank_start..]);
    Ok(output)
}

/// Pipeline primitive: prepend `prefix` to `Subject:` once (the
/// `subject-prefix` handler). A missing subject stays missing.
/// # Errors
/// Returns an error for a prefix carrying CR/LF or a message with no boundary.
pub fn prefix_subject(raw: &[u8], prefix: &str) -> Result<Vec<u8>> {
    if !is_safe_value(prefix) {
        return Err(Error::UnsafeHeaderContent);
    }
    let (blank_start, body_start) = header_body_split(raw).ok_or(Error::InvalidMessageId)?;
    if prefix.is_empty() {
        return Ok(raw.to_vec());
    }
    let newline = newline_style(&raw[..body_start]);
    let header_block = if prefix.is_ascii() {
        rewrite_subject(&raw[..blank_start], prefix, newline)
    } else {
        rewrite_unicode_subject(&raw[..blank_start], prefix, newline)?
    };
    let mut output = Vec::with_capacity(raw.len() + prefix.len());
    output.extend_from_slice(&header_block);
    output.extend_from_slice(&raw[blank_start..]);
    Ok(output)
}

/// Pipeline primitive: splice `additions` before the blank line without
/// touching existing fields or the body.
/// # Errors
/// Returns an error for an unsafe name/value or a message with no boundary.
pub fn append_headers(raw: &[u8], additions: &[(String, String)]) -> Result<Vec<u8>> {
    if additions
        .iter()
        .any(|(name, value)| !is_safe_header_name(name) || !is_safe_value(value))
    {
        return Err(Error::UnsafeHeaderContent);
    }
    let (blank_start, body_start) = header_body_split(raw).ok_or(Error::InvalidMessageId)?;
    let newline = newline_style(&raw[..body_start]);
    let mut output = Vec::with_capacity(raw.len() + additions.len() * 64);
    output.extend_from_slice(&raw[..blank_start]);
    for (name, value) in additions {
        output.extend_from_slice(name.as_bytes());
        output.extend_from_slice(b": ");
        output.extend_from_slice(value.as_bytes());
        output.extend_from_slice(newline);
    }
    output.extend_from_slice(&raw[blank_start..]);
    Ok(output)
}

/// Sanitize redistribution headers, then splice `additions` before the blank line.
///
/// If present and not already prefixed, also prepends `subject_prefix` to
/// `Subject:`. Non-ASCII prefixes use decoded prefix matching and RFC2047
/// encoding of the complete (possibly folded) subject; ASCII prefixes retain
/// the legacy byte-splicing behavior. Missing subjects stay missing. The
/// message body is copied byte-for-byte.
///
/// # Errors
/// Returns an error if `raw` has no header/body boundary (already rejected at
/// intake by [`crate::parse_message_id`], so this indicates caller misuse), or
/// if `subject_prefix` or any addition name/value could inject a header or
/// end the header block early (a bare CR, LF, or — for names — `:`).
pub fn cook_headers(
    raw: &[u8],
    subject_prefix: Option<&str>,
    additions: &[(String, String)],
) -> Result<Vec<u8>> {
    if subject_prefix.is_some_and(|prefix| !is_safe_value(prefix)) {
        return Err(Error::UnsafeHeaderContent);
    }
    if additions
        .iter()
        .any(|(name, value)| !is_safe_header_name(name) || !is_safe_value(value))
    {
        return Err(Error::UnsafeHeaderContent);
    }
    let (blank_start, body_start) = header_body_split(raw).ok_or(Error::InvalidMessageId)?;
    let header_block = &raw[..blank_start];
    let newline = newline_style(&raw[..body_start]);
    let filtered = redistribution_headers(header_block);
    let header_block = filtered.as_slice();
    let header_block = match subject_prefix.filter(|prefix| !prefix.is_empty()) {
        Some(prefix) if !prefix.is_ascii() => {
            rewrite_unicode_subject(header_block, prefix, newline)?
        }
        Some(prefix) => rewrite_subject(header_block, prefix, newline),
        None => header_block.to_vec(),
    };
    let mut output = Vec::with_capacity(raw.len() + additions.len() * 64);
    output.extend_from_slice(&header_block);
    for (name, value) in additions {
        output.extend_from_slice(name.as_bytes());
        output.extend_from_slice(b": ");
        output.extend_from_slice(value.as_bytes());
        output.extend_from_slice(newline);
    }
    output.extend_from_slice(&raw[blank_start..body_start]);
    output.extend_from_slice(&raw[body_start..]);
    Ok(output)
}

/// Retain only presentation/MIME structure for anonymous redistribution.
/// Free-form subject and MIME/body content are not anonymized: this is header
/// identity suppression, not a guarantee against authors identifying themselves.
pub fn anonymous_message(raw: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(raw.len());
    let mut keep = false;
    let mut in_body = false;
    for line in raw.split_inclusive(|b| *b == b'\n') {
        if line == b"\r\n" || line == b"\n" {
            in_body = true;
        }
        if !in_body && !line.starts_with(b" ") && !line.starts_with(b"\t") {
            let name = line.split(|b| *b == b':').next().unwrap_or_default();
            keep = [
                b"subject".as_slice(),
                b"date",
                b"mime-version",
                b"content-type",
                b"content-transfer-encoding",
                b"content-disposition",
                b"content-language",
            ]
            .iter()
            .any(|allowed| name.eq_ignore_ascii_case(allowed));
        }
        if in_body || keep {
            output.extend_from_slice(line);
        }
    }
    output
}

/// Cook a post for publication (archive copy): everything the pipeline does
/// before `to-archive`, so no delivery-only DMARC rewriting.
/// # Errors
/// Returns invalid header errors or a pipeline refusal.
pub fn cook_post(raw: &[u8], list: &listmngr_core::MailingList, identity: &str) -> Result<Vec<u8>> {
    crate::handlers::cook_for(crate::handlers::Target::Archive, raw, list, identity)
}

/// Cook a post for individual delivery: everything before `to-outgoing`,
/// including delivery-only DMARC From mitigation.
/// # Errors
/// Rejects unsupported mitigation settings and unsafe headers.
pub fn cook_individual_post(
    raw: &[u8],
    list: &listmngr_core::MailingList,
    identity: &str,
) -> Result<Vec<u8>> {
    crate::handlers::cook_for(crate::handlers::Target::Out, raw, list, identity)
}
