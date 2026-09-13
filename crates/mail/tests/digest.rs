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
        masthead: String::new(),
        header: String::new(),
        footer: String::new(),
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

fn templated(mode: DeliveryMode) -> Digest<'static> {
    let mut digest = input(mode);
    digest.masthead = "Send Tin tức mailing list submissions to news@example.invalid\n".into();
    digest.header = "Read the list rules before replying.\n".into();
    digest.footer = "Tin tức mailing list -- news@example.invalid\n".into();
    digest
}

#[test]
fn a_plaintext_digest_follows_rfc_1153_with_the_list_templates() {
    let raw = build(&templated(DeliveryMode::PlaintextDigests)).unwrap();
    let parsed = MessageParser::default().parse(&raw).unwrap();
    let body = parsed
        .body_text(0)
        .unwrap()
        .replace("\r\n", "\n")
        .replace('\n', "\r\n");
    // Masthead, then the table of contents with each author.
    assert!(
        body.starts_with("Send Tin tức mailing list submissions to"),
        "{body}"
    );
    assert!(body.contains("Today's Topics:\r\n\r\n   1. Xin chào (a@example.invalid)\r\n   2. Second (b@example.invalid)\r\n"), "{body}");
    // The header template, then messages separated by 30 hyphens, each
    // with its numbered header block.
    assert!(
        body.contains("Read the list rules before replying."),
        "{body}"
    );
    assert!(
        body.contains(
            "\r\n----------------------------------------------------------------------\r\n"
        ),
        "{body}"
    );
    assert!(
        body.contains("\r\nMessage: 1\r\nFrom: a@example.invalid\r\nSubject: Xin chào\r\n"),
        "{body}"
    );
    assert!(
        body.contains("\r\n------------------------------\r\n\r\nMessage: 2\r\n"),
        "{body}"
    );
    assert!(body.contains("Nội dung") && body.contains("Different body"));
    // The footer, then Mailman's closing line and its underline.
    assert!(
        body.contains(
            "\r\nSubject: Digest Footer\r\n\r\nTin tức mailing list -- news@example.invalid"
        ),
        "{body}"
    );
    assert!(
        body.ends_with(
            "\r\nEnd of Tin tức Digest, Vol 2, Issue 3\r\n******************************\r\n"
        ),
        "{body}"
    );
    assert_eq!(parsed.subject(), Some("Tin tức Digest, Vol 2, Issue 3"));
}

#[test]
fn a_mime_digest_carries_the_templates_as_their_own_parts() {
    let raw = build(&templated(DeliveryMode::MimeDigests)).unwrap();
    let parsed = MessageParser::default().parse(&raw).unwrap();
    let texts: Vec<String> = parsed
        .text_bodies()
        .filter_map(|part| match &part.body {
            mail_parser::PartType::Text(text) => Some(text.to_string()),
            _ => None,
        })
        .collect();
    assert!(
        texts[0].contains("Send Tin tức mailing list submissions to"),
        "{texts:?}"
    );
    assert!(texts[0].contains("Today's Topics:"), "{texts:?}");
    assert!(
        texts.iter().any(|t| t.contains("Read the list rules")),
        "{texts:?}"
    );
    assert!(
        texts.iter().any(|t| t.contains("Tin tức mailing list --")),
        "{texts:?}"
    );
    assert_eq!(parsed.attachments().count(), 2, "the two posts stay whole");
    assert!(
        texts
            .last()
            .unwrap()
            .starts_with("End of Tin tức Digest, Vol 2, Issue 3")
    );
    // Empty templates leave no empty parts behind: contents and closing only.
    let bare = build(&input(DeliveryMode::MimeDigests)).unwrap();
    let parsed = MessageParser::default().parse(&bare).unwrap();
    assert_eq!(
        parsed
            .text_bodies()
            .filter(|part| matches!(part.body, mail_parser::PartType::Text(_)))
            .count(),
        2
    );
}
