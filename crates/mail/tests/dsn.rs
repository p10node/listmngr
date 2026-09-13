use listmngr_mail::dsn;
use std::fmt::Write;

fn message(body: &str) -> Vec<u8> {
    format!("Content-Type: multipart/report; report-type=delivery-status; boundary=r\r\n\r\n--r\r\nContent-Type: text/plain\r\n\r\nExplanation\r\n--r\r\nContent-Type: message/delivery-status\r\n\r\n{body}\r\n--r--\r\n").into_bytes()
}

#[test]
fn rejects_false_boundaries_that_hide_invalid_status_tail() {
    let valid = body("alice@example.invalid", "failed", "5.1.1");
    assert!(dsn::parse(&message(&valid)).is_some());
    for false_boundary in [
        "--r--garbage",
        "--r--",
        "\r\n--r--garbage",
        "\r\n--r-garbage",
    ] {
        let forged = format!("{valid}{false_boundary}\r\nStatus: 2.0.0");
        assert!(
            dsn::parse(&message(&forged)).is_none(),
            "accepted {false_boundary:?}"
        );
    }
}

#[test]
fn delimiter_padding_requires_space_or_tab_not_other_whitespace() {
    let valid =
        String::from_utf8(message(&body("alice@example.invalid", "failed", "5.1.1"))).unwrap();
    for padding in ["", " ", "\t", " \t"] {
        assert!(
            dsn::parse(
                valid
                    .replace("--r--\r\n", &format!("--r--{padding}\r\n"))
                    .as_bytes()
            )
            .is_some()
        );
    }
    for padding in ["\x0b", "\x0c", "\r"] {
        assert!(
            dsn::parse(
                valid
                    .replace("--r--\r\n", &format!("--r--{padding}\r\n"))
                    .as_bytes()
            )
            .is_none(),
            "accepted {padding:?}"
        );
    }
    assert!(dsn::parse(valid.replace("--r\r\n", "--r\n").as_bytes()).is_none());
}

#[test]
fn enforces_input_recipient_line_and_field_budgets() {
    let valid = body("alice@example.invalid", "failed", "5.1.1");
    let mut raw = message(&valid);
    raw.resize(256 * 1024, b' ');
    assert!(dsn::parse(&raw).is_some());
    raw.push(b' ');
    assert!(dsn::parse(&raw).is_none(), "raw budget bypass");
    let recipient = valid.split_once("\r\n\r\n").unwrap().1;
    let many = format!(
        "Reporting-MTA: dns; mx.example.invalid{}",
        format!("\r\n\r\n{recipient}").repeat(100)
    );
    assert_eq!(dsn::parse(&message(&many)).unwrap().len(), 100);
    assert!(dsn::parse(&message(&format!("{many}\r\n\r\n{recipient}"))).is_none());
    let line = format!("{valid}\r\nX: {}", "x".repeat(995));
    assert!(dsn::parse(&message(&line)).is_some());
    assert!(dsn::parse(&message(&format!("{line}x"))).is_none());
    let mut fields = valid.clone();
    for i in 0..29 {
        write!(fields, "\r\nX-{i}: value").unwrap();
    }
    assert!(dsn::parse(&message(&fields)).is_some());
    assert!(dsn::parse(&message(&format!("{fields}\r\nX-Overflow: value"))).is_none());
    let oversized = format!("{valid}\r\nX-Padding: a{}", "\r\n a".repeat(16_384));
    assert!(dsn::parse(&message(&oversized)).is_none());
}

#[test]
fn rejects_ambiguous_mime_and_unterminated_report() {
    let raw =
        String::from_utf8(message(&body("alice@example.invalid", "failed", "5.1.1"))).unwrap();
    assert!(dsn::parse(raw.as_bytes()).is_some());
    for bad in [
        raw.replace("--r--\r\n", ""),
        raw.replace("boundary=r", "boundary=r; boundary=other"),
        raw.replace(
            "report-type=delivery-status",
            "report-type=delivery-status; report-type=other",
        ),
        raw.replace(
            "Content-Type: message/delivery-status",
            "Content-Type: message/delivery-status\r\nContent-Type: text/plain",
        ),
        raw.replace(
            "Content-Type: message/delivery-status",
            "Content-Type: message/delivery-status\r\nContent-Transfer-Encoding: base64",
        ),
        raw.replace(
            "Content-Type: message/delivery-status",
            "Content-Type: message/delivery-status\r\nContent-Transfer-Encoding: invented",
        ),
        raw.replace(
            "Content-Type: text/plain",
            "Content-Type: message/delivery-status",
        ),
    ] {
        assert!(
            dsn::parse(bad.as_bytes()).is_none(),
            "accepted ambiguous MIME"
        );
    }
}

#[test]
fn status_budget_is_inclusive_and_bad_tail_never_returns_partial_recipients() {
    let valid = body("alice@example.invalid", "failed", "5.1.1");
    let mut padded = format!("{valid}\r\nX-Padding: x");
    while 64 * 1024 - padded.len() > 600 {
        write!(padded, "\r\n {}", "x".repeat(500)).unwrap();
    }
    padded.push_str("\r\n ");
    padded.push_str(&"x".repeat(64 * 1024 - padded.len()));
    assert_eq!(padded.len(), 64 * 1024);
    assert!(dsn::parse(&message(&padded)).is_some());
    padded.push('x');
    assert!(dsn::parse(&message(&padded)).is_none());
    let second = body("bob@example.invalid", "delayed", "4.2.2");
    let second = second.split_once("\r\n\r\n").unwrap().1;
    let multi = format!("{valid}\r\n\r\n{second}");
    let reports = dsn::parse(&message(&multi)).unwrap();
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[1].final_recipient, "bob@example.invalid");
    assert_eq!(reports[1].action, "delayed");
    assert_eq!(reports[1].status, "4.2.2");
    assert!(dsn::parse(&message(&format!("{multi}\r\nStatus: 5.0.0"))).is_none());
}

#[test]
fn optional_returned_message_is_not_scanned_for_recipient_authority() {
    let raw =
        String::from_utf8(message(&body("alice@example.invalid", "failed", "5.1.1"))).unwrap();
    for content_type in ["message/rfc822", "text/rfc822-headers"] {
        let with_original = raw.replace("--r--\r\n", &format!("--r\r\nContent-Type: {content_type}\r\n\r\nFrom: original@example.invalid\r\nFinal-Recipient: rfc822; forged@example.invalid\r\n\r\nOriginal body\r\n--r--\r\n"));
        let reports = dsn::parse(with_original.as_bytes()).unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].final_recipient, "alice@example.invalid");
    }
}

fn recipient_block(recipient: &str, action: &str, status: &str) -> String {
    format!("Final-Recipient: rfc822; {recipient}\r\nAction: {action}\r\nStatus: {status}")
}

fn body(recipient: &str, action: &str, status: &str) -> String {
    format!(
        "Reporting-MTA: dns; mx.example.invalid\r\n\r\nFinal-Recipient: rfc822; {recipient}\r\nAction: {action}\r\nStatus: {status}"
    )
}

#[test]
fn rejects_ambiguous_fields_and_invalid_required_values() {
    let valid = body("alice@example.invalid", "failed", "5.1.1");
    assert!(dsn::parse(&message(&valid)).is_some());
    for invalid in [
        valid.replace("Action: failed", "Action: failed\r\naCTION: delivered"),
        valid.replace("Status: 5.1.1", "Status: 5.1.1\r\nStatus: 2.0.0"),
        valid.replace("Reporting-MTA: dns; mx.example.invalid", "X-Untrusted: yes"),
        valid.replace("Action: failed", "Action: invented"),
        valid.replace("Status: 5.1.1", "Status: 5.1"),
        valid.replace("Status: 5.1.1", "Status: 5.+1.1"),
        valid.replace("Status: 5.1.1", "Status: 5.1000.1"),
        valid.replace("Status: 5.1.1", "Status: 3.1.1"),
        valid.replace("alice@example.invalid", ""),
        valid.replace("rfc822; alice", "unknown; alice"),
        valid.replace(
            "Status: 5.1.1",
            "Status: 5.1.1\r\nX-Text: injected\x00value",
        ),
    ] {
        assert!(
            dsn::parse(&message(&invalid)).is_none(),
            "accepted invalid field matrix entry"
        );
    }
}

#[test]
fn preserves_distinct_claims_and_accepts_rfc_failed_temporary_status() {
    // RFC3464 §2.3.3 explicitly permits failed + 4.x when retry was abandoned.
    for (action, status) in [
        ("failed", "4.0.0"),
        ("delayed", "4.2.2"),
        ("delivered", "2.0.0"),
        ("relayed", "2.0.0"),
        ("expanded", "2.0.0"),
    ] {
        let raw = message(&body("Bob@example.invalid", action, status));
        let report = dsn::parse(&raw).unwrap();
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].final_recipient, "Bob@example.invalid");
        assert_eq!(report[0].action, action);
        assert_eq!(report[0].status, status);
    }
    let folded = body("alice@example.invalid", "FAILED", "5.1.1")
        .replace("rfc822; alice", "rfc822;\r\n\talice");
    let report = dsn::parse(&message(&folded)).unwrap();
    assert_eq!(report[0].final_recipient, "alice@example.invalid");
    assert_eq!(report[0].action, "failed");
}

#[test]
fn the_original_envelope_id_is_read_when_the_reporting_mta_echoes_it() {
    let with_envid = format!(
        "Reporting-MTA: dns; mx.example.invalid\r\nOriginal-Envelope-Id: lm-ENVID-1234\r\n\r\n{}",
        recipient_block("alice@example.invalid", "failed", "5.1.1")
    );
    let report = dsn::parse_report(&message(&with_envid)).unwrap();
    assert_eq!(
        report.original_envelope_id.as_deref(),
        Some("lm-ENVID-1234")
    );
    assert_eq!(report.recipients.len(), 1);
    let without =
        dsn::parse_report(&message(&body("alice@example.invalid", "failed", "5.1.1"))).unwrap();
    assert_eq!(without.original_envelope_id, None);
    let oversized = format!(
        "Reporting-MTA: dns; mx.example.invalid\r\nOriginal-Envelope-Id: {}\r\n\r\n{}",
        "x".repeat(101),
        recipient_block("alice@example.invalid", "failed", "5.1.1")
    );
    assert_eq!(
        dsn::parse_report(&message(&oversized))
            .unwrap()
            .original_envelope_id,
        None,
        "an oversized id is dropped, not truncated"
    );
}
