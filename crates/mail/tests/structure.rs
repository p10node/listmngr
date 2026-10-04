//! The intake's structure ceilings: header fields, MIME parts and nesting
//! depth are measured as documented, and `check` refuses exactly over each
//! configured number.
use listmngr_mail::structure::{Excess, Limits, check, measure};
use std::fmt::Write as _;

const HEAD: &str = "From: author@example.invalid\r\nTo: dev@example.invalid\r\nSubject: shape\r\nMessage-ID: <shape@example.invalid>\r\n";

/// A text body wrapped in `levels - 1` nested `multipart/mixed` bodies:
/// the outer body is depth 1, so the leaf sits at `levels`.
fn nested(levels: u32) -> Vec<u8> {
    fn body(level: u32, levels: u32) -> String {
        if level == levels {
            return "Content-Type: text/plain\r\n\r\nleaf\r\n".to_owned();
        }
        let boundary = format!("b{level}");
        format!(
            "Content-Type: multipart/mixed; boundary=\"{boundary}\"\r\n\r\n--{boundary}\r\n{}--{boundary}--\r\n",
            body(level + 1, levels)
        )
    }
    format!("{HEAD}{}", body(1, levels)).into_bytes()
}

/// A flat `multipart/mixed` of `n` text parts.
fn flat(n: u32) -> Vec<u8> {
    let mut message = format!("{HEAD}Content-Type: multipart/mixed; boundary=\"flat\"\r\n\r\n");
    for i in 0..n {
        write!(
            message,
            "--flat\r\nContent-Type: text/plain\r\n\r\npart {i}\r\n"
        )
        .unwrap();
    }
    message.push_str("--flat--\r\n");
    message.into_bytes()
}

/// `n` header fields, every other one folded onto a second line.
fn fields(n: u32) -> Vec<u8> {
    let mut message = String::new();
    for i in 0..n {
        write!(message, "X-Field-{i}: value\r\n").unwrap();
        if i % 2 == 0 {
            message.push_str("\tcontinued\r\n");
        }
    }
    message.push_str("\r\nbody\r\n");
    message.into_bytes()
}

#[test]
fn a_plain_message_is_one_part_one_deep() {
    let m = measure(format!("{HEAD}\r\nhello\r\n").as_bytes());
    assert_eq!((m.header_count, m.mime_parts, m.mime_depth), (4, 1, 1));
}

#[test]
fn nesting_counts_every_multipart_and_its_leaf() {
    for levels in [2, 3, 20, 21] {
        let m = measure(&nested(levels));
        assert_eq!(m.mime_depth, levels, "{levels}");
        assert_eq!(m.mime_parts, levels, "{levels}");
    }
}

#[test]
fn a_flat_multipart_counts_the_container_and_every_child() {
    let m = measure(&flat(999));
    assert_eq!((m.mime_parts, m.mime_depth), (1000, 2));
    let m = measure(&flat(1000));
    assert_eq!((m.mime_parts, m.mime_depth), (1001, 2));
}

#[test]
fn a_folded_field_counts_once_and_the_body_never_counts() {
    assert_eq!(measure(&fields(500)).header_count, 500);
    assert_eq!(measure(&fields(501)).header_count, 501);
    // No blank line: the whole thing is header.
    assert_eq!(measure(b"A: 1\r\nB: 2\r\n").header_count, 2);
    assert_eq!(measure(b"").header_count, 0);
}

#[test]
fn a_nested_message_goes_one_deeper_than_its_part() {
    let inner = "Content-Type: multipart/alternative; boundary=\"alt\"\r\n\r\n--alt\r\nContent-Type: text/plain\r\n\r\nplain\r\n--alt\r\nContent-Type: text/html\r\n\r\n<p>html</p>\r\n--alt--\r\n";
    let raw = format!(
        "{HEAD}Content-Type: multipart/mixed; boundary=\"outer\"\r\n\r\n--outer\r\nContent-Type: text/plain\r\n\r\nsee attached\r\n--outer\r\nContent-Type: message/rfc822\r\n\r\nFrom: inner@example.invalid\r\nSubject: inner\r\n{inner}--outer--\r\n"
    );
    let m = measure(raw.as_bytes());
    // outer (1) → text (2), message part (2) → inner body (3) → plain, html (4)
    assert_eq!(m.mime_depth, 4);
    assert_eq!(m.mime_parts, 6);
}

#[test]
fn check_refuses_exactly_over_each_ceiling_in_order() {
    let limits = Limits::default();
    assert!(check(&nested(20), &limits).is_ok());
    assert_eq!(
        check(&nested(21), &limits),
        Err(Excess::Depth { depth: 21, max: 20 })
    );
    assert!(check(&flat(999), &limits).is_ok());
    assert_eq!(
        check(&flat(1000), &limits),
        Err(Excess::Parts {
            count: 1001,
            max: 1000
        })
    );
    assert!(check(&fields(500), &limits).is_ok());
    assert_eq!(
        check(&fields(501), &limits),
        Err(Excess::Headers {
            count: 501,
            max: 500
        })
    );
    // Headers are judged first, then parts, then depth.
    let tight = Limits {
        max_header_count: 1,
        max_mime_parts: 1,
        max_mime_depth: 1,
    };
    assert!(matches!(
        check(&nested(3), &tight),
        Err(Excess::Headers { count: 5, max: 1 })
    ));
    let parts_first = Limits {
        max_header_count: 100,
        max_mime_parts: 2,
        max_mime_depth: 1,
    };
    assert!(matches!(
        check(&nested(3), &parts_first),
        Err(Excess::Parts { count: 3, max: 2 })
    ));
    assert_eq!(
        Excess::Depth { depth: 21, max: 20 }.to_string(),
        "MIME nesting 21 deep, at most 20"
    );
}

#[test]
fn unreadable_bytes_are_one_part_one_deep() {
    let m = measure(b"\xff\xfe\x00");
    assert_eq!((m.mime_parts, m.mime_depth), (1, 1));
}
