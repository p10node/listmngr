//! Shared rendering for generated notices: resolve the template for the
//! list, expand Mailman placeholders, and serialize a safe `text/plain` MIME
//! message with ASCII-validated transport headers.
//!
//! Bodies may be any UTF-8 (operators write them); the header block never is.
//! Non-ASCII subjects are RFC 2047 encoded and non-ASCII or long-line bodies
//! travel as base64, so a template can never inject headers or break the
//! transport line limit.

use base64::Engine;
use listmngr_core::{Error, ListId, MailingList, Result};
use listmngr_mail::templates::{Placeholders, expand};
use sqlx::{Any, Transaction};
use std::fmt::Write as _;

use crate::{db_error, list_from_row};

/// Read the list row inside a transaction without taking the writer lock.
/// # Errors
/// Returns not-found or a database error.
pub async fn list_snapshot(tx: &mut Transaction<'_, Any>, id: &ListId) -> Result<MailingList> {
    let row = sqlx::query("SELECT * FROM mailing_lists WHERE list_id=$1")
        .bind(id.as_str())
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_error)?
        .ok_or_else(|| Error::NotFound(id.to_string()))?;
    list_from_row(&row)
}

/// The placeholders every list notice can use (Mailman names).
#[must_use]
pub fn list_placeholders(list: &MailingList) -> Placeholders {
    let id = &list.id;
    Placeholders::new()
        .set("listname", id.posting_address())
        .set("fqdn_listname", id.posting_address())
        .set("list_id", id.to_string())
        .set("short_listname", id.list_name())
        .set("list_name", id.list_name())
        .set("display_name", list.display_name.clone())
        .set("description", list.description.clone())
        .set("info", list.info.clone())
        .set("domain", id.mail_host())
        .set("list_domain", id.mail_host())
        .set("request_email", id.request_address())
        .set("list_requests", id.request_address())
        .set("owner_email", id.owner_address())
        .set("bounces_email", id.bounces_address())
        .set("join_email", id.join_address())
        .set("leave_email", id.leave_address())
}

/// The language a notice to `email` on `list` should use.
///
/// The recipient's own preference (member, then address, then user layer)
/// when they hold a membership on the list, else the list's language, else
/// the site default, each only if a catalog ships for it; English last.
/// # Errors
/// Returns a database error.
pub async fn recipient_language(
    tx: &mut Transaction<'_, Any>,
    list: &MailingList,
    email: &str,
    site_default: &str,
) -> Result<String> {
    let identity = listmngr_core::Address::new(email, String::new())
        .map_or_else(|_| email.to_ascii_lowercase(), |address| address.email);
    let preferred: Option<String> = sqlx::query_scalar(
        "SELECT COALESCE(pm.preferred_language, pa.preferred_language, pu.preferred_language) \
         FROM members m JOIN addresses a ON a.id=m.address_id \
         LEFT JOIN preferences pm ON pm.id=m.preferences_id \
         LEFT JOIN preferences pa ON pa.id=a.preferences_id \
         LEFT JOIN users u ON u.id=m.user_id \
         LEFT JOIN preferences pu ON pu.id=u.preferences_id \
         WHERE m.list_id=$1 AND a.email=$2 ORDER BY m.role LIMIT 1",
    )
    .bind(list.id.as_str())
    .bind(&identity)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_error)?
    .flatten();
    let chosen = listmngr_i18n::choose(
        [
            preferred.as_deref().unwrap_or(""),
            list.preferred_language.as_str(),
            site_default,
        ]
        .into_iter()
        .filter(|candidate| !candidate.is_empty()),
    );
    Ok(chosen.to_owned())
}

/// Resolve `template` for `list` in `language` and expand.
/// # Errors
/// Returns validation for an unknown template name or a database error.
pub async fn render(
    tx: &mut Transaction<'_, Any>,
    list: &MailingList,
    template: &str,
    language: &str,
    placeholders: &Placeholders,
) -> Result<String> {
    let resolved = crate::templates::resolve_tx(tx, template, list, language).await?;
    Ok(expand(&resolved.body, placeholders))
}

/// Transport headers of a generated notice. Every value is validated ASCII
/// except `subject`, which is encoded when needed.
#[derive(Debug)]
pub struct Envelope<'a> {
    pub from: &'a str,
    pub to: &'a str,
    pub reply_to: Option<&'a str>,
    pub subject: &'a str,
    /// Local part of the generated `Message-ID`; the list's host completes it.
    pub message_id_local: &'a str,
    pub mail_host: &'a str,
    pub date: &'a str,
}

fn safe_mailbox(value: &str) -> Result<()> {
    if listmngr_mail::owner::safe_mailbox(value) && value.len() <= 254 {
        Ok(())
    } else {
        Err(Error::Validation("unsupported notice mailbox".into()))
    }
}

fn safe_ascii_line(value: &str) -> bool {
    !value.is_empty() && value.len() <= 998 && value.bytes().all(|b| (0x20..=0x7e).contains(&b))
}

/// RFC 2047 `B` encoding for a non-ASCII subject; ASCII subjects pass as is.
fn encoded_subject(subject: &str) -> Result<String> {
    let subject = subject.trim();
    if subject.is_empty() || subject.chars().any(char::is_control) {
        return Err(Error::Validation("unsupported notice subject".into()));
    }
    if subject.is_ascii() && subject.len() <= 200 {
        return Ok(subject.to_owned());
    }
    // Encode in chunks small enough to keep each encoded word under 75 bytes.
    let mut words = Vec::new();
    let mut chunk = String::new();
    for character in subject.chars() {
        if chunk.len() + character.len_utf8() > 33 {
            words.push(format!(
                "=?utf-8?B?{}?=",
                base64::engine::general_purpose::STANDARD.encode(&chunk)
            ));
            chunk.clear();
        }
        chunk.push(character);
    }
    if !chunk.is_empty() {
        words.push(format!(
            "=?utf-8?B?{}?=",
            base64::engine::general_purpose::STANDARD.encode(&chunk)
        ));
    }
    Ok(words.join("\r\n "))
}

/// The validated transport header block, without the blank line.
fn header_block(envelope: &Envelope<'_>, subject: &str, body_len: usize) -> String {
    let mut raw = String::with_capacity(body_len + 512);
    let _ = write!(raw, "From: {}\r\n", envelope.from);
    let _ = write!(raw, "To: {}\r\n", envelope.to);
    if let Some(reply_to) = envelope.reply_to {
        let _ = write!(raw, "Reply-To: {reply_to}\r\n");
    }
    let _ = write!(raw, "Subject: {subject}\r\n");
    let _ = write!(
        raw,
        "Message-ID: <{}@{}>\r\n",
        envelope.message_id_local, envelope.mail_host
    );
    let _ = write!(raw, "Date: {}\r\n", envelope.date);
    raw.push_str("Auto-Submitted: auto-generated\r\n");
    raw.push_str("MIME-Version: 1.0\r\n");
    raw.push_str("Content-Type: text/plain; charset=utf-8\r\n");
    raw
}

/// Serialize a notice. The body is CRLF-normalized; it travels 7bit when it
/// is ASCII with short lines, otherwise base64 in 76-column lines.
/// # Errors
/// Returns validation errors for any unsafe mailbox, host or subject.
pub fn serialize(envelope: &Envelope<'_>, body: &str) -> Result<Vec<u8>> {
    safe_mailbox(envelope.from)?;
    safe_mailbox(envelope.to)?;
    if let Some(reply_to) = envelope.reply_to {
        safe_mailbox(reply_to)?;
    }
    if !safe_ascii_line(envelope.mail_host)
        || !safe_ascii_line(envelope.message_id_local)
        || envelope.message_id_local.contains(['@', '<', '>', ' '])
        || !safe_ascii_line(envelope.date)
    {
        return Err(Error::Validation("unsupported notice header".into()));
    }
    let subject = encoded_subject(envelope.subject)?;
    let body = body.replace("\r\n", "\n").replace('\n', "\r\n");
    let seven_bit = body.is_ascii() && body.split("\r\n").all(|line| line.len() <= 998);
    let mut raw = header_block(envelope, &subject, body.len());
    if seven_bit {
        raw.push_str("\r\n");
        raw.push_str(&body);
        return Ok(raw.into_bytes());
    }
    raw.push_str("Content-Transfer-Encoding: base64\r\n\r\n");
    let encoded = base64::engine::general_purpose::STANDARD.encode(body.as_bytes());
    let mut bytes = raw.into_bytes();
    for line in encoded.as_bytes().chunks(76) {
        bytes.extend_from_slice(line);
        bytes.extend_from_slice(b"\r\n");
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(subject: &str) -> Envelope<'_> {
        Envelope {
            from: "dev-owner@example.invalid",
            to: "alice@example.invalid",
            reply_to: None,
            subject,
            message_id_local: "abc",
            mail_host: "example.invalid",
            date: "Mon, 1 Sep 2026 10:00:00 +0000",
        }
    }

    #[test]
    fn ascii_bodies_travel_7bit_with_crlf() {
        let raw = serialize(&envelope("Hello"), "line one\nline two\n").unwrap();
        let text = String::from_utf8(raw).unwrap();
        assert!(text.ends_with("\r\n\r\nline one\r\nline two\r\n"));
        assert!(!text.contains("Content-Transfer-Encoding"));
        assert!(text.contains("Subject: Hello\r\n"));
    }

    #[test]
    fn non_ascii_bodies_and_subjects_are_encoded() {
        let raw = serialize(&envelope("Chào mừng"), "Xin chào\n").unwrap();
        let text = String::from_utf8(raw).unwrap();
        assert!(text.contains("Subject: =?utf-8?B?"));
        assert!(text.contains("Content-Transfer-Encoding: base64\r\n\r\n"));
        assert!(!text.contains("Xin chào"));
        let parsed = mail_parser::MessageParser::default()
            .parse(text.as_bytes())
            .unwrap();
        assert_eq!(parsed.subject(), Some("Chào mừng"));
        assert_eq!(parsed.body_text(0).as_deref(), Some("Xin chào\r\n"));
    }

    #[test]
    fn a_template_cannot_inject_headers_or_over_long_lines() {
        let body = format!("Approved: sneaky\r\n\r\n{}\n", "x".repeat(2000));
        let raw = serialize(&envelope("Hi"), &body).unwrap();
        let text = String::from_utf8(raw).unwrap();
        assert!(text.contains("Content-Transfer-Encoding: base64"));
        for line in text.split("\r\n") {
            assert!(line.len() <= 998);
        }
        assert!(!text.contains("Approved: sneaky"));
    }

    #[test]
    fn unsafe_envelopes_are_refused() {
        let mut bad = envelope("Hi");
        bad.to = "alice@example.invalid\r\nBcc: x@example.invalid";
        assert!(serialize(&bad, "x").is_err());
        assert!(serialize(&envelope(""), "x").is_err());
        assert!(serialize(&envelope("line\nbreak"), "x").is_err());
        let mut bad = envelope("Hi");
        bad.message_id_local = "a b";
        assert!(serialize(&bad, "x").is_err());
    }
}
