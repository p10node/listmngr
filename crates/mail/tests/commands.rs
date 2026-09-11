use listmngr_mail::commands::{Command, parse};

#[test]
fn commands_are_bounded_single_operations_without_victim_mailbox_arguments() {
    for (line, expected) in [
        ("join", Command::Join),
        ("SUBSCRIBE", Command::Join),
        ("leave", Command::Leave),
        ("unsubscribe", Command::Leave),
        ("help", Command::Help),
    ] {
        let raw = format!("From: sender@example.com\r\nSubject: {line}\r\n\r\n");
        assert_eq!(parse(raw.as_bytes()), Some(expected));
    }
    for line in [
        "unsubscribe victim@example.com",
        "join victim@example.com",
        "help all",
        "confirm bad",
        "owner",
        "join leave",
    ] {
        let raw = format!("From: sender@example.com\r\nSubject: {line}\r\n\r\njoin\r\n");
        assert_eq!(parse(raw.as_bytes()), None, "{line}");
    }
    let token = "AbCdEfGhIjKlMnOpQrStUvWxYz0123456789_-AbCdE";
    let raw = format!("From: sender@example.com\r\nSubject: Re: confirm {token}\r\n\r\nleave\r\n");
    assert_eq!(parse(raw.as_bytes()), Some(Command::Confirm(token.into())));
    assert_eq!(
        parse(b"From: sender@example.com\r\n\r\n\r\nleave\r\n"),
        Some(Command::Leave)
    );
    let raw = format!(
        "From: sender@example.com\r\n\r\n{}join\r\n",
        "\r\n".repeat(20)
    );
    assert_eq!(parse(raw.as_bytes()), None);
}

#[test]
fn mime_carriers_select_real_plain_body_not_html_or_attachments() {
    for subtype in ["mixed", "alternative"] {
        for (parts, expected) in [
            (
                "Content-Type: text/html\r\n\r\n<p>join</p>\r\n--b\r\nContent-Type: text/plain\r\n\r\nleave",
                Some(Command::Leave),
            ),
            (
                "Content-Type: text/plain\r\n\r\njoin\r\n--b\r\nContent-Type: text/html\r\n\r\n<p>leave</p>",
                Some(Command::Join),
            ),
            ("Content-Type: text/html\r\n\r\n<p>join</p>", None),
            (
                "Content-Type: text/plain\r\nContent-Disposition: attachment; filename=command.txt\r\n\r\njoin",
                None,
            ),
            (
                "Content-Type: text/plain\r\nContent-Disposition: attachment; filename=command.txt\r\n\r\njoin\r\n--b\r\nContent-Type: text/plain\r\n\r\nleave",
                Some(Command::Leave),
            ),
        ] {
            let raw = format!(
                "From: sender@example.com\r\nMIME-Version: 1.0\r\nContent-Type: multipart/{subtype}; boundary=b\r\n\r\n--b\r\n{parts}\r\n--b--\r\n"
            );
            assert_eq!(parse(raw.as_bytes()), expected, "{subtype}: {parts}");
        }
    }
    for (headers, body, expected) in [
        (
            "Content-Type: text/plain\r\nContent-Transfer-Encoding: base64\r\n",
            "am9pbg==",
            Some(Command::Join),
        ),
        (
            "Content-Type: text/html\r\nSubject: leave\r\n",
            "<p>join</p>",
            Some(Command::Leave),
        ),
        (
            "Content-Type: text/plain\r\nSubject: invalid\r\n",
            "join",
            None,
        ),
        (
            "Content-Type: text/plain\r\nContent-Disposition: attachment; filename=command.txt\r\n",
            "join",
            None,
        ),
    ] {
        let raw = format!("From: sender@example.com\r\n{headers}\r\n{body}\r\n");
        assert_eq!(parse(raw.as_bytes()), expected, "{headers}");
    }
}

#[test]
fn html_body_is_not_an_email_command() {
    assert_eq!(
        parse(b"From: sender@example.com\r\nContent-Type: text/html\r\n\r\n<p>join</p>\r\n"),
        None
    );
}
