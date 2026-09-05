use listmngr_mail::{header_value, parse_message_id};

#[test]
fn rejects_ambiguous_malformed_and_unbounded_headers() {
    for raw in [
        b"Subject: none\r\n\r\n".as_slice(),
        b"Message-ID: <a@b>\nMessage-ID: <a@b>\n\n",
        b"Message-ID: a@b\n\n",
        b"Message-ID: <a@b> <c@d>\n\n",
        b"Message-ID: <a@b>\n",
        b"bad header\nMessage-ID: <a@b>\n\n",
        b" orphan\nMessage-ID: <a@b>\n\n",
        b"Bad : x\nMessage-ID: <a@b>\n\n",
        b"X: \0\nMessage-ID: <a@b>\n\n",
        b"Message-ID: <a@b>\rX: bad\n\n",
    ] {
        assert!(parse_message_id(raw).is_err(), "accepted {raw:?}");
    }
    let long_line = format!("X: {}\nMessage-ID: <a@b>\n\n", "x".repeat(999));
    assert!(parse_message_id(long_line.as_bytes()).is_err());
    let long_headers = format!("{}Message-ID: <a@b>\n\n", "X: y\n".repeat(14_000));
    assert!(parse_message_id(long_headers.as_bytes()).is_err());
}

#[test]
fn extracts_only_headers_without_decoding_binary_body() {
    assert_eq!(parse_message_id(b"Subject: hello\r\nmEsSaGe-Id:\r\n\t<Ab.c@example.com> \r\n\r\n\xff\0Message-ID: <evil@b>").unwrap(), "Ab.c@example.com");
    assert_eq!(parse_message_id(b"Message-ID: <a@b>\n\n").unwrap(), "a@b");
}

#[test]
fn header_value_finds_first_occurrence_and_unfolds_continuations() {
    let raw = b"Subject: Hello\r\n  World\r\nList-Post: <mailto:x@example.invalid>\r\n\r\nbody";
    assert_eq!(header_value(raw, "subject").unwrap(), "Hello World");
    assert_eq!(
        header_value(raw, "List-Post").unwrap(),
        "<mailto:x@example.invalid>"
    );
    assert_eq!(header_value(raw, "X-Missing"), None);
}

#[test]
fn header_value_is_first_occurrence_only_and_never_panics_on_garbage() {
    let raw = b"Subject: first\r\nSubject: second\r\n\r\nbody";
    assert_eq!(header_value(raw, "subject").unwrap(), "first");
    assert_eq!(header_value(b"not a header block", "subject"), None);
    assert_eq!(header_value(b"", "subject"), None);
    assert_eq!(header_value(b" leading-space\n\n", "subject"), None);
}
