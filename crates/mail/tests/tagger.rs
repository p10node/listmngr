//! Mailman's `tagger`: topic patterns matched against subjects, keywords and
//! the header-like lines opening the body, reported in `X-Topics`.
use listmngr_core::{MailingList, Topic};
use listmngr_mail::handlers::{Target, cook_for};

fn list(enabled: bool, limit: i32) -> MailingList {
    let mut list = MailingList::new("dev.example.invalid".parse().unwrap(), "Dev".into());
    list.topics_enabled = enabled;
    list.topics_bodylines_limit = limit;
    list.topics = vec![
        Topic {
            name: "Rust".into(),
            pattern: "cargo\nborrow checker".into(),
            description: "Rust talk".into(),
        },
        Topic {
            name: "Python".into(),
            pattern: r"\bpip\b".into(),
            description: String::new(),
        },
    ];
    list
}

fn topics_header(raw: &[u8]) -> Option<String> {
    listmngr_mail::header_value(raw, "x-topics")
}

fn message(subject: &str, extra: &str, body: &str) -> Vec<u8> {
    format!(
        "From: member@example.invalid\r\nTo: dev@example.invalid\r\nSubject: {subject}\r\nMessage-ID: <t@example.invalid>\r\n{extra}\r\n{body}\r\n"
    )
    .into_bytes()
}

#[test]
fn subjects_keywords_and_leading_pseudo_headers_are_matched() {
    let list = list(true, 5);
    let out = cook_for(
        Target::Out,
        &message("The BORROW CHECKER", "", "hi"),
        &list,
        "id",
    )
    .unwrap();
    assert_eq!(topics_header(&out).as_deref(), Some("Rust"));

    let out = cook_for(
        Target::Out,
        &message("hello", "Keywords: pip\r\n", "hi"),
        &list,
        "id",
    )
    .unwrap();
    assert_eq!(topics_header(&out).as_deref(), Some("Python"));

    let out = cook_for(
        Target::Out,
        &message(
            "hello",
            "",
            "Subject: cargo build\r\nKeywords: pip\r\n\r\nreal body",
        ),
        &list,
        "id",
    )
    .unwrap();
    assert_eq!(topics_header(&out).as_deref(), Some("Rust, Python"));
}

#[test]
fn the_body_scan_stops_at_the_first_ordinary_line_and_at_the_limit() {
    let list = list(true, 5);
    let out = cook_for(
        Target::Out,
        &message("hello", "", "real body first\r\nSubject: cargo"),
        &list,
        "id",
    )
    .unwrap();
    assert_eq!(topics_header(&out), None, "a non-header line ends the scan");

    let short = list_with_limit(1);
    let out = cook_for(
        Target::Out,
        &message("hello", "", "Keywords: nothing\r\nSubject: cargo"),
        &short,
        "id",
    )
    .unwrap();
    assert_eq!(topics_header(&out), None, "only one body line is scanned");

    let none = list_with_limit(0);
    let out = cook_for(
        Target::Out,
        &message("hello", "", "Subject: cargo"),
        &none,
        "id",
    )
    .unwrap();
    assert_eq!(topics_header(&out), None);

    let all = list_with_limit(-1);
    let out = cook_for(
        Target::Out,
        &message("hello", "", "Keywords: a\r\nKeywords: b\r\nKeywords: c\r\nKeywords: d\r\nKeywords: e\r\nKeywords: f\r\nSubject: cargo"),
        &all,
        "id",
    )
    .unwrap();
    assert_eq!(topics_header(&out).as_deref(), Some("Rust"));
}

fn list_with_limit(limit: i32) -> MailingList {
    list(true, limit)
}

#[test]
fn disabled_or_empty_topics_leave_the_message_alone_everywhere() {
    let disabled = list(false, 5);
    let raw = message("cargo", "", "hi");
    for target in [Target::Out, Target::Archive, Target::Digest] {
        assert_eq!(
            topics_header(&cook_for(target, &raw, &disabled, "id").unwrap()),
            None
        );
    }
    let mut empty = list(true, 5);
    empty.topics.clear();
    assert_eq!(
        topics_header(&cook_for(Target::Out, &raw, &empty, "id").unwrap()),
        None
    );

    // Every consumer sees the tag: it is added before the fan-out snapshots.
    let enabled = list(true, 5);
    for target in [Target::Out, Target::Archive, Target::Digest] {
        assert_eq!(
            topics_header(&cook_for(target, &raw, &enabled, "id").unwrap()).as_deref(),
            Some("Rust"),
            "{target:?}"
        );
    }
}
