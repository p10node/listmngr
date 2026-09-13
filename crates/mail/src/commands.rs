//! Bounded email commands and reply-loop guard. Examine every parsed header.

use mail_parser::{MimeHeaders, PartType};

/// Read an explicit `confirm TOKEN` subject (including Re:) or first nonblank
/// text body line. Unlike Mailman's hex tokens, our base64url tokens retain case.
#[must_use]
pub fn confirmation_token(raw: &[u8]) -> Option<String> {
    match parse(raw)? {
        Command::Confirm(token) => Some(token),
        _ => None,
    }
}

/// Shared durable command type, independent of mail and database adapters.
pub use listmngr_core::EmailCommand as Command;

/// Subject takes precedence; otherwise use first nonblank of 20 text lines.
#[must_use]
pub fn parse(raw: &[u8]) -> Option<Command> {
    let message = mail_parser::MessageParser::default().parse(raw)?;
    // body_text() also converts HTML; only an actual plain-text body is a
    // command carrier. Missing Content-Type defaults to text/plain.
    let body = message.text_bodies().find_map(|part| match &part.body {
        PartType::Text(text)
            if !part
                .content_disposition()
                .is_some_and(mail_parser::ContentType::is_attachment)
                && (part.content_type().is_none() || part.is_content_type("text", "plain")) =>
        {
            Some(text.as_ref())
        }
        _ => None,
    });
    let subject = message.subject().unwrap_or_default().trim();
    let line = if subject.is_empty() {
        body?
            .lines()
            .take(20)
            .find(|line| !line.trim().is_empty())?
            .trim()
    } else {
        subject
    };
    let line = if line
        .get(..3)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("re:"))
    {
        line[3..].trim()
    } else {
        line
    };
    let mut words = line.split_whitespace();
    let command = words.next()?.to_ascii_lowercase();
    // `echo` is the one verb whose argument is free text, so it is read from
    // the rest of the line rather than from the word iterator.
    if command == "echo" {
        return echo_text(&line[command.len()..]).map(Command::Echo);
    }
    if command != "confirm" {
        if words.next().is_some() {
            return None;
        }
        return match command.as_str() {
            "join" | "subscribe" => Some(Command::Join),
            "leave" | "unsubscribe" => Some(Command::Leave),
            "help" => Some(Command::Help),
            // Mailman's halt: everything after it (a signature, a quoted
            // reply) is deliberately not read as a command.
            "end" | "stop" => Some(Command::End),
            _ => None,
        };
    }
    let token = words.next()?;
    if words.next().is_some()
        || token.len() != 43
        || !token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return None;
    }
    Some(Command::Confirm(token.to_owned()))
}

/// The text an `echo` carries back: printable, single-line and bounded, so a
/// reply can never be steered by control characters or grown without limit.
fn echo_text(rest: &str) -> Option<String> {
    let text = rest.trim();
    if text.chars().count() > MAX_ECHO_CHARS
        || text.chars().any(|c| {
            c.is_control() || matches!(c, '\u{200e}'..='\u{200f}' | '\u{202a}'..='\u{202e}')
        })
    {
        return None;
    }
    Some(text.to_owned())
}

/// Characters an `echo` may carry back.
pub const MAX_ECHO_CHARS: usize = 200;

/// Whether an otherwise admitted message permits a command confirmation reply.
/// Missing/malformed MIME fails closed; Auto-Submitted must be absent or `no`.
#[must_use]
pub fn allows_reply(raw: &[u8]) -> bool {
    mail_parser::MessageParser::default()
        .parse(raw)
        .is_some_and(|message| {
            !message.headers().iter().any(|header| {
                header.name.as_str().eq_ignore_ascii_case("auto-submitted")
                    && header
                        .value
                        .as_text()
                        .is_none_or(|value| !value.trim().eq_ignore_ascii_case("no"))
            })
        })
}
