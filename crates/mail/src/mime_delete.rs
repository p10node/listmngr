//! Mailman's `mime-delete` content filter, applied to the stored bytes.
//!
//! The message is read into a tree of parts whose header blocks and leaf
//! bodies are the original byte spans, so everything that survives the
//! filter is byte-identical to what arrived: encodings, charsets and any
//! signature inside a kept part stay valid. Only the multipart framing is
//! regenerated around the kept children (the preamble and epilogue of a
//! rewritten multipart are dropped, as Mailman's rewrite drops them).
//!
//! Order and reasons follow `mailman/handlers/mime_delete.py`: outer type,
//! outer file extension, recursive part filtering, `multipart/alternative`
//! collapse to the first alternative, HTML to plain text, and the
//! `X-Content-Filtered-By` marker when anything changed.
use crate::{Error, Result, cook, encoding, html_text};
use listmngr_core::AlterMessages;
use mail_parser::{Message, MessageParser, MessagePart, MimeHeaders, PartType};

/// Header added to a message the filter changed.
pub const FILTERED_BY_HEADER: &str = "X-Content-Filtered-By";

const MAX_MESSAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_PARTS: usize = 1000;

/// What the filter decided for one message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing matched; deliver the original bytes.
    Unchanged,
    /// Parts were removed or rewritten; deliver these bytes instead.
    Changed(Vec<u8>),
    /// Nothing deliverable remains; the reason is Mailman's wording and the
    /// list's `filter_action` decides what happens to the post.
    Disposed(String),
}

#[derive(Debug)]
enum Body {
    /// A leaf body, verbatim.
    Verbatim(Vec<u8>),
    Multipart {
        boundary: String,
        children: Vec<Part>,
    },
}

#[derive(Debug)]
struct Part {
    /// The header block including its terminating blank line.
    headers: Vec<u8>,
    body: Body,
    /// Lower-case `type/subtype`.
    ctype: String,
    /// Lower-case file-name extension, when the part names a file.
    extension: Option<String>,
    /// The decoded text of a `text/html` leaf.
    html: Option<String>,
}

impl Part {
    fn main_type(&self) -> &str {
        self.ctype.split('/').next().unwrap_or_default()
    }

    /// `multipart/alternative` with a multipart body. A part whose type
    /// says so but carries no `boundary` parses as a leaf: collapsing has
    /// nothing to choose from and leaves it as it is (a fuzz find; the
    /// `unreachable!` branches below rely on this check).
    fn is_alternative(&self) -> bool {
        self.ctype == "multipart/alternative" && matches!(self.body, Body::Multipart { .. })
    }

    fn children(&self) -> Option<&[Self]> {
        match &self.body {
            Body::Multipart { children, .. } => Some(children),
            Body::Verbatim(_) => None,
        }
    }
}

struct Rules<'a> {
    settings: &'a AlterMessages,
}

impl Rules<'_> {
    fn type_disallowed(&self, part: &Part) -> bool {
        let filter = &self.settings.filter_types;
        filter
            .iter()
            .any(|t| t == &part.ctype || t == part.main_type())
    }

    fn type_not_allowed(&self, part: &Part) -> bool {
        let pass = &self.settings.pass_types;
        !pass.is_empty()
            && !pass
                .iter()
                .any(|t| t == &part.ctype || t == part.main_type())
    }

    fn extension_disallowed(&self, part: &Part) -> bool {
        part.extension
            .as_ref()
            .is_some_and(|ext| self.settings.filter_extensions.contains(ext))
    }

    fn extension_not_allowed(&self, part: &Part) -> bool {
        let pass = &self.settings.pass_extensions;
        !pass.is_empty()
            && part
                .extension
                .as_ref()
                .is_some_and(|ext| !pass.contains(ext))
    }

    /// Mailman's per-subpart test: dropped when its type or extension says so.
    fn drops(&self, part: &Part) -> bool {
        self.type_disallowed(part)
            || self.type_not_allowed(part)
            || self.extension_disallowed(part)
            || self.extension_not_allowed(part)
    }
}

fn content_type_of(part: &MessagePart<'_>) -> String {
    part.content_type().map_or_else(
        || "text/plain".to_owned(),
        |ct| {
            let mut name = ct.ctype().to_ascii_lowercase();
            if let Some(subtype) = ct.subtype() {
                name.push('/');
                name.push_str(&subtype.to_ascii_lowercase());
            }
            name
        },
    )
}

fn extension_of(part: &MessagePart<'_>) -> Option<String> {
    let name = part.attachment_name()?;
    let (_, extension) = name.rsplit_once('.')?;
    let extension = extension.trim();
    (!extension.is_empty() && !extension.contains(char::is_whitespace))
        .then(|| extension.to_ascii_lowercase())
}

fn build(message: &Message<'_>, raw: &[u8], id: usize, budget: &mut usize) -> Result<Part> {
    *budget = budget.checked_sub(1).ok_or(Error::CorruptMessage)?;
    let part = message.parts.get(id).ok_or(Error::CorruptMessage)?;
    let header_start = part.offset_header as usize;
    let body_start = part.offset_body as usize;
    let body_end = part.offset_end as usize;
    if header_start > body_start || body_start > body_end || body_end > raw.len() {
        return Err(Error::CorruptMessage);
    }
    let headers = raw[header_start..body_start].to_vec();
    let ctype = content_type_of(part);
    let extension = extension_of(part);
    let (body, html) = match &part.body {
        PartType::Multipart(children) => {
            let boundary = part
                .content_type()
                .and_then(|ct| ct.attribute("boundary"))
                .ok_or(Error::CorruptMessage)?
                .to_owned();
            let children = children
                .iter()
                .map(|child| build(message, raw, *child as usize, budget))
                .collect::<Result<Vec<_>>>()?;
            (Body::Multipart { boundary, children }, None)
        }
        PartType::Html(text) => (
            Body::Verbatim(raw[body_start..body_end].to_vec()),
            Some(text.to_string()),
        ),
        _ => (Body::Verbatim(raw[body_start..body_end].to_vec()), None),
    };
    Ok(Part {
        headers,
        body,
        ctype,
        extension,
        html,
    })
}

/// Mailman's `filter_parts`: prune matching children recursively. Returns
/// whether `part` itself survives (a multipart emptied by filtering does
/// not). `changed` records that something was removed.
fn filter_parts(part: &mut Part, rules: &Rules<'_>, changed: &mut bool) -> bool {
    let Body::Multipart { children, .. } = &mut part.body else {
        return true;
    };
    let before = children.len();
    let mut kept = Vec::with_capacity(before);
    for mut child in children.drain(..) {
        if !filter_parts(&mut child, rules, changed) || rules.drops(&child) {
            *changed = true;
            continue;
        }
        kept.push(child);
    }
    *children = kept;
    !(children.is_empty() && before > 0)
}

fn is_content_field(name: &str) -> bool {
    matches!(
        name,
        "content-type"
            | "content-transfer-encoding"
            | "content-disposition"
            | "content-description"
    )
}

/// Header lines of `headers` (a block with its blank line) that `keep`
/// selects, without the blank line.
fn fields(headers: &[u8], keep: impl Fn(&str) -> bool) -> Vec<u8> {
    let end = cook::header_body_split(headers).map_or(headers.len(), |(blank, _)| blank);
    cook::strip_fields(&headers[..end], |name| !keep(name))
}

fn blank_line(headers: &[u8]) -> &'static [u8] {
    cook::newline_style(headers)
}

/// Mailman's `reset_payload`: `part` takes `donor`'s body and content
/// headers while keeping its own other headers.
fn reset_payload(part: &mut Part, donor: Part) {
    let newline = blank_line(&part.headers);
    let mut headers = fields(&part.headers, |name| !is_content_field(name));
    headers.extend_from_slice(&fields(&donor.headers, is_content_field));
    headers.extend_from_slice(newline);
    part.headers = headers;
    part.body = donor.body;
    part.ctype = donor.ctype;
    part.extension = donor.extension;
    part.html = donor.html;
}

/// Replace every nested `multipart/alternative` with its first alternative.
fn collapse_alternatives(part: &mut Part, changed: &mut bool) {
    let Body::Multipart { children, .. } = &mut part.body else {
        return;
    };
    let mut replaced = Vec::with_capacity(children.len());
    for mut child in children.drain(..) {
        if child.is_alternative() {
            *changed = true;
            let Body::Multipart {
                children: mut alternatives,
                ..
            } = child.body
            else {
                unreachable!("an alternative part is multipart");
            };
            if alternatives.is_empty() {
                continue;
            }
            let mut first = alternatives.swap_remove(0);
            collapse_alternatives(&mut first, changed);
            replaced.push(first);
        } else {
            collapse_alternatives(&mut child, changed);
            replaced.push(child);
        }
    }
    *children = replaced;
}

/// Mailman's `to_plaintext`: every `text/html` leaf becomes UTF-8 plain text.
fn html_to_plaintext(part: &mut Part, changed: &mut bool) {
    if let Body::Multipart { children, .. } = &mut part.body {
        for child in children {
            html_to_plaintext(child, changed);
        }
        return;
    }
    let Some(html) = part.html.take() else {
        return;
    };
    *changed = true;
    let newline = blank_line(&part.headers);
    let text = html_text::html_to_text(&html);
    let (encoding, body) = encoding::text_body(&text, newline);
    let mut headers = fields(&part.headers, |name| {
        name != "content-type" && name != "content-transfer-encoding"
    });
    headers.extend_from_slice(b"Content-Type: text/plain; charset=utf-8");
    headers.extend_from_slice(newline);
    headers.extend_from_slice(b"Content-Transfer-Encoding: ");
    headers.extend_from_slice(encoding.as_bytes());
    headers.extend_from_slice(newline);
    headers.extend_from_slice(newline);
    part.headers = headers;
    part.body = Body::Verbatim(body);
    part.ctype = "text/plain".into();
}

fn serialize(part: &Part, out: &mut Vec<u8>) {
    out.extend_from_slice(&part.headers);
    match &part.body {
        Body::Verbatim(bytes) => out.extend_from_slice(bytes),
        Body::Multipart { boundary, children } => {
            let newline = blank_line(&part.headers);
            for child in children {
                out.extend_from_slice(b"--");
                out.extend_from_slice(boundary.as_bytes());
                out.extend_from_slice(newline);
                serialize(child, out);
                out.extend_from_slice(newline);
            }
            out.extend_from_slice(b"--");
            out.extend_from_slice(boundary.as_bytes());
            out.extend_from_slice(b"--");
            out.extend_from_slice(newline);
        }
    }
}

/// Apply the list's content filter to `raw`.
/// # Errors
/// Returns [`Error::CorruptMessage`] when the message does not parse or is
/// out of bounds; callers refuse rather than deliver it unfiltered.
pub fn apply(raw: &[u8], settings: &AlterMessages) -> Result<Verdict> {
    if !settings.filter_content {
        return Ok(Verdict::Unchanged);
    }
    if raw.len() > MAX_MESSAGE_BYTES {
        return Err(Error::CorruptMessage);
    }
    let message = MessageParser::default()
        .parse(raw)
        .ok_or(Error::CorruptMessage)?;
    cook::header_body_split(raw).ok_or(Error::CorruptMessage)?;
    let mut budget = MAX_PARTS;
    let mut root = build(&message, raw, 0, &mut budget)?;
    let rules = Rules { settings };

    if rules.type_disallowed(&root) {
        return Ok(Verdict::Disposed(
            "The message's content type was explicitly disallowed".into(),
        ));
    }
    if rules.type_not_allowed(&root) {
        return Ok(Verdict::Disposed(
            "The message's content type was not explicitly allowed".into(),
        ));
    }
    if rules.extension_disallowed(&root) {
        return Ok(Verdict::Disposed(
            "The message's file extension was explicitly disallowed".into(),
        ));
    }
    if rules.extension_not_allowed(&root) {
        return Ok(Verdict::Disposed(
            "The message's file extension was not explicitly allowed".into(),
        ));
    }

    let mut changed = false;
    if root.children().is_some() && !filter_parts(&mut root, &rules, &mut changed) {
        return Ok(Verdict::Disposed(
            "After content filtering, the message was empty".into(),
        ));
    }
    if settings.collapse_alternatives {
        if root.is_alternative() {
            let Body::Multipart { children, .. } = &mut root.body else {
                unreachable!("an alternative part is multipart");
            };
            if !children.is_empty() {
                let first = children.swap_remove(0);
                reset_payload(&mut root, first);
                changed = true;
            }
        }
        collapse_alternatives(&mut root, &mut changed);
    }
    if settings.convert_html_to_plaintext {
        html_to_plaintext(&mut root, &mut changed);
    }
    if !changed {
        return Ok(Verdict::Unchanged);
    }
    let mut out = Vec::with_capacity(raw.len());
    serialize(&root, &mut out);
    let marked = cook::append_headers(
        &out,
        &[(
            FILTERED_BY_HEADER.to_owned(),
            format!("listmngr/mime-delete {}", env!("CARGO_PKG_VERSION")),
        )],
    )?;
    Ok(Verdict::Changed(marked))
}
