//! The digest issues compared with real Mailman 3.3.10 output.
//!
//! `fixtures/digests/mailman-3.3.10-{plain,mime}.eml` were produced by
//! Mailman core's own `RFC1153Digester` and `MIMEDigester` from the inputs
//! in `fixtures/digests/inputs.json` (`tests/compat/
//! generate_mailman_digest.py`, run under Mailman's test configuration).
//! This test builds the same issue here from the same inputs and asserts
//! the same bytes, after the volatile headers (`Date`, `Message-ID`, MIME
//! boundaries) are set aside and two Mailman quirks are named.
use listmngr_core::{DeliveryMode, ListId};
use listmngr_mail::digest::{Digest, build};
use listmngr_mail::templates::{Placeholders, builtin, expand};
use mail_parser::{MessageParser, MimeHeaders, PartType};
use serde::Deserialize;
use std::fmt::Write as _;

#[derive(Deserialize)]
struct Inputs {
    mailman: String,
    list_id: String,
    display_name: String,
    subject_prefix: String,
    volume: i32,
    number: i64,
    header: String,
    footer: String,
    posts: Vec<String>,
}

fn inputs() -> Inputs {
    serde_json::from_str(include_str!("fixtures/digests/inputs.json")).unwrap()
}

const MAILMAN_PLAIN: &[u8] = include_bytes!("fixtures/digests/mailman-3.3.10-plain.eml");
const MAILMAN_MIME: &[u8] = include_bytes!("fixtures/digests/mailman-3.3.10-mime.eml");

/// The masthead Mailman expanded: the same default template, the same
/// placeholders (`wrap` at seventy columns is applied by the renderer).
fn masthead(list: &ListId, display_name: &str) -> String {
    let values = Placeholders::new()
        .set("display_name", display_name)
        .set("listname", list.posting_address())
        .set(
            "request_email",
            format!("{}-request@{}", list.list_name(), list.mail_host()),
        )
        .set(
            "owner_email",
            format!("{}-owner@{}", list.list_name(), list.mail_host()),
        );
    expand(builtin("list:member:digest:masthead").unwrap(), &values)
}

fn ours(inputs: &Inputs, mode: DeliveryMode) -> Vec<u8> {
    let list: ListId = inputs.list_id.parse().unwrap();
    let posts: Vec<Vec<u8>> = inputs.posts.iter().map(|p| p.as_bytes().to_vec()).collect();
    let digest = Digest {
        masthead: masthead(&list, &inputs.display_name),
        list,
        display_name: inputs.display_name.clone(),
        subject_prefix: inputs.subject_prefix.clone(),
        volume: inputs.volume,
        number: inputs.number,
        mode,
        timestamp: 1_704_105_600,
        header: inputs.header.clone(),
        footer: inputs.footer.clone(),
        messages: posts.iter().map(Vec::as_slice).collect(),
    };
    build(&digest).unwrap()
}

fn lf(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// Mailman's plain issue with its two quirks named and set aside: its
/// scrubber mangles an 8-bit UTF-8 body (`Nội dung …`) where this
/// renderer keeps the text, and it joins the "next part" note straight
/// onto the last body line.
fn mailman_plain_body() -> String {
    let parsed = MessageParser::default().parse(MAILMAN_PLAIN).unwrap();
    let body = lf(&parsed.body_text(0).unwrap());
    assert!(
        body.contains("N\\u1ed9i dung \\u0111\\u1ea7u ti"),
        "the quirk this test names is present"
    );
    let body = body.replacen(
        "N\\u1ed9i dung \\u0111\\u1ea7u ti\u{fffd}n.",
        "Nội dung đầu tiên.",
        1,
    );
    body.replacen(
        "Different body-------------- next part",
        "Different body\n-------------- next part",
        1,
    )
}

/// `From`, `To`, `Reply-To` and `Subject` as sent; a bare address and
/// one in angle brackets are the same mailbox.
fn same_envelope_headers(ours: &mail_parser::Message<'_>, mailman: &mail_parser::Message<'_>) {
    let bare = |value: Option<&str>| {
        value.map(|v| {
            v.trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_owned()
        })
    };
    for name in ["From", "To", "Reply-To", "Subject"] {
        assert_eq!(
            bare(ours.header_raw(name)),
            bare(mailman.header_raw(name)),
            "{name}"
        );
    }
}

fn diff(label: &str, ours: &str, theirs: &str) {
    if ours == theirs {
        return;
    }
    let mut report = String::new();
    for (index, (a, b)) in ours.lines().zip(theirs.lines()).enumerate() {
        if a != b {
            let _ = writeln!(
                report,
                "line {}:\n  ours:    {a:?}\n  mailman: {b:?}",
                index + 1
            );
        }
    }
    let (n, m) = (ours.lines().count(), theirs.lines().count());
    if n != m {
        let _ = writeln!(report, "line counts: ours {n}, mailman {m}");
    }
    panic!(
        "{label} differs from Mailman {}:\n{report}\n--- ours ---\n{ours}\n--- mailman ---\n{theirs}",
        inputs().mailman
    );
}

#[test]
fn the_plain_text_issue_is_mailmans_rfc_1153_issue() {
    let inputs = inputs();
    let raw = ours(&inputs, DeliveryMode::PlaintextDigests);
    let parsed = MessageParser::default().parse(&raw).unwrap();
    let mailman = MessageParser::default().parse(MAILMAN_PLAIN).unwrap();
    same_envelope_headers(&parsed, &mailman);
    diff(
        "the plain issue",
        &lf(&parsed.body_text(0).unwrap()),
        &mailman_plain_body(),
    );
}

/// One text part of a digest: its `Content-Description` and its text.
fn text_parts(message: &mail_parser::Message<'_>) -> Vec<(String, String)> {
    let root = &message.parts[0];
    let PartType::Multipart(children) = &root.body else {
        panic!("not multipart");
    };
    children
        .iter()
        .filter_map(|id| {
            let part = &message.parts[*id as usize];
            match &part.body {
                PartType::Text(text) => Some((
                    part.content_description().unwrap_or("").to_owned(),
                    lf(text.as_ref()),
                )),
                _ => None,
            }
        })
        .collect()
}

/// The posts inside the `multipart/digest` part, as (message id, subject,
/// `From` mailbox, Mailman's `Message` count header, body text). Mailman's third quirk is named here: it
/// re-encodes an 8-bit `From` header as one RFC 2047 word holding the
/// name and the address together, so the mailbox is compared as the text
/// `Name <address>` rather than as a parsed address.
fn digest_posts(
    message: &mail_parser::Message<'_>,
) -> Vec<(String, String, String, String, String)> {
    let root = &message.parts[0];
    let PartType::Multipart(children) = &root.body else {
        panic!("not multipart");
    };
    let digest = children
        .iter()
        .map(|id| &message.parts[*id as usize])
        .find(|part: &&mail_parser::MessagePart<'_>| {
            part.content_type()
                .is_some_and(|ct| ct.ctype() == "multipart" && ct.subtype() == Some("digest"))
        })
        .expect("a multipart/digest part");
    let PartType::Multipart(posts) = &digest.body else {
        panic!("digest is not multipart");
    };
    posts
        .iter()
        .map(|id| match &message.parts[*id as usize].body {
            PartType::Message(inner) => (
                inner.message_id().unwrap_or("").to_owned(),
                inner.subject().unwrap_or("").to_owned(),
                inner
                    .from()
                    .and_then(|f| f.first())
                    .map(|a| match (a.name(), a.address()) {
                        (Some(name), Some(address)) => format!("{name} <{address}>"),
                        (name, address) => name.or(address).unwrap_or("").to_owned(),
                    })
                    .unwrap_or_default(),
                inner
                    .header_raw("Message")
                    .map(|count| count.trim().to_owned())
                    .unwrap_or_default(),
                lf(&inner.body_text(0).unwrap_or_default()),
            ),
            other => panic!("not a message/rfc822 part: {other:?}"),
        })
        .collect()
}

#[test]
fn the_mime_issue_has_mailmans_parts_in_mailmans_order() {
    let inputs = inputs();
    let raw = ours(&inputs, DeliveryMode::MimeDigests);
    let parsed = MessageParser::default().parse(&raw).unwrap();
    let mailman = MessageParser::default().parse(MAILMAN_MIME).unwrap();
    same_envelope_headers(&parsed, &mailman);
    let ours_parts = text_parts(&parsed);
    let theirs = text_parts(&mailman);
    assert_eq!(
        ours_parts
            .iter()
            .map(|(d, _)| d.as_str())
            .collect::<Vec<_>>(),
        theirs.iter().map(|(d, _)| d.as_str()).collect::<Vec<_>>(),
        "the text parts and their descriptions, in order"
    );
    for ((description, mine), (_, theirs)) in ours_parts.iter().zip(&theirs) {
        diff(&format!("MIME part {description:?}"), mine, theirs);
    }
    assert_eq!(digest_posts(&parsed), digest_posts(&mailman));
}

/// `wrap` against `mailman.utilities.string.wrap` on the same inputs
/// (Mailman 3.3.10, Python 3.12): a word longer than the column is broken
/// at it, a sentence end gets two spaces, tabs expand to eight columns and
/// an indented paragraph is copied as it is.
#[test]
fn wrap_is_mailmans_wrap() {
    use listmngr_mail::digest::wrap;
    assert_eq!(
        wrap(
            "Message-ID: <CAExample+abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOP@mail.gmail.com>",
            70
        ),
        "Message-ID: <CAExample+abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJK\nLMNOP@mail.gmail.com>"
    );
    assert_eq!(
        wrap(
            "Subject: One sentence ends here. Then a second one follows it! And a third? Yes.",
            70
        ),
        "Subject: One sentence ends here.  Then a second one follows it!  And a\nthird?  Yes."
    );
    assert_eq!(
        wrap(
            "12. A very long subject line that certainly needs to be wrapped at sixty-five columns for the contents",
            65
        ),
        "12. A very long subject line that certainly needs to be wrapped\nat sixty-five columns for the contents"
    );
    assert_eq!(
        wrap(
            "First\tcolumn\tafter tabs and   several   spaces\nnext line\n\n  indented stays\n  as is\nback to filled text that is long enough to need wrapping at seventy columns ok\n",
            70
        ),
        "First   column  after tabs and   several   spaces next line\n\n  indented stays\n  as is\nback to filled text that is long enough to need wrapping at seventy\ncolumns ok"
    );
}
