use crate::{Error, Result};

/// Locate the header/body boundary: the byte range of the first blank line.
/// Returns `(blank_line_start, body_start)`. The header block is
/// `raw[..blank_line_start]`; the body (untouched) is `raw[body_start..]`.
fn header_body_split(raw: &[u8]) -> Option<(usize, usize)> {
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

fn newline_style(header_block: &[u8]) -> &'static [u8] {
    if header_block.contains(&b'\r') {
        b"\r\n"
    } else {
        b"\n"
    }
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

/// Splice `additions` header lines just before the terminating blank line.
///
/// If present and not already prefixed, also prepends `subject_prefix` to a
/// single-line `Subject:` header. The message body is copied byte-for-byte.
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
    let newline = newline_style(header_block);
    let header_block = subject_prefix
        .filter(|prefix| !prefix.is_empty())
        .map_or_else(
            || header_block.to_vec(),
            |prefix| rewrite_subject(header_block, prefix, newline),
        );
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
