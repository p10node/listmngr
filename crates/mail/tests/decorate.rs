//! Mailman's `decorate`: list header and footer text added at delivery.
//! Plain text bodies are concatenated; `multipart/mixed` gets inline text
//! parts first and last; anything else is wrapped in a new `multipart/mixed`
//! that keeps the message headers outside and the content headers inside.
use listmngr_mail::decorate::decorate;
use mail_parser::{MessageParser, MimeHeaders, PartType};

fn parsed(raw: &[u8]) -> mail_parser::Message<'_> {
    MessageParser::default().parse(raw).unwrap()
}

fn text_of(raw: &[u8]) -> String {
    parsed(raw).body_text(0).unwrap().replace("\r\n", "\n")
}

fn content_types(raw: &[u8]) -> Vec<String> {
    parsed(raw)
        .parts
        .iter()
        .map(|part| {
            part.content_type().map_or_else(
                || "text/plain".to_owned(),
                |ct| format!("{}/{}", ct.ctype(), ct.subtype().unwrap_or("")),
            )
        })
        .collect()
}

#[test]
fn nothing_to_add_leaves_the_bytes_alone() {
    let raw = b"From: a@example.invalid\r\nSubject: s\r\n\r\nbody\r\n";
    assert_eq!(decorate(raw, "", "").unwrap(), raw);
    assert_eq!(
        decorate(raw, "  \n", "\n").unwrap(),
        raw,
        "whitespace-only decoration is empty"
    );
}

#[test]
fn plain_text_is_concatenated_with_the_rfc_3676_parameters_kept() {
    let raw = b"From: a@example.invalid\r\nSubject: s\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=us-ascii; format=flowed; delsp=yes\r\nContent-Transfer-Encoding: 7bit\r\n\r\nhello world\r\n";
    let out = decorate(raw, "== header ==", "-- \nfooter line\n").unwrap();
    assert_eq!(content_types(&out), ["text/plain"]);
    assert_eq!(
        text_of(&out),
        "== header ==\nhello world\n-- \nfooter line\n"
    );
    let text = String::from_utf8_lossy(&out);
    assert!(text.starts_with("From: a@example.invalid\r\nSubject: s\r\nMIME-Version: 1.0\r\n"));
    assert!(text.contains("format=flowed"), "{text}");
    assert!(text.contains("delsp=yes"), "{text}");
    assert!(
        text.contains("Content-Transfer-Encoding: 7bit\r\n"),
        "{text}"
    );

    // A footer only, on a body that already ends with a newline: no extra blank line.
    let out = decorate(raw, "", "footer").unwrap();
    assert_eq!(text_of(&out), "hello world\nfooter");
    // A header only that lacks a trailing newline gets one.
    let out = decorate(raw, "header", "").unwrap();
    assert_eq!(text_of(&out), "header\nhello world\n");
}

#[test]
fn plain_text_in_another_charset_is_re_encoded_as_utf8() {
    let raw = b"From: a@example.invalid\r\nSubject: s\r\nContent-Type: text/plain; charset=iso-8859-1\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\nCaf=E9 au lait\r\n";
    let out = decorate(raw, "", "Hộp thư chung\n").unwrap();
    assert_eq!(text_of(&out), "Café au lait\nHộp thư chung\n");
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("Content-Type: text/plain; charset=utf-8"),
        "{text}"
    );
    assert!(
        text.contains("Content-Transfer-Encoding: quoted-printable\r\n"),
        "{text}"
    );
    assert!(text.contains("Caf=C3=A9"), "{text}");
    assert!(!text.contains("iso-8859-1"));
}

#[test]
fn multipart_mixed_gets_inline_text_parts_first_and_last() {
    let raw = b"From: a@example.invalid\r\nSubject: s\r\nContent-Type: multipart/mixed; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nbody\r\n--b\r\nContent-Type: application/pdf\r\n\r\nJVBE\r\n--b--\r\n";
    let out = decorate(raw, "HEADER", "FOOTER $x").unwrap();
    assert_eq!(
        content_types(&out),
        [
            "multipart/mixed",
            "text/plain",
            "text/plain",
            "application/pdf",
            "text/plain"
        ]
    );
    let text = String::from_utf8_lossy(&out);
    assert!(text.contains("--b\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 7bit\r\nContent-Disposition: inline\r\n\r\nHEADER\r\n--b\r\nContent-Type: text/plain\r\n\r\nbody\r\n--b\r\n"), "{text}");
    assert!(text.ends_with("\r\nFOOTER $x\r\n--b--\r\n"), "{text}");
    assert!(text.contains("JVBE"), "original parts untouched");

    let out = decorate(raw, "", "FOOTER").unwrap();
    assert_eq!(
        content_types(&out),
        [
            "multipart/mixed",
            "text/plain",
            "application/pdf",
            "text/plain"
        ]
    );
}

#[test]
fn other_structures_are_wrapped_keeping_message_headers_outside() {
    let raw = b"From: a@example.invalid\r\nSubject: s\r\nMIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary=\"alt\"\r\nContent-Transfer-Encoding: 7bit\r\n\r\n--alt\r\nContent-Type: text/plain\r\n\r\nplain\r\n--alt\r\nContent-Type: text/html\r\n\r\n<b>html</b>\r\n--alt--\r\n";
    let out = decorate(raw, "HEADER", "FOOTER").unwrap();
    assert_eq!(
        content_types(&out),
        [
            "multipart/mixed",
            "text/plain",
            "multipart/alternative",
            "text/plain",
            "text/html",
            "text/plain"
        ]
    );
    let text = String::from_utf8_lossy(&out);
    assert!(text.starts_with("From: a@example.invalid\r\nSubject: s\r\nMIME-Version: 1.0\r\n"));
    let message = parsed(&out);
    let inner = message
        .parts
        .iter()
        .find(|part| {
            part.content_type()
                .is_some_and(|ct| ct.subtype() == Some("alternative"))
        })
        .unwrap();
    let inner_headers =
        String::from_utf8_lossy(&out[inner.offset_header as usize..inner.offset_body as usize]);
    assert!(
        inner_headers.contains("Content-Type: multipart/alternative; boundary=\"alt\""),
        "{inner_headers}"
    );
    assert!(
        inner_headers.contains("Content-Transfer-Encoding: 7bit"),
        "{inner_headers}"
    );
    assert!(!inner_headers.contains("Subject"), "{inner_headers}");
    assert!(
        text.contains("--alt\r\nContent-Type: text/plain\r\n\r\nplain\r\n--alt\r\n"),
        "inner bytes verbatim"
    );

    let html =
        b"From: a@example.invalid\r\nSubject: s\r\nContent-Type: text/html\r\n\r\n<p>only</p>\r\n";
    let out = decorate(html, "", "FOOTER").unwrap();
    assert_eq!(
        content_types(&out),
        ["multipart/mixed", "text/html", "text/plain"]
    );
    let message = parsed(&out);
    assert!(
        message
            .parts
            .iter()
            .any(|part| matches!(&part.body, PartType::Html(body) if body.contains("<p>only</p>")))
    );
}

#[test]
fn undecodable_plain_text_falls_back_to_wrapping() {
    // A charset no decoder knows: the body cannot be joined with the text, so
    // the message is wrapped instead of corrupted.
    let raw = b"From: a@example.invalid\r\nSubject: s\r\nContent-Type: text/plain; charset=x-unknown-charset-9\r\nContent-Transfer-Encoding: base64\r\n\r\ngYKD\r\n";
    let out = decorate(raw, "", "FOOTER").unwrap();
    assert_eq!(
        content_types(&out),
        ["multipart/mixed", "text/plain", "text/plain"]
    );
    assert!(
        String::from_utf8_lossy(&out).contains("gYKD"),
        "original bytes kept"
    );
}
