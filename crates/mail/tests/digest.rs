use listmngr_core::DeliveryMode;
use listmngr_mail::digest::{Digest, build};
use mail_parser::MessageParser;

fn input(mode: DeliveryMode) -> Digest<'static> {
    Digest {
        list: "news.example.invalid".parse().unwrap(),
        display_name: "Tin tức".into(),
        volume: 2,
        number: 3,
        mode,
        timestamp: 1_700_000_000,
        messages: vec![
            b"From: a@example.invalid\r\nSubject: =?UTF-8?Q?Xin_ch=C3=A0o?=\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\nN=E1=BB=99i dung\r\n",
            b"From: b@example.invalid\r\nSubject: Second\r\nContent-Type: text/plain\r\n\r\nDifferent body\r\n",
        ],
    }
}

#[test]
fn plaintext_digest_decodes_mime_and_preserves_differential_messages() {
    let raw = build(&input(DeliveryMode::PlaintextDigests)).unwrap();
    let parsed = MessageParser::default().parse(&raw).unwrap();
    let body = parsed.body_text(0).unwrap();
    assert!(body.contains("Xin chào"));
    assert!(body.contains("Nội dung"));
    assert!(body.contains("Different body"));
    assert!(body.contains("1. ") && body.contains("2. "));
    assert_eq!(parsed.subject(), Some("Tin tức Digest, Vol 2, Issue 3"));
}

#[test]
fn mime_digest_contains_both_complete_messages() {
    let raw = build(&input(DeliveryMode::MimeDigests)).unwrap();
    let parsed = MessageParser::default().parse(&raw).unwrap();
    assert_eq!(parsed.attachments().count(), 2);
    let nested = parsed
        .attachments()
        .map(|part| part.message().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(nested[0].subject(), Some("Xin chào"));
    assert_eq!(nested[1].subject(), Some("Second"));
    assert!(nested[1].body_text(0).unwrap().contains("Different body"));
    assert_eq!(
        raw,
        build(&input(DeliveryMode::MimeDigests)).unwrap(),
        "same immutable issue renders identically"
    );
}

#[test]
fn summary_is_mailman_mime_alias_and_regular_is_not_a_digest() {
    let raw = build(&input(DeliveryMode::SummaryDigests)).unwrap();
    let parsed = MessageParser::default().parse(&raw).unwrap();
    let body = parsed.body_text(0).unwrap();
    assert!(body.contains("Xin chào") && body.contains("Second"));
    assert!(!body.contains("Different body"));
    assert_eq!(parsed.attachments().count(), 2);
    assert!(
        parsed
            .attachments()
            .nth(1)
            .unwrap()
            .message()
            .unwrap()
            .body_text(0)
            .unwrap()
            .contains("Different body")
    );
    assert_eq!(raw, build(&input(DeliveryMode::SummaryDigests)).unwrap());
    assert!(build(&input(DeliveryMode::Regular)).is_err());
}
