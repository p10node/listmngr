//! Pure digest rendering. Callers provide already policy-cooked posts; this
//! module neither reads a roster nor persists or schedules delivery.
//!
//! Both issues are laid out as Mailman 3.3's `RFC1153Digester` and
//! `MIMEDigester` (`mailman/runners/digest.py`) lay them out, byte for byte
//! where the inputs allow; `tests/digest_snapshot.rs` holds the comparison
//! with real Mailman output. The plain issue is the wrapped masthead, the
//! digest header, the table of contents, seventy hyphens, then each post's
//! kept headers and scrubbed text behind thirty hyphens, the footer as its
//! own "Digest Footer" section and the sign-off. The MIME issue carries the
//! same texts as described `text/plain` parts around a `multipart/digest`
//! of the whole posts.
use crate::{Error, Result};
use listmngr_core::{DeliveryMode, ListId};
use mail_builder::{
    MessageBuilder,
    headers::{content_type::ContentType, raw::Raw, text::Text},
    mime::MimePart,
};
use mail_parser::{HeaderValue, MessageParser, MimeHeaders, PartType, parsers::MessageStream};
use sha2::{Digest as _, Sha256};
use std::fmt::Write as _;

/// RFC 1153's message separator.
const SEPARATOR30: &str = "------------------------------";
/// Mailman's line between the table of contents and the messages.
const SEPARATOR70: &str = "----------------------------------------------------------------------";
/// Mailman's `plain_digest_keep_headers` default: the headers of a post
/// that the plain issue prints, in this order.
const PLAIN_KEEP_HEADERS: [&str; 9] = [
    "Message",
    "Date",
    "From",
    "Subject",
    "To",
    "Cc",
    "Message-ID",
    "Keywords",
    "Content-Type",
];
/// Mailman's scrubber note between two parts of one post.
const NEXT_PART: &str = "-------------- next part --------------\n";

#[derive(Debug)]
pub struct Digest<'a> {
    pub list: ListId,
    pub display_name: String,
    /// The list's subject prefix, left out of the table of contents.
    pub subject_prefix: String,
    pub volume: i32,
    pub number: i64,
    pub mode: DeliveryMode,
    /// Unix seconds for a stable issue Date.
    pub timestamp: i64,
    /// The list's resolved digest templates; empty ones are omitted. The
    /// masthead is wrapped here, as Mailman's digester wraps it.
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
        || input.subject_prefix.len() > 200
        || input.subject_prefix.chars().any(char::is_control)
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
    let masthead = wrap(&input.masthead, 70);
    let header = decorated(&input.header);
    let footer = decorated(&input.footer);
    let toc = table_of_contents(&messages, &input.subject_prefix);
    // Mailman's headers: `From` the request address, `To` and `Reply-To`
    // the posting address.
    let mut builder = MessageBuilder::new()
        .from(request_address(&input.list))
        .to(input.list.posting_address())
        .reply_to(input.list.posting_address())
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
        builder = builder.text_body(crlf(&plaintext_body(
            &title, &masthead, &header, &footer, &toc, &messages,
        )));
    } else {
        builder = builder.body(mime_body(input, &title, masthead, header, footer, toc));
    }
    builder.write_to_vec().map_err(Error::Io)
}

/// Mailman's `RFC1153Digester`: its `print` calls, in its order.
fn plaintext_body(
    title: &str,
    masthead: &str,
    header: &str,
    footer: &str,
    toc: &str,
    messages: &[mail_parser::Message<'_>],
) -> String {
    let mut text = String::new();
    let _ = writeln!(text, "{masthead}\n");
    if !header.is_empty() {
        let _ = writeln!(text, "{header}\n");
    }
    let _ = writeln!(text, "{toc}\n\n{SEPARATOR70}\n");
    for (index, message) in messages.iter().enumerate() {
        let count = index + 1;
        if count > 1 {
            let _ = writeln!(text, "{SEPARATOR30}\n");
        }
        let _ = writeln!(text, "Message: {count}");
        for name in PLAIN_KEEP_HEADERS {
            if let Some(raw) = message.header_raw(name) {
                let line = wrap(&format!("{name}: {}", oneline(raw)), 70);
                let _ = writeln!(text, "{}", line.replace('\n', "\n\t"));
            }
        }
        let _ = writeln!(text);
        let payload = scrub(message);
        let _ = writeln!(text, "{payload}");
        if !payload.ends_with('\n') {
            let _ = writeln!(text);
        }
    }
    if !footer.is_empty() {
        let _ = writeln!(
            text,
            "{SEPARATOR30}\n\nSubject: Digest Footer\n\n{footer}\n\n{SEPARATOR30}\n"
        );
    }
    let sign_off = format!("End of {title}");
    let _ = writeln!(text, "{sign_off}\n{}", "*".repeat(sign_off.chars().count()));
    text
}

/// Mailman's `MIMEDigester`: the masthead, the header, the contents, the
/// whole posts (each with a `Message: n` header) and the footer. Mailman's
/// "End of" postamble never reaches the wire, so it is not written here.
fn mime_body(
    input: &Digest<'_>,
    title: &str,
    masthead: String,
    header: String,
    footer: String,
    toc: String,
) -> MimePart<'static> {
    let mut hash = Sha256::new();
    for raw in &input.messages {
        hash.update(raw);
    }
    let boundary = format!("listmngr-{:x}", hash.finalize());
    let posts = input
        .messages
        .iter()
        .enumerate()
        .map(|(index, raw)| {
            MimePart::new("message/rfc822", with_message_header(raw, index + 1))
                .transfer_encoding("8bit")
        })
        .collect::<Vec<_>>();
    let described = |text: String, description: String| {
        MimePart::new("text/plain", crlf(&text))
            .header("Content-Description", Text::new(description))
    };
    let mut outer = vec![described(masthead, title.to_owned())];
    if !header.is_empty() {
        outer.push(described(header, "Digest Header".into()));
    }
    outer.push(described(
        toc,
        format!("Today's Topics ({} messages)", input.messages.len()),
    ));
    outer.push(MimePart::new(
        ContentType::new("multipart/digest").attribute("boundary", boundary.clone()),
        posts,
    ));
    if !footer.is_empty() {
        outer.push(described(footer, "Digest Footer".into()));
    }
    MimePart::new(
        ContentType::new("multipart/mixed").attribute("boundary", format!("{boundary}-outer")),
        outer,
    )
}

/// The post with Mailman's `Message: n` header added after its own.
fn with_message_header(raw: &[u8], count: usize) -> Vec<u8> {
    let find = |needle: &[u8]| raw.windows(needle.len()).position(|w| w == needle);
    let (at, eol): (usize, &[u8]) = match (find(b"\r\n\r\n"), find(b"\n\n")) {
        (Some(i), _) => (i + 2, b"\r\n"),
        (None, Some(i)) => (i + 1, b"\n"),
        (None, None) => (raw.len(), b"\r\n"),
    };
    let mut out = Vec::with_capacity(raw.len() + 16);
    out.extend_from_slice(&raw[..at]);
    if at == raw.len() && !raw.ends_with(b"\n") {
        out.extend_from_slice(eol);
    }
    out.extend_from_slice(format!("Message: {count}").as_bytes());
    out.extend_from_slice(eol);
    out.extend_from_slice(&raw[at..]);
    out
}

/// Mailman's table of contents: each post's subject without the list's
/// prefix and its author's name, numbered.
fn table_of_contents(messages: &[mail_parser::Message<'_>], subject_prefix: &str) -> String {
    let mut toc = String::from("Today's Topics:\n\n");
    let prefix =
        regex::RegexBuilder::new(&format!("^(re:? *)?({})", regex::escape(subject_prefix)))
            .case_insensitive(true)
            .build()
            .ok();
    for (index, message) in messages.iter().enumerate() {
        let count = index + 1;
        let mut subject = message
            .header_raw("Subject")
            .map_or_else(|| "(no subject)".to_owned(), oneline);
        // The redundant prefix, also behind a "Re:", is left out.
        if let Some(found) = prefix.as_ref().and_then(|re| re.captures(&subject))
            && let Some(m) = found.get(2)
        {
            subject = format!("{}{}", &subject[..m.start()], &subject[m.end()..]);
        }
        let username = author(message)
            .map(|name| format!(" ({name})"))
            .unwrap_or_default();
        let mut lines: Vec<String> = wrap(&format!("{count:>2}. {subject}"), 65)
            .split('\n')
            .map(str::to_owned)
            .collect();
        // The author's name goes on the last line when it fits there.
        match lines.last_mut() {
            Some(last) if last.chars().count() + username.chars().count() <= 70 => {
                last.push_str(&username);
            }
            _ => lines.push(username),
        }
        for (position, line) in lines.iter().enumerate() {
            if position == 0 {
                let _ = writeln!(toc, "  {line}");
            } else {
                let _ = writeln!(toc, "      {}", line.trim_start());
            }
        }
    }
    toc
}

/// Mailman's scrubber: every `text/plain` part's text, every other leaf
/// part replaced by a note, joined by the "next part" line. Where Mailman
/// glues that line onto a text part's last line, this starts a new line.
fn scrub(message: &mail_parser::Message<'_>) -> String {
    let mut text = String::new();
    scrub_into(message, &mut text);
    text
}

fn scrub_into(message: &mail_parser::Message<'_>, text: &mut String) {
    for part in &message.parts {
        let payload = match &part.body {
            PartType::Multipart(_) => continue,
            PartType::Message(inner) => {
                scrub_into(inner, text);
                continue;
            }
            PartType::Text(body) if is_plain_text(part) => body.to_string(),
            _ => {
                let ctype = part.content_type().map_or_else(
                    || "text/plain".to_owned(),
                    |ct| {
                        ct.subtype().map_or_else(
                            || ct.ctype().to_lowercase(),
                            |subtype| format!("{}/{subtype}", ct.ctype()).to_lowercase(),
                        )
                    },
                );
                format!(
                    "A message part incompatible with plain text digests has been removed ...\nName: {}\nType: {ctype}\nSize: {} bytes\nDesc: {}\n",
                    part.attachment_name().unwrap_or("not available"),
                    part.contents().len(),
                    part.content_description().unwrap_or("not available"),
                )
            }
        };
        if !text.is_empty() {
            if !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(NEXT_PART);
        }
        text.push_str(&payload);
    }
}

/// `text/plain`, which a part without a `Content-Type` is.
fn is_plain_text(part: &mail_parser::MessagePart<'_>) -> bool {
    part.content_type().is_none_or(|ct| {
        ct.ctype().eq_ignore_ascii_case("text")
            && ct.subtype().is_none_or(|s| s.eq_ignore_ascii_case("plain"))
    })
}

/// Mailman's `decorate`: a whitespace-only template is nothing, and a
/// template's line endings are `\n`.
fn decorated(text: &str) -> String {
    if text.trim().is_empty() {
        String::new()
    } else {
        text.replace("\r\n", "\n")
    }
}

/// Mailman's `oneline`: the header value decoded (RFC 2047) and unfolded.
fn oneline(raw: &str) -> String {
    let bytes = format!("{}\n", raw.trim_end_matches(['\r', '\n']));
    match MessageStream::new(bytes.as_bytes()).parse_unstructured() {
        HeaderValue::Text(text) => text.into_owned(),
        _ => String::new(),
    }
}

/// `<list>-request@<host>`, the digest's sender.
fn request_address(list: &ListId) -> String {
    format!("{}-request@{}", list.list_name(), list.mail_host())
}

/// Mailman's `wrap` (`mailman.utilities.string.wrap`).
///
/// Fills each unindented paragraph at `column`, copies an indented
/// paragraph verbatim, and keeps the blank lines between paragraphs; a
/// change of indentation starts a paragraph without a blank line.
#[must_use]
pub fn wrap(text: &str, column: usize) -> String {
    let text = text.replace("\r\n", "\n");
    let mut paragraphs: Vec<String> = Vec::new();
    let mut paragraph = String::new();
    let mut last_indented = false;
    for line in text.split_inclusive('\n') {
        let indented = line.starts_with(is_space);
        if line == "\n" {
            if !paragraph.is_empty() {
                paragraphs.push(std::mem::take(&mut paragraph));
            }
            paragraphs.push("\n".into());
            last_indented = false;
        } else if last_indented != indented {
            if !paragraph.is_empty() {
                paragraphs.push(std::mem::take(&mut paragraph));
            }
            paragraph.push_str(line);
            last_indented = indented;
        } else {
            paragraph.push_str(line);
        }
    }
    if !paragraph.is_empty() {
        paragraphs.push(paragraph);
    }
    let mut out = String::new();
    let mut pending_break = false;
    for paragraph in paragraphs {
        if pending_break {
            out.push('\n');
            pending_break = false;
        }
        if paragraph == "\n" {
            out.push('\n');
        } else if paragraph.starts_with(is_space) {
            out.push_str(&paragraph);
        } else {
            out.push_str(&fill(&paragraph, column));
            pending_break = true;
        }
    }
    out
}

/// Python's `string.whitespace`.
const fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c')
}

/// Python's `TextWrapper(width, break_on_hyphens=False,
/// fix_sentence_endings=True).fill` over one paragraph: whitespace
/// collapsed to spaces (tabs expanded to eight columns), two spaces after
/// a sentence end, words longer than the width broken at it.
fn fill(paragraph: &str, width: usize) -> String {
    let mut chunks: Vec<String> = Vec::new();
    for ch in munge_whitespace(paragraph).chars() {
        match chunks.last_mut() {
            Some(last) if last.starts_with(' ') == (ch == ' ') => last.push(ch),
            _ => chunks.push(ch.to_string()),
        }
    }
    for index in 0..chunks.len().saturating_sub(1) {
        if chunks[index + 1] == " " && sentence_end(&chunks[index]) {
            chunks[index + 1] = "  ".into();
        }
    }
    chunks.reverse();
    let length = |chunk: &String| chunk.chars().count();
    let mut lines: Vec<String> = Vec::new();
    while !chunks.is_empty() {
        let mut line: Vec<String> = Vec::new();
        let mut used = 0;
        if !lines.is_empty() && chunks.last().is_some_and(|c| c.trim().is_empty()) {
            chunks.pop();
        }
        while let Some(chunk) = chunks.last() {
            let l = length(chunk);
            if used + l > width {
                break;
            }
            used += l;
            line.extend(chunks.pop());
        }
        if let Some(chunk) = chunks.last_mut()
            && length(chunk) > width
        {
            let space_left = if width < 1 { 1 } else { width - used };
            let head: String = chunk.chars().take(space_left).collect();
            *chunk = chunk.chars().skip(space_left).collect();
            line.push(head);
        }
        if line.last().is_some_and(|c| c.trim().is_empty()) {
            line.pop();
        }
        if !line.is_empty() {
            lines.push(line.concat());
        }
    }
    lines.join("\n")
}

/// `expand_tabs` and `replace_whitespace` of Python's `TextWrapper`.
fn munge_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut column = 0;
    for ch in text.chars() {
        match ch {
            '\t' => {
                let n = 8 - column % 8;
                out.push_str(&" ".repeat(n));
                column += n;
            }
            '\n' | '\r' => {
                out.push(' ');
                column = 0;
            }
            c if is_space(c) => {
                out.push(' ');
                column += 1;
            }
            c => {
                out.push(c);
                column += 1;
            }
        }
    }
    out
}

/// `fix_sentence_endings`: a lowercase letter, then `.`, `!` or `?`, then
/// optionally a quote, ends a sentence.
fn sentence_end(word: &str) -> bool {
    let word = word.strip_suffix(['"', '\'']).unwrap_or(word);
    let mut chars = word.chars().rev();
    matches!(chars.next(), Some('.' | '!' | '?'))
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
}

/// Line endings as the wire wants them, whatever a template was typed with.
fn crlf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\r\n")
}

/// The first author as Mailman names them in the table of contents: the
/// display name, else the address.
fn author(message: &mail_parser::Message<'_>) -> Option<String> {
    let address = message.from()?.first()?;
    match address.name() {
        Some(name) if !name.trim().is_empty() => Some(name.trim().to_owned()),
        _ => address
            .address()
            .filter(|email| !email.is_empty())
            .map(str::to_owned),
    }
}
