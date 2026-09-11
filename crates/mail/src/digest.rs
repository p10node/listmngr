//! Pure digest rendering. Callers provide already policy-cooked posts; this
//! module neither reads a roster nor persists or schedules delivery.
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

#[derive(Debug)]
pub struct Digest<'a> {
    pub list: ListId,
    pub display_name: String,
    pub volume: i32,
    pub number: i64,
    pub mode: DeliveryMode,
    /// Unix seconds for a stable issue Date.
    pub timestamp: i64,
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
    let mut text = format!("{title}\r\n\r\nToday's topics:\r\n\r\n");
    let parser = MessageParser::default();
    let messages = input
        .messages
        .iter()
        .map(|raw| parser.parse(*raw).ok_or(Error::InvalidDigest))
        .collect::<Result<Vec<_>>>()?;
    for (index, message) in messages.iter().enumerate() {
        let _ = write!(
            text,
            "{}. {}\r\n",
            index + 1,
            message.subject().unwrap_or("(no subject)")
        );
    }
    if input.mode == DeliveryMode::PlaintextDigests {
        for message in &messages {
            text.push_str("\r\n----------------------------------------------------------------------\r\n\r\n");
            let _ = write!(
                text,
                "Subject: {}\r\n\r\n",
                message.subject().unwrap_or("(no subject)")
            );
            text.push_str(&message.body_text(0).unwrap_or_else(|| {
                "[Non-text message: use MIME digest to receive its contents.]".into()
            }));
        }
    }
    text.push_str("\r\n\r\nEnd of Digest\r\n*************\r\n");
    let mut builder = MessageBuilder::new()
        .from((input.display_name.clone(), input.list.posting_address()))
        .to(input.list.posting_address())
        .subject(title)
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
    if matches!(
        input.mode,
        DeliveryMode::MimeDigests | DeliveryMode::SummaryDigests
    ) {
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
        builder = builder.body(MimePart::new(
            ContentType::new("multipart/mixed").attribute("boundary", format!("{boundary}-outer")),
            vec![
                MimePart::new("text/plain", text),
                MimePart::new(
                    ContentType::new("multipart/digest").attribute("boundary", boundary),
                    parts,
                ),
            ],
        ));
    } else {
        builder = builder.text_body(text);
    }
    builder.write_to_vec().map_err(Error::Io)
}
