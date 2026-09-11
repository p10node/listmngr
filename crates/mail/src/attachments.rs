//! Bounded attachment projection from already-authorized, cooked archive MIME.
use crate::{Error, Result};
use mail_parser::{Encoding, Message, MessageParser, MimeHeaders, PartType};

const MAX_MESSAGE_BYTES: usize = 10 * 1024 * 1024;
const MAX_ATTACHMENTS: usize = 64;

fn parse(raw: &[u8]) -> Result<Message<'_>> {
    if raw.len() > MAX_MESSAGE_BYTES {
        return Err(Error::CorruptMessage);
    }
    let message = MessageParser::default()
        .parse(raw)
        .ok_or(Error::CorruptMessage)?;
    if message.attachments().count() > MAX_ATTACHMENTS
        || message.parts.iter().any(|part| part.is_encoding_problem)
    {
        return Err(Error::CorruptMessage);
    }
    Ok(message)
}

/// Display names only; callers must escape them and must not use them as paths.
/// # Errors
/// Rejects malformed or oversized messages and excessive attachment counts.
pub fn names(raw: &[u8]) -> Result<Vec<String>> {
    Ok(parse(raw)?
        .attachments()
        .enumerate()
        .map(|(index, part)| {
            part.attachment_name().map_or_else(
                || format!("Attachment {index}"),
                |name| name.chars().take(200).collect(),
            )
        })
        .collect())
}

/// Decode one zero-based MIME attachment without charset conversion. Text display
/// and file download are different projections. Never render these bytes inline.
/// # Errors
/// Rejects malformed or oversized messages and excessive attachment counts.
pub fn content(raw: &[u8], index: usize) -> Result<Option<Vec<u8>>> {
    let message = parse(raw)?;
    let Some(part) = message.attachments().nth(index) else {
        return Ok(None);
    };
    if !matches!(part.body, PartType::Text(_) | PartType::Html(_)) {
        return Ok(Some(part.contents().to_vec()));
    }
    // The parser's Text/Html bodies have already undergone lossy charset
    // conversion. Decode the original selected MIME body, not that display text.
    let encoded = message
        .raw_message
        .get(part.offset_body as usize..part.offset_end as usize)
        .ok_or(Error::CorruptMessage)?;
    let decoded = match part.encoding {
        Encoding::None => Some(encoded.to_vec()),
        Encoding::Base64 => mail_parser::decoders::base64::base64_decode(encoded),
        Encoding::QuotedPrintable => {
            mail_parser::decoders::quoted_printable::quoted_printable_decode(encoded)
        }
    }
    .ok_or(Error::CorruptMessage)?;
    Ok(Some(decoded))
}
