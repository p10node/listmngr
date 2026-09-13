//! Pure digest rendering. Callers provide already policy-cooked posts; this
//! module neither reads a roster nor persists or schedules delivery.
//!
//! The plain-text issue follows RFC 1153 as Mailman writes it: the masthead,
//! the table of contents, the header template, each message under a
//! numbered header block behind a line of thirty hyphens, the footer, and
//! the closing line. The MIME issue (RFC 2046 `multipart/digest`) keeps the
//! posts whole and carries the same three templates as text parts.
use crate::{Error, Result};
use listmngr_core::{DeliveryMode, ListId};
use mail_builder::{
    MessageBuilder,
    headers::{content_type::ContentType, raw::Raw},
    mime::MimePart,
};
use mail_parser::MessageParser;
use sha2::{Digest as _, Sha256};
use std::fmt::Write as _;

/// RFC 1153's message separator.
const SEPARATOR: &str = "------------------------------";
/// Mailman's line between the table of contents and the messages.
const RULE: &str = "----------------------------------------------------------------------";

#[derive(Debug)]
pub struct Digest<'a> {
    pub list: ListId,
    pub display_name: String,
    pub volume: i32,
    pub number: i64,
    pub mode: DeliveryMode,
    /// Unix seconds for a stable issue Date.
    pub timestamp: i64,
    /// The list's resolved digest templates; empty ones are omitted.
    pub masthead: String,
    pub header: String,
    pub footer: String,
    /// Already cooked (including list privacy policy), in accepted-post order.
    pub messages: Vec<&'a [u8]>,
}

/// Render one immutable digest issue.
/// # Errors
/// Returns an error for invalid/empty/oversized inputs or encoding failure.
pub fn build(input: &Digest<'_>) -> Result<Vec<u8>> {
    if input.mode == DeliveryMode::Regular
        || input.messages.is_empty()
        || input.messages.len() > 1_000
        || input.volume < 1
        || input.number < 1
        || input.display_name.len() > 200
        || input.display_name.chars().any(char::is_control)
        || [&input.masthead, &input.header, &input.footer]
            .iter()
            .any(|text| text.len() > 64 * 1024)
        || input
            .messages
            .iter()
            .try_fold(0_usize, |n, m| n.checked_add(m.len()))
            .is_none_or(|n| n > 10 * 1024 * 1024)
    {
        return Err(Error::InvalidDigest);
    }
    let title = format!(
        "{} Digest, Vol {}, Issue {}",
        input.display_name, input.volume, input.number
    );
    let parser = MessageParser::default();
    let messages = input
        .messages
        .iter()
        .map(|raw| parser.parse(*raw).ok_or(Error::InvalidDigest))
        .collect::<Result<Vec<_>>>()?;
    let mut text = String::new();
    if !input.masthead.is_empty() {
        text.push_str(&crlf(&input.masthead));
        text.push_str("\r\n");
    }
    text.push_str("Today's Topics:\r\n\r\n");
    for (index, message) in messages.iter().enumerate() {
        let _ = write!(
            text,
            "   {}. {} ({})\r\n",
            index + 1,
            message.subject().unwrap_or("(no subject)"),
            author(message)
        );
    }
    let closing = format!("End of {title}\r\n{}\r\n", "*".repeat(SEPARATOR.len()));
    let mut builder = MessageBuilder::new()
        .from((input.display_name.clone(), input.list.posting_address()))
        .to(input.list.posting_address())
        .subject(title.as_str())
        .date(input.timestamp)
        .message_id(format!(
            "{}-digest-{}-{}-{}@{}",
            input.list.list_name(),
            input.volume,
            input.number,
            input.mode,
            input.list.mail_host()
        ))
        .header("List-Id", Raw::new(format!("<{}>", input.list)))
        .header(
            "List-Post",
            Raw::new(format!("<mailto:{}>", input.list.posting_address())),
        )
        .header("Precedence", Raw::new("list"));
    if input.mode == DeliveryMode::PlaintextDigests {
        builder = builder.text_body(plaintext_body(input, &messages, text, &closing));
    } else {
        builder = builder.body(mime_body(input, text, closing));
    }
    builder.write_to_vec().map_err(Error::Io)
}

/// RFC 1153's message section after the table of contents.
fn plaintext_body(
    input: &Digest<'_>,
    messages: &[mail_parser::Message<'_>],
    mut text: String,
    closing: &str,
) -> String {
    if !input.header.is_empty() {
        let _ = write!(text, "\r\n{}", crlf(&input.header));
    }
    let _ = write!(text, "\r\n{RULE}\r\n");
    for (index, message) in messages.iter().enumerate() {
        if index > 0 {
            let _ = write!(text, "\r\n{SEPARATOR}\r\n");
        }
        let _ = write!(text, "\r\nMessage: {}\r\n", index + 1);
        for (name, value) in [
            ("From", message.header_raw("From")),
            ("Date", message.header_raw("Date")),
            ("To", message.header_raw("To")),
            ("Subject", message.subject()),
            ("Message-ID", message.message_id()),
        ] {
            if let Some(value) = value {
                let value = value.trim();
                if !value.is_empty() {
                    let _ = write!(text, "{name}: {value}\r\n");
                }
            }
        }
        text.push_str("\r\n");
        text.push_str(&crlf(&message.body_text(0).unwrap_or_else(|| {
            "[Non-text message: use MIME digest to receive its contents.]".into()
        })));
        if !text.ends_with("\r\n") {
            text.push_str("\r\n");
        }
    }
    let _ = write!(text, "\r\n{SEPARATOR}\r\n");
    if !input.footer.is_empty() {
        let _ = write!(
            text,
            "\r\nSubject: Digest Footer\r\n\r\n{}\r\n{SEPARATOR}\r\n",
            crlf(&input.footer)
        );
    }
    let _ = write!(text, "\r\n{closing}");
    text
}

/// The MIME issue: contents, header, the whole posts, footer, closing.
fn mime_body<'a>(input: &Digest<'a>, text: String, closing: String) -> MimePart<'a> {
    let mut hash = Sha256::new();
    for raw in &input.messages {
        hash.update(raw);
    }
    let boundary = format!("listmngr-{:x}", hash.finalize());
    let parts = input
        .messages
        .iter()
        .map(|raw| MimePart::new("message/rfc822", *raw).transfer_encoding("8bit"))
        .collect::<Vec<_>>();
    let outer_boundary = format!("{boundary}-outer");
    let mut outer = vec![MimePart::new("text/plain", text)];
    if !input.header.is_empty() {
        outer.push(MimePart::new("text/plain", crlf(&input.header)));
    }
    outer.push(MimePart::new(
        ContentType::new("multipart/digest").attribute("boundary", boundary),
        parts,
    ));
    if !input.footer.is_empty() {
        outer.push(MimePart::new("text/plain", crlf(&input.footer)));
    }
    outer.push(MimePart::new("text/plain", closing));
    MimePart::new(
        ContentType::new("multipart/mixed").attribute("boundary", outer_boundary),
        outer,
    )
}

/// Line endings as the wire wants them, whatever a template was typed with.
fn crlf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\r\n")
}

/// The author as Mailman prints it in the table of contents.
fn author(message: &mail_parser::Message<'_>) -> String {
    message.from().and_then(|from| from.first()).map_or_else(
        || "(unknown sender)".into(),
        |address| {
            let email = address.address().unwrap_or_default();
            match address.name() {
                Some(name) if !name.trim().is_empty() => {
                    format!("{} <{email}>", name.trim())
                }
                _ => email.to_string(),
            }
        },
    )
}
