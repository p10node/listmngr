//! Ceilings on a message's structure at the intake, independent of the
//! parser's own.
//!
//! They bound how many header fields the outer block carries, how many
//! MIME parts the message has in all, and how deep they nest. A message
//! over any of them is refused before anything is stored, so a crafted
//! post cannot make every later stage — filtering, archiving, digesting —
//! walk a tree built to be expensive.
//!
//! The numbers come from `[mta] max_header_count`, `max_mime_parts` and
//! `max_mime_depth`; the defaults are the security design's (500, 1000,
//! 20). Measuring is one pass over the header block and one parse of the
//! body, bounded by the LMTP size limit that runs first.
use mail_parser::{Message, MessageParser, PartType};
use std::fmt;

/// The three ceilings, as configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Header fields in the outer header block; a folded field counts once.
    pub max_header_count: u32,
    /// MIME parts in all, nested messages' parts included.
    pub max_mime_parts: u32,
    /// Nesting depth; the outer body is 1, a multipart's children 2, and
    /// a `message/rfc822` part's own body one deeper than the part.
    pub max_mime_depth: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_header_count: 500,
            max_mime_parts: 1000,
            max_mime_depth: 20,
        }
    }
}

impl From<&listmngr_core::MtaConfig> for Limits {
    fn from(mta: &listmngr_core::MtaConfig) -> Self {
        Self {
            max_header_count: mta.max_header_count,
            max_mime_parts: mta.max_mime_parts,
            max_mime_depth: mta.max_mime_depth,
        }
    }
}

/// What one message measures.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Measure {
    pub header_count: u32,
    pub mime_parts: u32,
    pub mime_depth: u32,
}

/// The first ceiling a message is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Excess {
    Headers { count: u32, max: u32 },
    Parts { count: u32, max: u32 },
    Depth { depth: u32, max: u32 },
}

impl fmt::Display for Excess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Headers { count, max } => write!(f, "{count} header fields, at most {max}"),
            Self::Parts { count, max } => write!(f, "{count} MIME parts, at most {max}"),
            Self::Depth { depth, max } => write!(f, "MIME nesting {depth} deep, at most {max}"),
        }
    }
}

/// Count the fields of the outer header block: every line up to the first
/// empty one that does not continue the field before it. A message without
/// a blank line is all header.
fn header_count(raw: &[u8]) -> u32 {
    let mut count = 0_u32;
    for line in raw.split_inclusive(|b| *b == b'\n') {
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            break;
        }
        if !(line.starts_with(b" ") || line.starts_with(b"\t")) {
            count = count.saturating_add(1);
        }
    }
    count
}

/// Walk the part tree without recursion (the parser builds it without
/// recursion too, so a deep tree must not overflow the stack here).
fn mime_shape(message: &Message<'_>) -> (u32, u32) {
    let mut parts = 0_u32;
    let mut depth = 0_u32;
    let mut stack: Vec<(&Message<'_>, usize, u32)> = vec![(message, 0, 1)];
    while let Some((owner, id, level)) = stack.pop() {
        let Some(part) = owner.parts.get(id) else {
            continue;
        };
        parts = parts.saturating_add(1);
        depth = depth.max(level);
        match &part.body {
            PartType::Multipart(children) => {
                for child in children {
                    stack.push((owner, *child as usize, level.saturating_add(1)));
                }
            }
            PartType::Message(inner) => stack.push((inner, 0, level.saturating_add(1))),
            PartType::Text(_)
            | PartType::Html(_)
            | PartType::Binary(_)
            | PartType::InlineBinary(_) => {}
        }
    }
    (parts, depth)
}

/// Measure a message: the outer header count and the MIME shape. Bytes the
/// parser cannot read at all count as one part, one deep.
#[must_use]
pub fn measure(raw: &[u8]) -> Measure {
    let (mime_parts, mime_depth) = MessageParser::default()
        .parse(raw)
        .as_ref()
        .map_or((1, 1), mime_shape);
    Measure {
        header_count: header_count(raw),
        mime_parts,
        mime_depth,
    }
}

/// Measure a message and refuse it over the first ceiling it exceeds, in
/// the order headers, parts, depth.
/// # Errors
/// The ceiling exceeded, with the measured and the configured number.
pub fn check(raw: &[u8], limits: &Limits) -> Result<Measure, Excess> {
    let measure = measure(raw);
    if measure.header_count > limits.max_header_count {
        return Err(Excess::Headers {
            count: measure.header_count,
            max: limits.max_header_count,
        });
    }
    if measure.mime_parts > limits.max_mime_parts {
        return Err(Excess::Parts {
            count: measure.mime_parts,
            max: limits.max_mime_parts,
        });
    }
    if measure.mime_depth > limits.max_mime_depth {
        return Err(Excess::Depth {
            depth: measure.mime_depth,
            max: limits.max_mime_depth,
        });
    }
    Ok(measure)
}
