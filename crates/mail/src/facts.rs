//! Message facts for the posting rules.
//!
//! Extracted once by the `in` runner so the pure pipeline never parses MIME.
//! Also the `Approved:` posting-key helpers shared by the runner (extraction)
//! and the cook stage (stripping).
use crate::metadata::MAX_HEADER_BYTES;
use mail_parser::{Encoding, MessagePart, MimeHeaders, PartType};

/// Header field names that may carry the `Approved:` posting key.
const APPROVED_HEADERS: [&str; 4] = ["approved", "approve", "x-approved", "x-approve"];

/// Every header field, unfolded, in order.
///
/// Names are kept as written and values trimmed. Stops at the blank line.
/// Malformed lines (no colon) are skipped without discarding later fields.
/// Bounded by [`MAX_HEADER_BYTES`].
#[must_use]
pub fn header_fields(raw: &[u8]) -> Vec<(String, String)> {
    let mut fields: Vec<(String, String)> = Vec::new();
    let mut collecting = false;
    for line in raw[..raw.len().min(MAX_HEADER_BYTES)].split_inclusive(|b| *b == b'\n') {
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            break;
        }
        if line.starts_with(b" ") || line.starts_with(b"\t") {
            if collecting && let Some((_, value)) = fields.last_mut() {
                let continuation = String::from_utf8_lossy(line);
                let continuation = continuation.trim_matches([' ', '\t']);
                if !continuation.is_empty() {
                    if !value.is_empty() {
                        value.push(' ');
                    }
                    value.push_str(continuation);
                }
            }
            continue;
        }
        let Some(colon) = line.iter().position(|b| *b == b':') else {
            collecting = false;
            continue;
        };
        collecting = true;
        fields.push((
            String::from_utf8_lossy(&line[..colon]).trim().to_owned(),
            String::from_utf8_lossy(&line[colon + 1..])
                .trim_matches([' ', '\t'])
                .to_owned(),
        ));
    }
    fields
}

/// The first `text/plain` part (or a part with no declared type) that is not
/// an attachment, in MIME order.
fn first_plain_part<'a, 'x>(message: &'a mail_parser::Message<'x>) -> Option<&'a MessagePart<'x>> {
    message.parts.iter().find(|part| {
        matches!(part.body, PartType::Text(_))
            && !part
                .content_disposition()
                .is_some_and(mail_parser::ContentType::is_attachment)
            && (part.content_type().is_none() || part.is_content_type("text", "plain"))
    })
}

/// Non-blank lines of the first text part, decoded, stopping at a `-- `
/// signature separator and capped at `max` lines.
#[must_use]
pub fn body_preview_lines(raw: &[u8], max: usize) -> Vec<String> {
    let Some(message) = mail_parser::MessageParser::default().parse(raw) else {
        return Vec::new();
    };
    let Some(part) = first_plain_part(&message) else {
        return Vec::new();
    };
    let PartType::Text(text) = &part.body else {
        return Vec::new();
    };
    text.lines()
        .take_while(|line| *line != "-- ")
        .filter(|line| !line.trim().is_empty())
        .take(max)
        .map(|line| line.trim_end().to_owned())
        .collect()
}

/// `Approved: key` on one line, any spelling, case-insensitive.
fn approved_line_value(line: &str) -> Option<&str> {
    let (name, value) = line.split_once(':')?;
    let name = name.trim();
    APPROVED_HEADERS
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(name))
        .then(|| value.trim())
        .filter(|value| !value.is_empty())
}

/// The raw byte range of the first line of the first plain part, when that
/// line is an `Approved:` line the cook stage can remove byte-for-byte: the
/// part must carry no content-transfer-encoding, so the raw bytes are the text.
fn strippable_body_line(raw: &[u8]) -> Option<std::ops::Range<usize>> {
    let message = mail_parser::MessageParser::default().parse(raw)?;
    let part = first_plain_part(&message)?;
    if !matches!(part.encoding, Encoding::None) {
        return None;
    }
    let start = part.offset_body as usize;
    let end = part.offset_end as usize;
    let body = raw.get(start..end)?;
    let line_end = body
        .iter()
        .position(|b| *b == b'\n')
        .map_or(body.len(), |i| i + 1);
    let line = std::str::from_utf8(&body[..line_end]).ok()?;
    approved_line_value(line.trim_end_matches(['\r', '\n']))?;
    Some(start..start + line_end)
}

/// The `Approved:` posting key.
///
/// From a header first, else from the very first line of the first plain part
/// when that part is unencoded (so the same line can later be stripped). An
/// encoded body-line key is deliberately ignored.
#[must_use]
pub fn approved_key(raw: &[u8]) -> Option<String> {
    let from_header = header_fields(raw).into_iter().find_map(|(name, value)| {
        APPROVED_HEADERS
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(&name))
            .then_some(value)
            .filter(|value| !value.is_empty())
    });
    if from_header.is_some() {
        return from_header;
    }
    let range = strippable_body_line(raw)?;
    let line = std::str::from_utf8(&raw[range]).ok()?;
    approved_line_value(line.trim_end_matches(['\r', '\n'])).map(str::to_owned)
}

/// Remove a leading `Approved:` line from the first unencoded plain part.
///
/// A posting key must never reach subscribers or the archive. Header spellings
/// are removed separately by [`crate::cook_headers`]. Bytes are otherwise
/// untouched.
#[must_use]
pub fn strip_approved_line(raw: &[u8]) -> Vec<u8> {
    strippable_body_line(raw).map_or_else(
        || raw.to_vec(),
        |range| {
            let mut output = Vec::with_capacity(raw.len() - range.len());
            output.extend_from_slice(&raw[..range.start]);
            output.extend_from_slice(&raw[range.end..]);
            output
        },
    )
}

/// Read every unfolded List-Post and X-BeenThere field from uncooked headers.
/// Never examine the body, and never let an earlier field hide a later marker.
///
/// # Panics
/// Never panics: a continuation line is only appended to a field that was
/// already started, and the guard is checked before the unwrap.
#[must_use]
pub fn loop_markers(raw: &[u8]) -> Vec<String> {
    let mut fields = Vec::<(bool, String)>::new();
    let mut collecting = false;
    for line in raw.split_inclusive(|b| *b == b'\n') {
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            break;
        }
        if line.starts_with(b" ") || line.starts_with(b"\t") {
            if collecting {
                let (_, value) = fields.last_mut().expect("collecting a field");
                value.push(' ');
                value.push_str(String::from_utf8_lossy(line).trim());
            }
        } else {
            collecting = false;
            if let Some(colon) = line.iter().position(|b| *b == b':') {
                let history = line[..colon].eq_ignore_ascii_case(b"x-beenthere");
                collecting = history || line[..colon].eq_ignore_ascii_case(b"list-post");
                if collecting {
                    fields.push((
                        history,
                        String::from_utf8_lossy(&line[colon + 1..]).into_owned(),
                    ));
                }
            }
        }
    }
    fields
        .into_iter()
        .flat_map(|(history, value)| {
            if history {
                return listmngr_core::Address::new(value.trim(), String::new())
                    .ok()
                    .map(|address| address.email)
                    .into_iter()
                    .collect();
            }
            value
                .split('<')
                .skip(1)
                .filter_map(|part| {
                    let uri = part.split_once('>')?.0.trim().to_ascii_lowercase();
                    let address = uri.strip_prefix("mailto:")?.split('?').next()?.trim();
                    let mut decoded = Vec::with_capacity(address.len());
                    let mut bytes = address.bytes();
                    while let Some(byte) = bytes.next() {
                        decoded.push(if byte == b'%' {
                            let high = char::from(bytes.next()?).to_digit(16)?;
                            let low = char::from(bytes.next()?).to_digit(16)?;
                            u8::try_from(high * 16 + low).ok()?
                        } else {
                            byte
                        });
                    }
                    let address = std::str::from_utf8(&decoded).ok()?;
                    // Only mailbox markers, never arbitrary control text, are propagated.
                    listmngr_core::Address::new(address, String::new())
                        .ok()
                        .map(|address| address.email)
                })
                .collect::<Vec<_>>()
        })
        .collect()
}
