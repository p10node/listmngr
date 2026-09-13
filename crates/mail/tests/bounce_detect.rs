//! The heuristic bounce detectors over a fixture corpus: one message per
//! MTA family, each with the addresses it must yield — and the warnings and
//! human mail it must not mistake for a bounce.
use listmngr_mail::bounce::{Detection, detect};
use std::collections::BTreeSet;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/bounces")
            .join(name),
    )
    .unwrap()
}

fn addresses(names: &[&str]) -> Detection {
    Detection::Failed(names.iter().map(|name| (*name).to_owned()).collect())
}

#[test]
fn every_fixture_in_the_corpus_is_classified_as_expected() {
    let expected = [
        ("dsn.eml", addresses(&["nina@example.net"]), "dsn"),
        ("dsn-delayed.eml", Detection::Temporary, "dsn"),
        (
            "postfix-text.eml",
            addresses(&["alice@example.net", "bob@example.org"]),
            "postfix",
        ),
        ("qmail.eml", addresses(&["carol@example.net"]), "qmail"),
        (
            "exim.eml",
            addresses(&["dave@example.net", "erin@example.org"]),
            "exim",
        ),
        (
            "sendmail.eml",
            addresses(&["frank@example.net"]),
            "sendmail",
        ),
        ("yahoo.eml", addresses(&["grace@yahoo.example"]), "yahoo"),
        (
            "exchange.eml",
            addresses(&["heidi@example.net"]),
            "exchange",
        ),
        (
            "exchange-ndr.eml",
            addresses(&["ivan@example.net"]),
            "exchange",
        ),
        (
            "generic-user-unknown.eml",
            addresses(&["judy@example.net"]),
            "simplematch",
        ),
        ("warning-delayed.eml", Detection::Temporary, "postfix"),
        ("generic-delay.eml", Detection::Temporary, "warning"),
        ("out-of-office.eml", Detection::Unrecognized, "none"),
    ];
    let corpus: BTreeSet<String> = std::fs::read_dir(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bounces"),
    )
    .unwrap()
    .map(|entry| entry.unwrap().file_name().into_string().unwrap())
    .collect();
    let listed: BTreeSet<String> = expected
        .iter()
        .map(|(name, ..)| (*name).to_owned())
        .collect();
    assert_eq!(corpus, listed, "every fixture has an expectation");
    for (name, detection, detector) in expected {
        let result = detect(&fixture(name));
        assert_eq!(result.detection, detection, "{name}");
        assert_eq!(result.detector, detector, "{name}");
    }
}

#[test]
fn addresses_are_bounded_lowercased_and_never_the_mailer_daemon() {
    let raw = format!(
        "From: MAILER-DAEMON@mx.example.net\r\nTo: dev-bounces@example.invalid\r\nSubject: Undelivered Mail Returned to Sender\r\n\r\nThis is the mail system at host mx.example.net.\r\n\r\n<Upper.Case@Example.NET>: user unknown\r\n<postmaster@example.net>: quoted here too\r\n<MAILER-DAEMON@example.net>: and this one\r\n{}",
        (0..300).fold(String::new(), |mut out, n| {
            use std::fmt::Write as _;
            let _ = write!(out, "<user{n}@example.net>: unknown user\r\n");
            out
        })
    );
    let result = detect(raw.as_bytes());
    let Detection::Failed(found) = result.detection else {
        panic!("expected failures, got {:?}", result.detection);
    };
    assert!(found.contains("upper.case@example.net"));
    assert!(!found.contains("postmaster@example.net"));
    assert!(!found.iter().any(|a| a.starts_with("mailer-daemon")));
    assert_eq!(found.len(), 100, "at most a hundred addresses per report");
}

#[test]
fn an_oversized_or_unparsable_message_is_unrecognized() {
    assert_eq!(detect(b"").detection, Detection::Unrecognized);
    assert_eq!(
        detect(b"\xff\xfe not mail").detection,
        Detection::Unrecognized
    );
    let huge = format!(
        "Subject: big\r\n\r\nThis is the mail system at host x.\r\n{}<zed@example.net>: unknown user\r\n",
        "filler line\r\n".repeat(20_000)
    );
    assert_eq!(
        detect(huge.as_bytes()).detection,
        Detection::Unrecognized,
        "only the first part of a huge body is scanned"
    );
}
