use base64::Engine as _;
use listmngr_mail::cook_headers;
use mail_parser::MessageParser;

#[test]
fn long_unicode_subjects_respect_encoded_word_limits() {
    for padding in 0..80 {
        for prefix in ["[Việt] ", "[📬] ", "[e\u{301}] "] {
            let original = format!("{}{}", "a".repeat(padding), "😀".repeat(12));
            let original = format!("{} {original}", "x".repeat(300));
            let raw = format!("Subject: {original}\r\n\r\nbody");
            let cooked = cook_headers(raw.as_bytes(), Some(prefix), &[]).unwrap();
            assert_subject(&cooked, &format!("{prefix}{original}"));
            let headers = std::str::from_utf8(&cooked)
                .unwrap()
                .split("\r\n\r\n")
                .next()
                .unwrap();
            for line in headers.lines() {
                assert!(line.len() <= 78, "line length {}: {line}", line.len());
                for word in line
                    .split_ascii_whitespace()
                    .filter(|w| w.starts_with("=?"))
                {
                    let payload = word
                        .strip_prefix("=?utf-8?B?")
                        .unwrap()
                        .strip_suffix("?=")
                        .unwrap();
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(payload)
                        .unwrap();
                    assert!(
                        std::str::from_utf8(&bytes).is_ok(),
                        "each word must contain complete UTF-8"
                    );
                    assert!(
                        word.len() <= 75,
                        "encoded-word length {}: {word}",
                        word.len()
                    );
                }
            }
        }
    }
}

#[test]
fn folded_encoded_and_empty_subject_contracts() {
    for newline in ["\r\n", "\n"] {
        for (subject, original) in [
            ("Hello\r\n\tworld", "Hello world"),
            (
                "=?UTF-8?Q?Ti=E1=BA=BFng_Vi=E1=BB=87t?=\r\n =?UTF-8?B?8J+Tqw==?=",
                "Tiếng Việt📫",
            ),
            ("", ""),
            ("[Việt] Hello", "[Việt] Hello"),
        ] {
            let raw = format!(
                "sUbJeCt: {}{newline}Content-Type: text/plain{newline}{newline}body",
                subject.replace("\r\n", newline)
            );
            let prefix = "[Việt] ";
            let expected = if original.starts_with(prefix) {
                original.to_owned()
            } else {
                format!("{prefix}{original}")
            };
            let cooked = cook_headers(raw.as_bytes(), Some(prefix), &[]).unwrap();
            assert_subject(&cooked, &expected);
            assert_eq!(cook_headers(&cooked, Some(prefix), &[]).unwrap(), cooked);
            assert!(
                cooked.ends_with(
                    format!("Content-Type: text/plain{newline}{newline}body").as_bytes()
                )
            );
            if newline == "\n" {
                assert!(!cooked.contains(&b'\r'));
            }
            for prefix in [None, Some("")] {
                assert_eq!(
                    cook_headers(raw.as_bytes(), prefix, &[]).unwrap(),
                    raw.as_bytes()
                );
            }
        }
        let raw = format!("From: a@example.invalid{newline}{newline}body");
        assert_eq!(
            cook_headers(raw.as_bytes(), Some("[Việt] "), &[]).unwrap(),
            raw.as_bytes()
        );
    }
}

#[test]
fn long_unicode_prefix_repeated_cooking_is_stable() {
    let prefix = format!("[{}] ", "Việt 📬".repeat(100));
    let cooked = cook_headers(b"Subject: original\r\n\r\nbody", Some(&prefix), &[]).unwrap();
    assert_subject(&cooked, &format!("{prefix}original"));
    assert_eq!(cook_headers(&cooked, Some(&prefix), &[]).unwrap(), cooked);
}

#[test]
fn unicode_prefix_injection_is_rejected() {
    for prefix in ["[Việt]\r\nBcc: evil", "📬\n\nbody", "📬\rBcc: evil"] {
        assert!(cook_headers(b"Subject: Hello\r\n\r\nbody", Some(prefix), &[]).is_err());
    }
}

fn assert_subject(raw: &[u8], expected: &str) {
    let blank = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .or_else(|| raw.windows(2).position(|w| w == b"\n\n"))
        .unwrap();
    assert!(
        raw[..blank].is_ascii(),
        "generated header must be ASCII RFC2047"
    );
    let parsed = MessageParser::default().parse(raw).unwrap();
    assert_eq!(parsed.subject(), Some(expected));
    assert!(
        std::str::from_utf8(&raw[..blank])
            .unwrap()
            .contains("=?utf-8?")
    );
}

#[test]
fn repeat_cooking_does_not_duplicate_unicode_prefix() {
    let prefix = "[Tiếng Việt 📬] ";
    let cooked = cook_headers(b"Subject: Hello\r\n\r\nbody", Some(prefix), &[]).unwrap();
    let twice = cook_headers(&cooked, Some(prefix), &[]).unwrap();
    assert_subject(&twice, "[Tiếng Việt 📬] Hello");
    assert_eq!(twice, cooked);
}

#[test]
fn unicode_prefix_is_rfc2047_and_preserves_mime_bytes() {
    let raw = b"Subject: Hello\r\nMIME-Version: 1.0\r\nContent-Type: application/octet-stream\r\nContent-Transfer-Encoding: binary\r\n\r\n\x00\xff\xfe\r\n.dot\n";
    let cooked = cook_headers(raw, Some("[Tiếng Việt 📬] "), &[]).unwrap();
    assert_subject(&cooked, "[Tiếng Việt 📬] Hello");
    let mime = b"MIME-Version: 1.0\r\nContent-Type: application/octet-stream\r\nContent-Transfer-Encoding: binary\r\n\r\n\x00\xff\xfe\r\n.dot\n";
    assert!(cooked.ends_with(mime));
}
