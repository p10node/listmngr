//! Bounded unconditional From rewriting. No DNS policy or authentication claim.
use crate::{Error, Result};
use mail_builder::headers::{Header, address::Address};

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

pub fn rewrite(raw: &[u8], posting_address: &str) -> Result<Vec<u8>> {
    let (blank, body) = super::cook::header_body_split(raw).ok_or(Error::UnsafeHeaderContent)?;
    if body > crate::MAX_HEADER_BYTES || !crate::owner::safe_mailbox(posting_address) {
        return Err(Error::UnsafeHeaderContent);
    }
    let mut fields: Vec<(String, String)> = Vec::new();
    let mut output = Vec::with_capacity(raw.len());
    let mut keep = false;
    for line in raw[..blank].split_inclusive(|b| *b == b'\n') {
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
            let name = name.to_ascii_lowercase();
            keep = !matches!(name.as_str(), "from" | "sender" | "reply-to");
            fields.push((name, value.trim().into()));
        }
        if keep {
            output.extend_from_slice(line);
        }
    }
    let from: Vec<_> = fields.iter().filter(|(name, _)| name == "from").collect();
    if from.len() != 1 {
        return Err(Error::UnsafeHeaderContent);
    }
    let (name, email) = mailbox(&from[0].1).ok_or(Error::UnsafeHeaderContent)?;
    // Already list-addressed publication needs no further attribution wrapper.
    // This is alignment, not trust in a user-supplied mitigation marker.
    let attribution = if email.eq_ignore_ascii_case(posting_address) {
        name
    } else if name.is_empty() {
        format!("{email} via {posting_address}")
    } else {
        format!("{name} ({email}) via {posting_address}")
    };
    let reply: Vec<_> = fields
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
    if raw[..body].contains(&b'\r') {
        output.extend_from_slice(&generated);
    } else {
        output.extend(generated.into_iter().filter(|b| *b != b'\r'));
    }
    output.extend_from_slice(&raw[blank..]);
    Ok(output)
}
