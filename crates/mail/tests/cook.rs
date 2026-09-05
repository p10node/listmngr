use listmngr_mail::cook_headers;

#[test]
fn prepends_subject_prefix_once_and_preserves_binary_body() {
    let raw = b"Message-ID: <a@example.invalid>\r\nSubject: Hello\r\n\r\nbody\x00\xff\r\n.dot\r\n";
    let cooked = cook_headers(raw, Some("[list] "), &[]).unwrap();
    let text = String::from_utf8_lossy(&cooked);
    assert!(text.starts_with("Message-ID: <a@example.invalid>\r\nSubject: [list] Hello\r\n"));
    assert!(cooked.ends_with(b"\r\n\r\nbody\x00\xff\r\n.dot\r\n"));

    // Re-cooking must not double the prefix.
    let recooked = cook_headers(&cooked, Some("[list] "), &[]).unwrap();
    assert_eq!(recooked, cooked);
}

#[test]
fn leaves_missing_subject_untouched_and_only_appends_additions() {
    let raw = b"Message-ID: <b@example.invalid>\r\n\r\nbody";
    let cooked = cook_headers(
        raw,
        Some("[list] "),
        &[("List-Id".into(), "list.example.invalid".into())],
    )
    .unwrap();
    let text = String::from_utf8_lossy(&cooked);
    assert_eq!(
        text,
        "Message-ID: <b@example.invalid>\r\nList-Id: list.example.invalid\r\n\r\nbody"
    );
}

#[test]
fn appends_additions_before_the_blank_line_using_lf_style_when_the_header_block_has_no_cr() {
    let raw = b"Message-ID: <c@example.invalid>\nSubject: Hi\n\nbody\n";
    let cooked = cook_headers(
        raw,
        Some("[x] "),
        &[("List-Post".into(), "<mailto:x@example.invalid>".into())],
    )
    .unwrap();
    assert_eq!(
        cooked,
        b"Message-ID: <c@example.invalid>\nSubject: [x] Hi\nList-Post: <mailto:x@example.invalid>\n\nbody\n"
    );
}

#[test]
fn rejects_input_without_a_header_body_boundary() {
    assert!(cook_headers(b"Message-ID: <d@example.invalid>", None, &[]).is_err());
}

#[test]
fn empty_prefix_does_not_rewrite_subject() {
    let raw = b"Subject: Plain\r\n\r\nbody";
    let cooked = cook_headers(raw, Some(""), &[]).unwrap();
    assert_eq!(cooked, raw);
}

#[test]
fn rejects_a_subject_prefix_carrying_a_header_injection() {
    let raw = b"Message-ID: <e@example.invalid>\r\nSubject: Hello\r\n\r\nbody";
    // A malicious/misconfigured subject_prefix must never be able to splice
    // a fake header (or end the header block early) into the cooked message.
    for prefix in [
        "[x]\r\nBcc: evil@attacker.invalid\r\nSubject: ",
        "[x]\nBcc: evil@attacker.invalid\nSubject: ",
        "[x]\r\n\r\nInjected-Body: yes",
    ] {
        let result = cook_headers(raw, Some(prefix), &[]);
        assert!(result.is_err(), "prefix {prefix:?} must be rejected");
    }
}

#[test]
fn rejects_addition_header_names_or_values_carrying_crlf() {
    let raw = b"Message-ID: <f@example.invalid>\r\n\r\nbody";
    assert!(
        cook_headers(
            raw,
            None,
            &[("List-Id\r\nBcc".into(), "evil@attacker.invalid".into())]
        )
        .is_err()
    );
    assert!(
        cook_headers(
            raw,
            None,
            &[(
                "List-Id".into(),
                "list.example.invalid\r\nBcc: evil@attacker.invalid".into()
            )]
        )
        .is_err()
    );
    assert!(cook_headers(raw, None, &[("List-Id".into(), "ok\nBcc: evil".into())]).is_err());
    // A colon inside the header name is also structurally unsafe.
    assert!(cook_headers(raw, None, &[("Li:st-Id".into(), "ok".into())]).is_err());
}

#[test]
fn accepts_safe_prefix_and_additions_unchanged() {
    let raw = b"Message-ID: <g@example.invalid>\r\nSubject: Hi\r\n\r\nbody";
    let cooked = cook_headers(
        raw,
        Some("[safe-list] "),
        &[("List-Id".into(), "safe.example.invalid".into())],
    )
    .unwrap();
    let text = String::from_utf8_lossy(&cooked);
    assert!(text.contains("Subject: [safe-list] Hi"));
    assert!(text.contains("List-Id: safe.example.invalid"));
}
