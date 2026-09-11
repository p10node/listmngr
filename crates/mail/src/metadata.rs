use crate::{Error, Result, validate_id};

/// Best-effort, first-occurrence lookup of one header's unfolded value.
///
/// Unlike [`parse_message_id`], this never errors: malformed or absent
/// headers simply return `None`. Intended for advisory use (loop detection,
/// display-only subject text), not security decisions.
#[must_use]
pub fn header_value(raw: &[u8], name: &str) -> Option<String> {
    let mut value = None::<String>;
    let mut collecting = false;
    let mut have_field = false;
    for line in raw[..raw.len().min(MAX_HEADER_BYTES)].split_inclusive(|b| *b == b'\n') {
        let line = line.strip_suffix(b"\n")?;
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            break;
        }
        if line.starts_with(b" ") || line.starts_with(b"\t") {
            if collecting && have_field {
                let value = value.as_mut()?;
                value.push(' ');
                value.push_str(std::str::from_utf8(line).ok()?.trim_matches([' ', '\t']));
            }
            continue;
        }
        let colon = line.iter().position(|b| *b == b':')?;
        have_field = true;
        collecting = value.is_none() && line[..colon].eq_ignore_ascii_case(name.as_bytes());
        if collecting {
            value = Some(
                std::str::from_utf8(&line[colon + 1..])
                    .ok()?
                    .trim_matches([' ', '\t'])
                    .to_owned(),
            );
        }
    }
    value
}

/// Maximum header block size, including the terminating blank line.
pub const MAX_HEADER_BYTES: usize = 65_536;
/// Maximum physical header line length, excluding CRLF/LF.
pub const MAX_HEADER_LINE_BYTES: usize = 998;

/// Extract an unbracketed, case-preserved Message-ID without decoding the body.
///
/// Accepts CRLF or LF and folded field whitespace. Requires exactly one ID and
/// a terminating blank line. Modern ASCII dot-atom IDs only: comments, quoted
/// obsolete syntax and domain literals are deliberately unsupported.
/// # Errors
/// Rejects missing/duplicate IDs, malformed headers and exceeded byte bounds.
pub fn parse_message_id(raw: &[u8]) -> Result<String> {
    parse_optional_message_id(raw)?.ok_or(Error::InvalidMessageId)
}

/// Validate the bounded header block, returning `None` only for an absent ID.
/// This does not authorize delivery or relax malformed/duplicate-ID rejection.
/// # Errors
/// Rejects malformed headers, invalid IDs and exceeded header byte bounds.
pub fn parse_optional_message_id(raw: &[u8]) -> Result<Option<String>> {
    let mut value = None::<String>;
    let mut collecting = false;
    let mut have_field = false;
    let mut ended = false;
    let mut consumed = 0;
    for line in raw[..raw.len().min(MAX_HEADER_BYTES)].split_inclusive(|b| *b == b'\n') {
        consumed += line.len();
        let line = line.strip_suffix(b"\n").ok_or(Error::InvalidMessageId)?;
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            ended = true;
            break;
        }
        if line.len() > MAX_HEADER_LINE_BYTES
            || line.iter().any(|b| (*b < 32 && *b != b'\t') || *b == 127)
        {
            return Err(Error::InvalidMessageId);
        }
        if line.starts_with(b" ") || line.starts_with(b"\t") {
            if !have_field {
                return Err(Error::InvalidMessageId);
            }
            if collecting {
                let value = value.as_mut().ok_or(Error::InvalidMessageId)?;
                value.push(' ');
                value.push_str(
                    std::str::from_utf8(line)
                        .map_err(|_| Error::InvalidMessageId)?
                        .trim_matches([' ', '\t']),
                );
            }
        } else {
            let colon = line
                .iter()
                .position(|b| *b == b':')
                .ok_or(Error::InvalidMessageId)?;
            if colon == 0 || !line[..colon].iter().all(|b| (33..=126).contains(b)) {
                return Err(Error::InvalidMessageId);
            }
            have_field = true;
            collecting = line[..colon].eq_ignore_ascii_case(b"message-id");
            if collecting {
                if value.is_some() {
                    return Err(Error::InvalidMessageId);
                }
                value = Some(
                    std::str::from_utf8(&line[colon + 1..])
                        .map_err(|_| Error::InvalidMessageId)?
                        .trim_matches([' ', '\t'])
                        .into(),
                );
            }
        }
    }
    if !ended || consumed > MAX_HEADER_BYTES {
        return Err(Error::InvalidMessageId);
    }
    let Some(value) = value else {
        return Ok(None);
    };
    let id = value
        .trim_matches([' ', '\t'])
        .strip_prefix('<')
        .and_then(|v| v.strip_suffix('>'))
        .ok_or(Error::InvalidMessageId)?;
    validate_id(id)?;
    Ok(Some(id.into()))
}
