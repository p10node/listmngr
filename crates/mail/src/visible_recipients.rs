//! Conservative admission around mail-parser's deliberately forgiving address parser.
//! Counting itself uses parsed mailboxes, including groups and duplicate fields.
use mail_parser::{HeaderName, MessageParser};

/// Count all visible mailboxes, preserving duplicates. Malformed input is unknown.
#[must_use]
pub fn count(raw: &[u8]) -> Option<u64> {
    u64::try_from(mailboxes(raw)?.len()).ok()
}

/// Canonical To/Cc mailbox identities from every unfolded field and group.
///
/// Unknown/malformed input must not authorize duplicate suppression. This is a
/// user-controlled heuristic, not author authentication or proof of delivery.
#[must_use]
pub fn mailboxes(raw: &[u8]) -> Option<Vec<String>> {
    let fields = header_count(raw)?;
    let message = MessageParser::default().parse(raw)?;
    if message.headers().len() != fields {
        return None; // Never allow a partially parsed header block to hide To/Cc.
    }
    let mut mailboxes = Vec::new();
    for header in message.headers() {
        if !matches!(header.name, HeaderName::To | HeaderName::Cc) {
            continue;
        }
        let value = raw.get(header.offset_start as usize..header.offset_end as usize)?;
        if !balanced(value) {
            return None;
        }
        if value.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        for mailbox in header.value.as_address()?.iter() {
            let address = mailbox.address()?;
            // A display-name-only or damaged mailbox is not a zero-recipient success.
            let canonical = listmngr_core::Address::new(address, String::new()).ok()?;
            if address.contains(['<', '>', ',', ';']) {
                return None;
            }
            mailboxes.push(canonical.email);
        }
    }
    Some(mailboxes)
}

fn header_count(raw: &[u8]) -> Option<usize> {
    let mut count = 0;
    for line in raw.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            return Some(count);
        }
        if line
            .iter()
            .any(|byte| byte.is_ascii_control() && *byte != b'\t')
        {
            return None;
        }
        if line.starts_with(b" ") || line.starts_with(b"\t") {
            if count == 0 {
                return None;
            }
        } else {
            let colon = line.iter().position(|byte| *byte == b':')?;
            if colon == 0 || !line[..colon].iter().all(|byte| (33..=126).contains(byte)) {
                return None;
            }
            count += 1;
        }
    }
    None
}

// This is a delimiter guard, not an alternative address parser. Unclosed comments,
// quotes, groups and angle addresses can otherwise swallow later mailboxes.
fn balanced(value: &[u8]) -> bool {
    let mut stack = Vec::new();
    let mut escaped = false;
    let mut group = false;
    for &byte in value {
        if escaped {
            escaped = false;
            continue;
        }
        let top = stack.last().copied();
        if byte == b'\\' && matches!(top, Some(b'"' | b'(' | b'[')) {
            escaped = true;
            continue;
        }
        if top == Some(b'"') {
            if byte == b'"' {
                stack.pop();
            }
            continue;
        }
        if top == Some(b'(') {
            match byte {
                b'(' => stack.push(byte),
                b')' => {
                    stack.pop();
                }
                _ => {}
            }
            continue;
        }
        if top == Some(b'[') {
            if byte == b']' {
                stack.pop();
            }
            continue;
        }
        match byte {
            b'"' | b'(' | b'[' => stack.push(byte),
            b'<' if top.is_none() => stack.push(byte),
            b'>' if top == Some(b'<') => {
                stack.pop();
            }
            b':' if top.is_none() && !group => group = true,
            b';' if top.is_none() && group => group = false,
            b':' | b';' | b',' if top == Some(b'<') => return false,
            b':' | b';' | b'<' | b'>' | b')' | b']' => return false,
            _ => {}
        }
    }
    stack.is_empty() && !escaped && !group
}
