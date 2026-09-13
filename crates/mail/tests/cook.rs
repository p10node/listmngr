use listmngr_mail::cook_headers;

#[test]
fn empty_filtered_header_block_preserves_crlf_for_new_fields() {
    assert_eq!(
        cook_headers(
            b"\r\nbody",
            None,
            &[("From".into(), "list@example.invalid".into())]
        )
        .unwrap(),
        b"From: list@example.invalid\r\n\r\nbody"
    );
}

#[test]
fn strips_legacy_approval_aliases_and_list_control_metadata() {
    let raw = b"Approve: secret\nX-Approved: secret\nx-approve: secret\n continuation-secret\nX-List-Received-Date: old\nX-Mailman-Approved-At: old\n\nbody";
    assert_eq!(cook_headers(raw, None, &[]).unwrap(), b"\nbody");
}

#[test]
fn redistribution_removes_private_and_obsolete_control_fields() {
    let raw = b"From: author@example.invalid\r\nbCc: hidden\r\n\tsecret\r\nResent-Bcc: hidden2\r\nAPPROVED: password\r\n approved-continuation\r\nList-Post: <mailto:old@example.invalid>\r\nLiSt-Archive: <https://old.invalid>\r\nList-Unsubscribe-Post: List-Unsubscribe=One-Click\r\nPrecedence: bulk\r\nReturn-Path: <old@example.invalid>\r\nX-Approval: password\r\nX-Confirm: token\r\nX-List-Administrivia: yes\r\nX-Mailman-Version: old\r\nMIME-Version: 1.0\r\nContent-Type: application/octet-stream\r\n\r\n\x00\xffbody";
    let cooked = cook_headers(
        raw,
        None,
        &[("List-Post".into(), "<mailto:new@example.invalid>".into())],
    )
    .unwrap();
    assert_eq!(cooked, b"From: author@example.invalid\r\nMIME-Version: 1.0\r\nContent-Type: application/octet-stream\r\nList-Post: <mailto:new@example.invalid>\r\n\r\n\x00\xffbody");
}

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

mod approved_line {
    use listmngr_mail::{cook_post, facts::strip_approved_line};

    fn list() -> listmngr_core::MailingList {
        listmngr_core::MailingList::new("dev.example.invalid".parse().unwrap(), "Dev".into())
    }

    #[test]
    fn strips_a_leading_approved_line_from_a_plain_body_only_once() {
        let raw =
            b"From: a@example.invalid\r\nSubject: hi\r\n\r\nApproved: s3cret\r\nreal body\r\n";
        let stripped = strip_approved_line(raw);
        assert_eq!(
            stripped,
            b"From: a@example.invalid\r\nSubject: hi\r\n\r\nreal body\r\n"
        );
        // Idempotent: a body that merely starts with the word is left alone.
        let again = strip_approved_line(&stripped);
        assert_eq!(again, stripped);
    }

    #[test]
    fn strips_every_spelling_case_insensitively_and_keeps_lf_style() {
        for prefix in ["approve:", "APPROVED:", "X-Approved:", "x-approve:"] {
            let raw = format!("From: a@example.invalid\n\n{prefix} key here\nbody\n");
            assert_eq!(
                strip_approved_line(raw.as_bytes()),
                b"From: a@example.invalid\n\nbody\n",
                "{prefix}"
            );
        }
    }

    #[test]
    fn leaves_a_body_whose_first_line_is_blank_or_ordinary_untouched() {
        for raw in [
            &b"From: a@example.invalid\r\n\r\n\r\nApproved: late\r\n"[..],
            &b"From: a@example.invalid\r\n\r\nApproved by the board\r\n"[..],
            &b"From: a@example.invalid\r\n\r\n"[..],
            &b"From: a@example.invalid\r\n"[..],
        ] {
            assert_eq!(strip_approved_line(raw), raw);
        }
    }

    #[test]
    fn strips_inside_the_first_text_plain_part_of_a_multipart_message() {
        let raw = b"From: a@example.invalid\r\n\
Content-Type: multipart/mixed; boundary=b\r\n\
\r\n\
--b\r\n\
Content-Type: text/plain\r\n\
\r\n\
Approved: s3cret\r\n\
real body\r\n\
--b\r\n\
Content-Type: application/octet-stream\r\n\
\r\n\
Approved: not text, untouched\r\n\
--b--\r\n";
        let stripped = strip_approved_line(raw);
        let text = String::from_utf8_lossy(&stripped);
        assert!(!text.contains("Approved: s3cret"), "{text}");
        assert!(text.contains("real body"));
        assert!(text.contains("Approved: not text, untouched"));
        assert_eq!(stripped.len(), raw.len() - b"Approved: s3cret\r\n".len());
    }

    #[test]
    fn does_not_touch_an_encoded_part_it_cannot_safely_rewrite() {
        let raw = b"From: a@example.invalid\r\n\
Content-Type: text/plain\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
QXBwcm92ZWQ6IHNlY3JldA0K\r\n";
        assert_eq!(strip_approved_line(raw), raw);
    }

    #[test]
    fn cook_post_strips_the_approved_line_and_the_approved_headers_together() {
        let raw = b"From: a@example.invalid\r\nApproved: s3cret\r\nSubject: hi\r\nMessage-ID: <m@example.invalid>\r\n\r\nApproved: s3cret\r\nreal body\r\n";
        let cooked = cook_post(raw, &list(), "identity").unwrap();
        let text = String::from_utf8_lossy(&cooked);
        assert!(!text.contains("s3cret"), "{text}");
        assert!(text.ends_with("\r\n\r\nreal body\r\n"), "{text}");
    }
}
