//! Mailman's `mime-delete` content filter over stored bytes: type and
//! extension filtering (outer and nested), collapse of `multipart/alternative`,
//! HTML to plain text, the `X-Content-Filtered-By` marker, and Mailman's
//! disposal reasons when nothing deliverable remains.
use listmngr_core::AlterMessages;
use listmngr_mail::mime_delete::{Verdict, apply};
use mail_parser::MimeHeaders;

const MIXED: &[u8] = b"From: author@example.invalid\r\nTo: dev@example.invalid\r\nSubject: attachments\r\nMessage-ID: <mixed@example.invalid>\r\nContent-Type: multipart/mixed; boundary=\"outer\"\r\n\r\nThis is a MIME message.\r\n--outer\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\nHello =E2=9C=93\r\n--outer\r\nContent-Type: application/pdf; name=\"report.pdf\"\r\nContent-Disposition: attachment; filename=\"report.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERi0x\r\n--outer\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment; filename=\"Setup.EXE\"\r\nContent-Transfer-Encoding: base64\r\n\r\nTVo=\r\n--outer\r\nContent-Type: image/png; name=\"chart.png\"\r\nContent-Transfer-Encoding: base64\r\n\r\niVBORw0KGgo=\r\n--outer--\r\n";

fn settings() -> AlterMessages {
    AlterMessages {
        filter_content: true,
        collapse_alternatives: false,
        ..AlterMessages::default()
    }
}

fn changed(verdict: Verdict) -> Vec<u8> {
    match verdict {
        Verdict::Changed(bytes) => bytes,
        other => panic!("expected a changed message, got {other:?}"),
    }
}

fn disposed(verdict: Verdict) -> String {
    match verdict {
        Verdict::Disposed(reason) => reason,
        other => panic!("expected disposal, got {other:?}"),
    }
}

fn parts(raw: &[u8]) -> Vec<String> {
    let message = mail_parser::MessageParser::default().parse(raw).unwrap();
    message
        .parts
        .iter()
        .map(|part| {
            part.content_type().map_or_else(
                || "text/plain".to_owned(),
                |ct| {
                    ct.subtype().map_or_else(
                        || ct.ctype().to_owned(),
                        |sub| format!("{}/{sub}", ct.ctype()),
                    )
                },
            )
        })
        .collect()
}

#[test]
fn filtering_is_off_by_default_and_a_clean_message_is_untouched() {
    assert_eq!(
        apply(MIXED, &AlterMessages::default()).unwrap(),
        Verdict::Unchanged
    );
    let mut on = settings();
    on.filter_types = vec!["application/x-nothing-here".into()];
    assert_eq!(apply(MIXED, &on).unwrap(), Verdict::Unchanged);
}

#[test]
fn the_outer_type_is_checked_first_with_mailman_reasons() {
    let mut deny = settings();
    deny.filter_types = vec!["multipart/mixed".into()];
    assert_eq!(
        disposed(apply(MIXED, &deny).unwrap()),
        "The message's content type was explicitly disallowed"
    );
    let mut main_type = settings();
    main_type.filter_types = vec!["multipart".into()];
    assert_eq!(
        disposed(apply(MIXED, &main_type).unwrap()),
        "The message's content type was explicitly disallowed"
    );
    let mut allow = settings();
    allow.pass_types = vec!["text/plain".into()];
    assert_eq!(
        disposed(apply(MIXED, &allow).unwrap()),
        "The message's content type was not explicitly allowed"
    );
    let single =
        b"From: a@example.invalid\r\nSubject: x\r\nContent-Type: text/html\r\n\r\n<p>hi</p>\r\n";
    let mut html = settings();
    html.filter_types = vec!["text/html".into()];
    assert_eq!(
        disposed(apply(single, &html).unwrap()),
        "The message's content type was explicitly disallowed"
    );
}

#[test]
fn matching_subparts_are_removed_and_the_rest_stay_byte_identical() {
    let mut filter = settings();
    filter.filter_types = vec!["application/pdf".into(), "image".into()];
    let out = changed(apply(MIXED, &filter).unwrap());
    assert_eq!(
        parts(&out),
        ["multipart/mixed", "text/plain", "application/octet-stream"]
    );
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("Hello =E2=9C=93"),
        "quoted-printable body untouched"
    );
    assert!(text.contains("Content-Type: text/plain; charset=utf-8\r\n"));
    assert!(text.contains("TVo=\r\n"), "kept attachment untouched");
    assert!(!text.contains("JVBERi0x"), "pdf removed");
    assert!(!text.contains("iVBORw0KGgo="), "png removed by main type");
    assert!(text.contains("boundary=\"outer\""), "boundary preserved");
    assert!(text.contains("\r\n--outer--\r\n"), "closing boundary");
    assert!(
        text.contains("X-Content-Filtered-By: listmngr/mime-delete "),
        "{text}"
    );
    assert!(
        text.starts_with("From: author@example.invalid\r\n"),
        "outer headers first"
    );
}

#[test]
fn pass_types_keep_only_what_is_listed_and_the_outer_type() {
    let mut allow = settings();
    allow.pass_types = vec!["multipart".into(), "text/plain".into()];
    let out = changed(apply(MIXED, &allow).unwrap());
    assert_eq!(parts(&out), ["multipart/mixed", "text/plain"]);
}

#[test]
fn extensions_are_compared_case_insensitively_on_the_file_name() {
    let mut filter = settings();
    filter.filter_extensions = vec!["exe".into()];
    let out = changed(apply(MIXED, &filter).unwrap());
    assert_eq!(
        parts(&out),
        [
            "multipart/mixed",
            "text/plain",
            "application/pdf",
            "image/png"
        ]
    );

    let mut allow = settings();
    allow.pass_extensions = vec!["pdf".into()];
    let out = changed(apply(MIXED, &allow).unwrap());
    // Parts without a file name are not subject to extension rules.
    assert_eq!(
        parts(&out),
        ["multipart/mixed", "text/plain", "application/pdf"]
    );
}

#[test]
fn an_emptied_multipart_is_disposed_and_nested_multiparts_are_pruned() {
    let mut everything = settings();
    everything.filter_types = vec!["text".into(), "application".into(), "image".into()];
    assert_eq!(
        disposed(apply(MIXED, &everything).unwrap()),
        "After content filtering, the message was empty"
    );

    let nested = b"From: a@example.invalid\r\nSubject: nested\r\nContent-Type: multipart/mixed; boundary=\"o\"\r\n\r\n--o\r\nContent-Type: text/plain\r\n\r\nbody\r\n--o\r\nContent-Type: multipart/mixed; boundary=\"i\"\r\n\r\n--i\r\nContent-Type: application/pdf\r\n\r\nJVBE\r\n--i\r\nContent-Type: application/zip\r\n\r\nUEsD\r\n--i--\r\n--o--\r\n";
    let mut filter = settings();
    filter.filter_types = vec!["application".into()];
    let out = changed(apply(nested, &filter).unwrap());
    assert_eq!(
        parts(&out),
        ["multipart/mixed", "text/plain"],
        "an inner multipart emptied by filtering is dropped whole"
    );
}

#[test]
fn nested_alternatives_collapse_to_their_first_part() {
    let raw = b"From: a@example.invalid\r\nSubject: alt\r\nContent-Type: multipart/mixed; boundary=\"o\"\r\n\r\n--o\r\nContent-Type: multipart/alternative; boundary=\"a\"\r\n\r\n--a\r\nContent-Type: text/plain; charset=us-ascii\r\n\r\nplain body\r\n--a\r\nContent-Type: text/html\r\n\r\n<b>html body</b>\r\n--a--\r\n--o\r\nContent-Type: application/pdf\r\n\r\nJVBE\r\n--o--\r\n";
    let mut collapse = settings();
    collapse.collapse_alternatives = true;
    let out = changed(apply(raw, &collapse).unwrap());
    assert_eq!(
        parts(&out),
        ["multipart/mixed", "text/plain", "application/pdf"]
    );
    let text = String::from_utf8_lossy(&out);
    assert!(text.contains(
        "--o\r\nContent-Type: text/plain; charset=us-ascii\r\n\r\nplain body\r\n--o\r\n"
    ));
    assert!(!text.contains("html body"));
}

#[test]
fn an_outer_alternative_collapses_keeping_the_message_headers() {
    let raw = b"From: a@example.invalid\r\nSubject: alt\r\nMIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary=\"a\"\r\nContent-Description: outer\r\n\r\n--a\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\nplain body\r\n--a\r\nContent-Type: text/html\r\n\r\n<b>html body</b>\r\n--a--\r\n";
    let mut collapse = settings();
    collapse.collapse_alternatives = true;
    let out = changed(apply(raw, &collapse).unwrap());
    let text = String::from_utf8_lossy(&out);
    assert_eq!(parts(&out), ["text/plain"]);
    assert!(text.starts_with("From: a@example.invalid\r\nSubject: alt\r\nMIME-Version: 1.0\r\n"));
    assert!(!text.contains("multipart/alternative"));
    assert!(!text.contains("Content-Description: outer"));
    assert!(text.contains("Content-Type: text/plain; charset=utf-8\r\n"));
    assert!(text.contains("Content-Transfer-Encoding: 8bit\r\n"));
    assert!(text.ends_with("\r\n\r\nplain body"), "{text:?}");
}

#[test]
fn html_parts_become_utf8_plain_text() {
    let raw = b"From: a@example.invalid\r\nSubject: html\r\nContent-Type: multipart/mixed; boundary=\"o\"\r\n\r\n--o\r\nContent-Type: text/html; charset=iso-8859-1\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n<html><head><style>p{}</style><title>t</title></head><body><p>Caf=E9 &amp; <b>bold</b></p><ul><li>one</li><li>two</li></ul><p>see <a href=3D\"https://example.invalid/x\">the page</a></p><br>done</body></html>\r\n--o--\r\n";
    let mut convert = settings();
    convert.convert_html_to_plaintext = true;
    let out = changed(apply(raw, &convert).unwrap());
    assert_eq!(parts(&out), ["multipart/mixed", "text/plain"]);
    let message = mail_parser::MessageParser::default().parse(&out).unwrap();
    let body = message.body_text(0).unwrap().replace("\r\n", "\n");
    assert_eq!(
        body.trim(),
        "Café & bold\n\n- one\n- two\n\nsee the page <https://example.invalid/x>\n\ndone"
    );
    let text = String::from_utf8_lossy(&out);
    assert!(text.contains("Content-Type: text/plain; charset=utf-8\r\n"));
    assert!(!text.contains("iso-8859-1"));
    assert!(!text.contains("<style>"));

    // A single-part HTML message converts in place, keeping its other headers.
    let single = b"From: a@example.invalid\r\nSubject: html\r\nContent-Type: text/html\r\n\r\n<p>only html</p>\r\n";
    let out = changed(apply(single, &convert).unwrap());
    assert_eq!(parts(&out), ["text/plain"]);
    assert!(
        String::from_utf8_lossy(&out).starts_with("From: a@example.invalid\r\nSubject: html\r\n")
    );
    assert_eq!(
        mail_parser::MessageParser::default()
            .parse(&out)
            .unwrap()
            .body_text(0)
            .unwrap()
            .trim(),
        "only html"
    );
}

#[test]
fn filtering_is_deterministic_and_idempotent() {
    let mut all = settings();
    all.filter_types = vec!["application/pdf".into()];
    all.filter_extensions = vec!["exe".into()];
    all.collapse_alternatives = true;
    all.convert_html_to_plaintext = true;
    let once = changed(apply(MIXED, &all).unwrap());
    assert_eq!(changed(apply(MIXED, &all).unwrap()), once);
    assert_eq!(
        apply(&once, &all).unwrap(),
        Verdict::Unchanged,
        "a filtered message has nothing left to filter"
    );
}

#[test]
fn a_message_that_does_not_parse_is_refused_not_delivered() {
    let mut on = settings();
    on.filter_types = vec!["application/pdf".into()];
    assert!(apply(b"no header block at all", &on).is_err());
}

#[test]
fn html_to_text_survives_hostile_and_multibyte_input() {
    use listmngr_mail::html_text::html_to_text;
    assert_eq!(html_to_text("a &ééééééééééé; b"), "a &ééééééééééé; b");
    assert_eq!(
        html_to_text("&#x1F600; &#65; &amp &unknown; &#0;"),
        "😀 A &amp &unknown; &#0;"
    );
    assert_eq!(
        html_to_text("<a href=\"https://x.invalid/?a=1&amp;b=2\">x</a>"),
        "x <https://x.invalid/?a=1&b=2>"
    );
    assert_eq!(
        html_to_text("<a href=\"https://x.invalid\">https://x.invalid</a>"),
        "https://x.invalid"
    );
    assert_eq!(html_to_text("<p>unterminated <b"), "unterminated");
    assert_eq!(
        html_to_text("<script>alert('x')</script>safe<style>p{}</style>"),
        "safe"
    );
    assert_eq!(html_to_text("<pre>  keep\n   this</pre>"), "keep\n   this");
    assert_eq!(
        html_to_text("<!-- hidden -->shown<![CDATA[ raw <b> ]]>"),
        "shown raw <b>"
    );
    assert_eq!(
        html_to_text("<img alt=\"logo\"><table><tr><td>a</td><td>b</td></tr></table>"),
        "[logo]\n\na b"
    );
    assert_eq!(
        html_to_text("tag attr with quote <a href='x>y'>z</a>"),
        "tag attr with quote z <x>y>"
    );
}
