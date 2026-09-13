//! Message facts the `in` runner hands to the posting rules: unfolded header
//! fields, a bounded body preview, and the `Approved:` posting key.
use listmngr_mail::facts::{approved_key, body_preview_lines, header_fields};

// Built without `\`-continuations: those strip the leading whitespace that
// makes ` world` a folded continuation of the Subject field.
const SIMPLE: &[u8] = b"From: alice@example.invalid\r\nSubject: hello\r\n world\r\nReceived: from a\r\nReceived: from b\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nfirst line\r\n\r\nsecond line\r\n-- \r\nsignature\r\n";

#[test]
fn header_fields_unfold_and_keep_every_occurrence_in_order() {
    let fields = header_fields(SIMPLE);
    assert_eq!(
        fields,
        vec![
            ("From".to_owned(), "alice@example.invalid".to_owned()),
            ("Subject".to_owned(), "hello world".to_owned()),
            ("Received".to_owned(), "from a".to_owned()),
            ("Received".to_owned(), "from b".to_owned()),
            (
                "Content-Type".to_owned(),
                "text/plain; charset=utf-8".to_owned()
            ),
        ]
    );
}

#[test]
fn header_fields_stop_at_the_blank_line_and_survive_a_missing_body() {
    assert_eq!(
        header_fields(b"Subject: only\r\n"),
        vec![("Subject".to_owned(), "only".to_owned())]
    );
    assert!(header_fields(b"").is_empty());
    // A line without a colon is not a field and does not poison later ones.
    assert_eq!(
        header_fields(b"garbage\r\nSubject: kept\r\n\r\n"),
        vec![("Subject".to_owned(), "kept".to_owned())]
    );
}

#[test]
fn body_preview_skips_blank_lines_stops_at_the_signature_and_caps() {
    assert_eq!(
        body_preview_lines(SIMPLE, 10),
        vec!["first line".to_owned(), "second line".to_owned()]
    );
    assert_eq!(body_preview_lines(SIMPLE, 1), vec!["first line".to_owned()]);
    assert!(body_preview_lines(b"Subject: x\r\n\r\n", 10).is_empty());
}

#[test]
fn body_preview_reads_the_first_text_plain_part_of_a_multipart_message() {
    let raw = b"From: a@example.invalid\r\n\
Content-Type: multipart/alternative; boundary=b\r\n\
\r\n\
--b\r\n\
Content-Type: text/html\r\n\
\r\n\
<p>html first</p>\r\n\
--b\r\n\
Content-Type: text/plain\r\n\
Content-Transfer-Encoding: quoted-printable\r\n\
\r\n\
unsubscribe=20now\r\n\
--b--\r\n";
    assert_eq!(
        body_preview_lines(raw, 10),
        vec!["unsubscribe now".to_owned()]
    );
}

#[test]
fn approved_key_prefers_headers_and_accepts_every_spelling() {
    for name in ["Approved", "Approve", "X-Approved", "X-Approve"] {
        let raw = format!("From: a@example.invalid\r\n{name}: s3cret\r\n\r\nbody\r\n");
        assert_eq!(
            approved_key(raw.as_bytes()).as_deref(),
            Some("s3cret"),
            "{name}"
        );
    }
    let raw = b"From: a@example.invalid\r\nApproved: from-header\r\n\r\nApproved: from-body\r\n";
    assert_eq!(approved_key(raw).as_deref(), Some("from-header"));
}

#[test]
fn approved_key_reads_only_the_very_first_body_line() {
    let raw = b"From: a@example.invalid\r\n\r\napproved: lower-case-works\r\nrest\r\n";
    assert_eq!(approved_key(raw).as_deref(), Some("lower-case-works"));
    let raw = b"From: a@example.invalid\r\n\r\n\r\nApproved: after-blank\r\n";
    assert_eq!(approved_key(raw), None, "a leading blank line means no key");
    let raw = b"From: a@example.invalid\r\n\r\nApproved:\r\n";
    assert_eq!(approved_key(raw), None, "an empty key is no key");
}

#[test]
fn approved_key_in_an_encoded_body_is_not_recognized() {
    // A key we cannot strip from the redistributed bytes must not be honored.
    let raw = b"From: a@example.invalid\r\n\
Content-Type: text/plain\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
QXBwcm92ZWQ6IHNlY3JldA0K\r\n";
    assert_eq!(approved_key(raw), None);
}
