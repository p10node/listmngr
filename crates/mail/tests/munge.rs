use listmngr_core::{DmarcMitigateAction, MailingList};
use listmngr_mail::{cook_individual_post as cook_post, header_value};

fn list() -> MailingList {
    MailingList::new("dev.example.invalid".parse().unwrap(), "Developers".into())
}
fn enabled() -> MailingList {
    let mut list = list();
    list.dmarc.action = DmarcMitigateAction::MungeFrom;
    list.dmarc.unconditional = true;
    list
}
#[test]
fn munge_repeated_publication_is_stable_and_keeps_multi_reply_targets() {
    let raw = b"From: \"Doe, Jane\" <Jane@elsewhere.invalid>\nReply-To: \"Reply, One\" <one@elsewhere.invalid>, two@elsewhere.invalid\nMessage-ID: <p@elsewhere.invalid>\n\nbody\x00\xff";
    let first = cook_post(raw, &enabled(), "stable").unwrap();
    let second = cook_post(&first, &enabled(), "stable").unwrap();
    assert_eq!(first, second);
    let parsed = mail_parser::MessageParser::default().parse(&first).unwrap();
    let replies: Vec<_> = parsed
        .reply_to()
        .unwrap()
        .iter()
        .filter_map(|a| a.address())
        .collect();
    assert_eq!(replies, ["one@elsewhere.invalid", "two@elsewhere.invalid"]);
    assert!(first.ends_with(b"\n\nbody\x00\xff"));
}

#[test]
fn munge_is_opt_in_attributes_author_and_preserves_body_and_reply() {
    let raw = b"From: =?UTF-8?B?QW5kcsOp?= <Author@elsewhere.invalid>\r\nSender: secretary@elsewhere.invalid\r\nReply-To: Replies <reply@elsewhere.invalid>\r\nMessage-ID: <post@elsewhere.invalid>\r\nContent-Type: application/octet-stream\r\n\r\n\x00\xffbody\r\n.dot";
    // Without mitigation the author survives cooking untouched (the pipeline
    // still adds its list headers, so compare the identity, not the bytes).
    let default = cook_post(raw, &list(), "stable").unwrap();
    assert_eq!(
        header_value(&default, "From").as_deref(),
        Some("=?UTF-8?B?QW5kcsOp?= <Author@elsewhere.invalid>")
    );
    assert_eq!(
        header_value(&default, "Sender").as_deref(),
        Some("secretary@elsewhere.invalid")
    );
    assert!(default.ends_with(b"\r\n\r\n\x00\xffbody\r\n.dot"));
    let cooked = cook_post(raw, &enabled(), "stable").unwrap();
    assert!(cooked.ends_with(b"\r\n\r\n\x00\xffbody\r\n.dot"));
    let parsed = mail_parser::MessageParser::default()
        .parse(&cooked)
        .unwrap();
    let from = parsed.from().unwrap().first().unwrap();
    assert_eq!(from.address(), Some("dev@example.invalid"));
    assert!(from.name().unwrap().contains("André"));
    assert!(from.name().unwrap().contains("Author@elsewhere.invalid"));
    assert!(from.name().unwrap().contains("via"));
    assert!(header_value(&cooked, "Sender").is_none());
    assert_eq!(
        parsed.reply_to().unwrap().first().unwrap().address(),
        Some("reply@elsewhere.invalid")
    );
}
#[test]
fn munge_reply_falls_back_to_author_and_anonymity_wins() {
    for reply in [
        "",
        "Reply-To: invalid\r\n",
        "Reply-To: a@elsewhere.invalid\r\nReply-To: b@elsewhere.invalid\r\n",
    ] {
        let raw = format!(
            "From: Author <author@elsewhere.invalid>\r\n{reply}Message-ID: <p@elsewhere.invalid>\r\n\r\nbody"
        );
        let cooked = cook_post(raw.as_bytes(), &enabled(), "stable").unwrap();
        let parsed = mail_parser::MessageParser::default()
            .parse(&cooked)
            .unwrap();
        assert_eq!(
            parsed.reply_to().unwrap().first().unwrap().address(),
            Some("author@elsewhere.invalid")
        );
        let mut anonymous = enabled();
        anonymous.anonymous_list = true;
        let cooked = cook_post(raw.as_bytes(), &anonymous, "stable").unwrap();
        assert!(!String::from_utf8_lossy(&cooked).contains("elsewhere.invalid"));
        assert_eq!(
            header_value(&cooked, "From").as_deref(),
            Some("dev@example.invalid")
        );
    }
}
#[test]
fn munge_fails_closed_for_ambiguous_or_unsafe_authors() {
    for from in [
        "",
        "From: invalid\r\n",
        "From: a@elsewhere.invalid, b@elsewhere.invalid\r\n",
        "From: a@elsewhere.invalid\r\nFrom: b@elsewhere.invalid\r\n",
        "From: A\x00 <a@elsewhere.invalid>\r\n",
        "From: =?UTF-8?B?QQ0KQmNjOiBldmls?= <a@elsewhere.invalid>\r\n",
        "From: valid@elsewhere.invalid garbage\r\n",
    ] {
        let raw = format!("{from}Message-ID: <p@elsewhere.invalid>\r\n\r\nbody");
        assert!(
            cook_post(raw.as_bytes(), &enabled(), "stable").is_err(),
            "{from:?}"
        );
        let mut anonymous = enabled();
        anonymous.anonymous_list = true;
        assert!(cook_post(raw.as_bytes(), &anonymous, "stable").is_ok());
    }
}
