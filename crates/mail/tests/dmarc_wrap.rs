//! Mailman's `wrap_message` DMARC mitigation: the post travels whole inside
//! a list-addressed outer message (`mailman/handlers/dmarc.py`,
//! `wrap_message`).
use listmngr_core::{DmarcMitigateAction, MailingList};
use listmngr_mail::{cook_individual_post, cook_post, header_value};
use mail_parser::{MessageParser, MimeHeaders, PartType};

const RAW: &[u8] = b"From: =?UTF-8?B?QW5kcsOp?= <Author@elsewhere.invalid>\r\nSender: secretary@elsewhere.invalid\r\nTo: dev@example.invalid\r\nCc: Third <third@elsewhere.invalid>\r\nReply-To: Replies <reply@elsewhere.invalid>\r\nSubject: Quarterly figures\r\nDate: Mon, 01 Jan 2024 10:00:00 +0000\r\nMessage-ID: <post@elsewhere.invalid>\r\nIn-Reply-To: <earlier@elsewhere.invalid>\r\nReferences: <earlier@elsewhere.invalid>\r\nX-Spam-Score: 0.1\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\nS\xe1\xbb\x91 li\xe1\xbb\x87u qu\xc3\xbd.\r\n";

fn list(text: &str) -> MailingList {
    let mut list = MailingList::new("dev.example.invalid".parse().unwrap(), "Developers".into());
    list.dmarc.action = DmarcMitigateAction::WrapMessage;
    list.dmarc.unconditional = true;
    list.dmarc.dmarc_wrapped_message_text = text.into();
    list
}

/// The `From` mailbox as (name, address).
fn from_of(message: &mail_parser::Message<'_>) -> (String, String) {
    let from = message.from().unwrap().first().unwrap();
    (
        from.name().unwrap_or("").to_owned(),
        from.address().unwrap_or("").to_owned(),
    )
}

/// The outer message is the list's: From via the list, the author in
/// Reply-To as `munge_from` would put them, no Sender; Mailman's keepers
/// travel to it, the rest stay inside.
fn assert_outer_headers(cooked: &[u8]) {
    let outer = MessageParser::default().parse(cooked).unwrap();
    let (name, address) = from_of(&outer);
    assert_eq!(address, "dev@example.invalid");
    assert!(name.contains("André") && name.contains("via"), "{name}");
    assert_eq!(
        outer.reply_to().unwrap().first().unwrap().address(),
        Some("reply@elsewhere.invalid")
    );
    assert!(header_value(cooked, "Sender").is_none(), "{cooked:?}");
    assert!(header_value(cooked, "X-Spam-Score").is_none());
    assert_eq!(
        header_value(cooked, "Subject").as_deref(),
        Some("[dev] Quarterly figures")
    );
    assert_eq!(
        header_value(cooked, "Date").as_deref(),
        Some("Mon, 01 Jan 2024 10:00:00 +0000")
    );
    assert_eq!(
        header_value(cooked, "To").as_deref(),
        Some("dev@example.invalid")
    );
    assert_eq!(
        header_value(cooked, "Cc").as_deref(),
        Some("Third <third@elsewhere.invalid>")
    );
    assert_eq!(
        header_value(cooked, "In-Reply-To").as_deref(),
        Some("<earlier@elsewhere.invalid>")
    );
    assert_eq!(
        header_value(cooked, "References").as_deref(),
        Some("<earlier@elsewhere.invalid>")
    );
    assert_eq!(
        header_value(cooked, "List-Id").as_deref(),
        Some("<dev.example.invalid>")
    );
    assert_eq!(header_value(cooked, "Precedence").as_deref(), Some("list"));
    assert!(header_value(cooked, "X-Mailman-Version").is_some());
    assert_eq!(
        header_value(cooked, "X-BeenThere").as_deref(),
        Some("dev@example.invalid"),
        "the loop history stays readable on the outside"
    );
    assert!(header_value(cooked, "Message-ID-Hash").is_none());
    let outer_id = header_value(cooked, "Message-ID").unwrap();
    assert_ne!(outer_id, "<post@elsewhere.invalid>");
    assert!(outer_id.ends_with("@example.invalid>"), "{outer_id}");
    assert_eq!(header_value(cooked, "MIME-Version").as_deref(), Some("1.0"));
}

#[test]
fn wrap_message_puts_the_post_whole_inside_a_list_addressed_message() {
    let text = "The sender's domain publishes a DMARC policy that would reject this list's copy of their post, so the post is attached below unchanged.\n";
    let cooked = cook_individual_post(RAW, &list(text), "stable").unwrap();
    assert_outer_headers(&cooked);
    let outer = MessageParser::default().parse(&cooked).unwrap();
    // With a wrapped-message text: multipart/mixed, the text inline and
    // wrapped at seventy columns, then the post as an inline message/rfc822.
    let content_type = outer.content_type().unwrap();
    assert_eq!(
        (content_type.ctype(), content_type.subtype()),
        ("multipart", Some("mixed"))
    );
    let PartType::Multipart(parts) = &outer.parts[0].body else {
        panic!("not multipart");
    };
    assert_eq!(parts.len(), 2);
    let note = &outer.parts[parts[0] as usize];
    let PartType::Text(note_text) = &note.body else {
        panic!("first part is not text: {:?}", note.body);
    };
    assert_eq!(
        note_text.replace("\r\n", "\n"),
        "The sender's domain publishes a DMARC policy that would reject this\nlist's copy of their post, so the post is attached below unchanged."
    );
    assert!(
        note.content_disposition()
            .is_some_and(|d| d.ctype() == "inline")
    );
    let attached = &outer.parts[parts[1] as usize];
    assert!(
        attached
            .content_disposition()
            .is_some_and(|d| d.ctype() == "inline")
    );
    let PartType::Message(inner) = &attached.body else {
        panic!("second part is not a message: {:?}", attached.body);
    };
    // The post inside is the delivered copy before mitigation: its author,
    // its own Message-ID, the list's cooked headers and its body.
    assert_eq!(
        from_of(inner),
        ("André".to_owned(), "Author@elsewhere.invalid".to_owned())
    );
    assert_eq!(inner.message_id(), Some("post@elsewhere.invalid"));
    assert_eq!(
        inner.header_raw("Sender").map(str::trim),
        Some("dev-bounces@example.invalid")
    );
    assert_eq!(inner.subject(), Some("[dev] Quarterly figures"));
    assert_eq!(inner.body_text(0).as_deref(), Some("Số liệu quý.\r\n"));
    // The same post wraps to the same bytes.
    assert_eq!(
        cooked,
        cook_individual_post(RAW, &list(text), "stable").unwrap()
    );
}

#[test]
fn wrap_message_without_a_text_is_a_bare_message_rfc822() {
    let cooked = cook_individual_post(RAW, &list(""), "stable").unwrap();
    let outer = MessageParser::default().parse(&cooked).unwrap();
    assert_eq!(from_of(&outer).1, "dev@example.invalid");
    let content_type = outer.content_type().unwrap();
    assert_eq!(
        (content_type.ctype(), content_type.subtype()),
        ("message", Some("rfc822"))
    );
    assert!(
        outer
            .content_disposition()
            .is_some_and(|d| d.ctype() == "inline")
    );
    let PartType::Message(inner) = &outer.parts[0].body else {
        panic!("not a message: {:?}", outer.parts[0].body);
    };
    assert_eq!(from_of(inner).1, "Author@elsewhere.invalid");
    assert_eq!(inner.body_text(0).as_deref(), Some("Số liệu quý.\r\n"));
}

#[test]
fn wrap_message_is_delivery_only_conditional_and_yields_to_anonymity() {
    // The archive copy is never wrapped.
    let archived = cook_post(RAW, &list("note"), "stable").unwrap();
    assert_eq!(
        header_value(&archived, "From").as_deref(),
        Some("=?UTF-8?B?QW5kcsOp?= <Author@elsewhere.invalid>")
    );
    // A conditional list wraps only what the `dmarc-mitigation` rule tagged.
    let mut conditional = list("note");
    conditional.dmarc.unconditional = false;
    let cooked = cook_individual_post(RAW, &conditional, "stable").unwrap();
    assert_eq!(
        header_value(&cooked, "From").as_deref(),
        Some("=?UTF-8?B?QW5kcsOp?= <Author@elsewhere.invalid>")
    );
    let tagged = listmngr_mail::handlers::cook_with(
        listmngr_mail::handlers::Target::Out,
        RAW,
        &conditional,
        "stable",
        &listmngr_mail::handlers::Admission {
            dmarc_mitigate: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        MessageParser::default()
            .parse(&tagged)
            .unwrap()
            .content_type()
            .map(|c| c.ctype().to_owned()),
        Some("multipart".to_owned())
    );
    // An anonymous list already hides the author; nothing to wrap.
    let mut anonymous = list("note");
    anonymous.anonymous_list = true;
    let cooked = cook_individual_post(RAW, &anonymous, "stable").unwrap();
    assert_eq!(
        header_value(&cooked, "From").as_deref(),
        Some("dev@example.invalid")
    );
    assert!(
        !String::from_utf8_lossy(&cooked).contains("elsewhere.invalid"),
        "{cooked:?}"
    );
    // A post already from the list is aligned; it is not wrapped again.
    let aligned = b"From: Developers <dev@example.invalid>\r\nTo: dev@example.invalid\r\nMessage-ID: <own@example.invalid>\r\n\r\nbody\r\n";
    let cooked = cook_individual_post(aligned, &list("note"), "stable").unwrap();
    assert!(
        MessageParser::default()
            .parse(&cooked)
            .unwrap()
            .content_type()
            .is_none_or(|c| c.ctype() == "text"),
        "{cooked:?}"
    );
}
