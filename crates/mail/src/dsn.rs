//! Read-only DSN inspection. Parsed recipient claims are NEVER delivery authority.
//! No token verification, scoring, acknowledgement or sender authentication occurs.

use mail_parser::{MessageParser, MessagePart, MimeHeaders, PartType};
use std::collections::{BTreeMap, BTreeSet};

/// Maximum raw report size admitted before MIME parsing.
pub const MAX_REPORT_BYTES: usize = 256 * 1024;
/// Maximum delivery-status body size, before unfolding fields.
pub const MAX_STATUS_BYTES: usize = 64 * 1024;
/// Maximum recipient blocks returned by one report.
pub const MAX_RECIPIENTS: usize = 100;

/// Recipient claims from an untrusted message/delivery-status report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipientReport {
    pub final_recipient: String,
    pub action: String,
    pub status: String,
}

/// Read recipient claims from a multipart/report delivery-status message.
///
/// Unsupported or malformed input returns `None`, without reflecting raw data.
/// A parsed report is not authorization to change any member or queue state.
#[must_use]
pub fn parse(raw: &[u8]) -> Option<Vec<RecipientReport>> {
    if raw.len() > MAX_REPORT_BYTES {
        return None;
    }
    let message = MessageParser::default().parse(raw)?;
    let root = message.parts.first()?;
    if !safe_mime(root)
        || !root.is_content_type("multipart", "report")
        || !root
            .content_type()?
            .attribute("report-type")?
            .eq_ignore_ascii_case("delivery-status")
    {
        return None;
    }
    let PartType::Multipart(children) = &root.body else {
        return None;
    };
    if !(2..=3).contains(&children.len()) {
        return None;
    }
    let boundary = root.content_type()?.attribute("boundary")?;
    if boundary.is_empty()
        || boundary.len() > 70
        || !boundary.bytes().all(|b| b.is_ascii_graphic() || b == b' ')
        || boundary.ends_with(' ')
    {
        return None;
    }
    let frames = frames(raw, root.offset_body as usize, boundary)?;
    if frames.len() != children.len() {
        return None;
    }
    for (id, (start, end)) in children.iter().zip(frames) {
        let part = message.parts.get(*id as usize)?;
        if part.offset_header as usize != start || part.offset_end as usize != end {
            return None;
        }
    }
    let human = message.parts.get(*children.first()? as usize)?;
    if !safe_mime(human) || !human.is_content_type("text", "plain") {
        return None;
    }
    if let Some(id) = children.get(2) {
        let original = message.parts.get(*id as usize)?;
        if !safe_mime(original)
            || !(original.is_content_type("message", "rfc822")
                || original.is_content_type("text", "rfc822-headers"))
        {
            return None;
        }
    }
    let part = message.parts.get(*children.get(1)? as usize)?;
    if !safe_mime(part) || !part.is_content_type("message", "delivery-status") {
        return None;
    }
    let body =
        std::str::from_utf8(raw.get(part.offset_body as usize..part.offset_end as usize)?).ok()?;
    if body.len() > MAX_STATUS_BYTES
        || !body
            .bytes()
            .all(|b| b.is_ascii_graphic() || b" \t\r\n".contains(&b))
    {
        return None;
    }
    let mut blocks = body.trim_end_matches("\r\n").split("\r\n\r\n");
    let message_fields = fields(blocks.next()?)?;
    typed_value(message_fields.get("reporting-mta")?, "dns")?;
    let mut recipients = Vec::new();
    for block in blocks {
        if recipients.len() == MAX_RECIPIENTS {
            return None;
        }
        let fields = fields(block)?;
        let recipient = typed_value(fields.get("final-recipient")?, "rfc822")?;
        let action = fields.get("action")?.to_ascii_lowercase();
        if !matches!(
            action.as_str(),
            "failed" | "delayed" | "delivered" | "relayed" | "expanded"
        ) {
            return None;
        }
        let status = fields.get("status")?;
        let parts: Vec<_> = status.split('.').collect();
        if parts.len() != 3
            || !matches!(parts[0], "2" | "4" | "5")
            || !parts[1..]
                .iter()
                .all(|p| (1..=3).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit()))
        {
            return None;
        }
        recipients.push(RecipientReport {
            final_recipient: recipient.into(),
            action,
            status: status.clone(),
        });
    }
    (!recipients.is_empty()).then_some(recipients)
}

// Cross-check best-effort MIME offsets against actual CRLF delimiter lines.
// A boundary-like substring inside data must never hide an invalid report tail.
fn frames(raw: &[u8], body_start: usize, boundary: &str) -> Option<Vec<(usize, usize)>> {
    let opening = format!("--{boundary}");
    let closing = format!("--{boundary}--");
    let mut result = Vec::new();
    let mut start = None;
    let mut offset = body_start;
    for line in raw.get(body_start..)?.split_inclusive(|b| *b == b'\n') {
        let content = line.strip_suffix(b"\r\n").unwrap_or(line);
        let matches = |delimiter: &str| {
            content
                .strip_prefix(delimiter.as_bytes())
                .is_some_and(|suffix| suffix.iter().all(|b| b" \t".contains(b)))
        };
        let is_closing = matches(&closing);
        if matches(&opening) || is_closing {
            if let Some(start) = start {
                let end = offset.checked_sub(2)?;
                if raw.get(end..offset)? != b"\r\n" || end < start || result.len() == 3 {
                    return None;
                }
                result.push((start, end));
            } else if is_closing {
                return None;
            }
            if is_closing {
                return Some(result);
            }
            start = Some(offset + line.len());
        }
        offset += line.len();
    }
    None
}

fn safe_mime(part: &MessagePart<'_>) -> bool {
    let count = |name: &str| {
        part.headers
            .iter()
            .filter(|h| h.name.as_str().eq_ignore_ascii_case(name))
            .count()
    };
    if part.is_encoding_problem
        || count("Content-Type") != 1
        || count("Content-Transfer-Encoding") > 1
        || part
            .content_transfer_encoding()
            .is_some_and(|v| !v.eq_ignore_ascii_case("7bit"))
    {
        return false;
    }
    let Some(content_type) = part.content_type() else {
        return false;
    };
    let mut names = BTreeSet::new();
    content_type
        .attributes
        .iter()
        .flatten()
        .all(|a| names.insert(a.name.to_ascii_lowercase()))
}

fn typed_value<'a>(value: &'a str, kind: &str) -> Option<&'a str> {
    let (actual, address) = value.split_once(';')?;
    let address = address.trim();
    (actual.trim().eq_ignore_ascii_case(kind) && !address.is_empty()).then_some(address)
}

fn fields(block: &str) -> Option<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    let mut previous = String::new();
    for line in block.split("\r\n") {
        if line.len() > 998 || line.contains(['\r', '\n']) {
            return None;
        }
        if line.starts_with([' ', '\t']) {
            let value: &mut String = result.get_mut(&previous)?;
            value.push(' ');
            value.push_str(line.trim());
        } else {
            if result.len() == 32 {
                return None;
            }
            let (key, value) = line.split_once(':')?;
            if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
                return None;
            }
            previous = key.to_ascii_lowercase();
            if result
                .insert(previous.clone(), value.trim().to_owned())
                .is_some()
            {
                return None;
            }
        }
    }
    Some(result)
}
