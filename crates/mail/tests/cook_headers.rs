//! Mailman's `cook-headers` and `rfc-2369` handlers, complete: the full
//! RFC 2369 set with its switches and archive URLs, `Sender` rewriting,
//! `Reply-To` munging policies, `Message-ID-Hash` and `X-Mailman-Version`.
use listmngr_core::{ArchivePolicy, MailingList, ReplyToMunging};
use listmngr_mail::handlers::{Target, cook_for, cook_for_site};
use listmngr_mail::header_value;

const BASE: &str = "https://lists.example.invalid/";

fn list() -> MailingList {
    let mut list = MailingList::new("dev.example.invalid".parse().unwrap(), "Dev".into());
    list.subject_prefix = String::new();
    list
}

fn message(extra: &str) -> Vec<u8> {
    format!(
        "From: Alice <alice@elsewhere.invalid>\r\nTo: dev@example.invalid\r\nSubject: hi\r\nMessage-ID: <post@elsewhere.invalid>\r\n{extra}\r\nbody\r\n"
    )
    .into_bytes()
}

/// Every raw (unfolded) value of the header, in order.
fn all(raw: &[u8], name: &str) -> Vec<String> {
    listmngr_mail::facts::header_fields(raw)
        .into_iter()
        .filter(|(field, _)| field.eq_ignore_ascii_case(name))
        .map(|(_, value)| value)
        .collect()
}

#[test]
fn the_full_rfc_2369_set_is_emitted_with_archive_urls_when_a_base_url_is_known() {
    let mut described = list();
    described.description = "Dev Chat".into();
    let out = cook_for_site(Target::Out, &message(""), &described, "id", Some(BASE)).unwrap();
    for (name, expected) in [
        // `formataddr` quotes only phrases carrying specials.
        ("List-Id", "Dev Chat <dev.example.invalid>"),
        (
            "List-Help",
            "<mailto:dev-request@example.invalid?subject=help>",
        ),
        ("List-Subscribe", "<mailto:dev-join@example.invalid>"),
        ("List-Unsubscribe", "<mailto:dev-leave@example.invalid>"),
        ("List-Post", "<mailto:dev@example.invalid>"),
        ("List-Owner", "<mailto:dev-owner@example.invalid>"),
        (
            "List-Archive",
            "<https://lists.example.invalid/archives/list/dev.example.invalid/>",
        ),
        (
            "Archived-At",
            "<https://lists.example.invalid/archives/list/dev.example.invalid/message/DJGKBPHQEC5YJUQMQGRJ6YQT7IA5QREH/>",
        ),
        ("Precedence", "list"),
    ] {
        assert_eq!(all(&out, name), [expected], "{name}");
    }
    let hash = header_value(&out, "Message-ID-Hash").unwrap();
    assert_eq!(
        hash,
        listmngr_mail::message_id_hash("<post@elsewhere.invalid>").unwrap()
    );
    assert_eq!(header_value(&out, "X-Message-ID-Hash").unwrap(), hash);
    assert!(
        header_value(&out, "X-Mailman-Version")
            .unwrap()
            .starts_with("listmngr "),
        "{out:?}"
    );

    let mut quoted = list();
    quoted.description = "Dev: chat, etc.".into();
    let out = cook_for(Target::Out, &message(""), &quoted, "id").unwrap();
    assert_eq!(
        all(&out, "List-Id"),
        ["\"Dev: chat, etc.\" <dev.example.invalid>"]
    );
    let mut unicode = list();
    unicode.description = "Hộp thư".into();
    let out = cook_for(Target::Out, &message(""), &unicode, "id").unwrap();
    assert!(
        all(&out, "List-Id")[0].starts_with("=?utf-8?B?"),
        "{:?}",
        all(&out, "List-Id")
    );

    // A list without a description keeps the bare bracketed id; no base URL
    // means no archive headers.
    let out = cook_for(Target::Out, &message(""), &list(), "id").unwrap();
    assert_eq!(all(&out, "List-Id"), ["<dev.example.invalid>"]);
    assert!(all(&out, "List-Archive").is_empty());
    assert!(all(&out, "Archived-At").is_empty());

    // A list that never archives advertises no archive even with a base URL.
    let mut never = list();
    never.archive_policy = ArchivePolicy::Never;
    let out = cook_for_site(Target::Out, &message(""), &never, "id", Some(BASE)).unwrap();
    assert!(all(&out, "List-Archive").is_empty());
    assert!(all(&out, "Archived-At").is_empty());
}

#[test]
fn every_consumer_sees_the_same_list_headers() {
    let list = list();
    let raw = message("");
    let reference = all(
        &cook_for_site(Target::Out, &raw, &list, "id", Some(BASE)).unwrap(),
        "List-Id",
    );
    for target in [Target::Archive, Target::Digest] {
        let out = cook_for_site(target, &raw, &list, "id", Some(BASE)).unwrap();
        assert_eq!(all(&out, "List-Id"), reference, "{target:?}");
        assert_eq!(all(&out, "Archived-At").len(), 1, "{target:?}");
    }
}

#[test]
fn the_rfc_2369_switches_drop_the_set_or_mark_posting_closed() {
    let mut silent = list();
    silent.alter_messages.include_rfc2369_headers = false;
    let out = cook_for(
        Target::Out,
        &message("List-Post: <mailto:old@example.invalid>\r\n"),
        &silent,
        "id",
    )
    .unwrap();
    let text = String::from_utf8_lossy(&out);
    assert!(!text.to_ascii_lowercase().contains("\r\nlist-"), "{text}");
    assert_eq!(all(&out, "Precedence"), ["list"], "cook-headers still runs");

    let mut announce = list();
    announce.alter_messages.allow_list_posts = false;
    let out = cook_for(Target::Out, &message(""), &announce, "id").unwrap();
    assert_eq!(all(&out, "List-Post"), ["NO"]);
    assert_eq!(all(&out, "List-Help").len(), 1);
}

#[test]
fn inbound_list_headers_are_replaced_not_duplicated() {
    let raw = message(
        "List-Id: <other.example.invalid>\r\nList-Help: <mailto:other-request@example.invalid>\r\nArchived-At: <https://other.invalid/x>\r\nX-Mailman-Version: 3.3.9\r\n",
    );
    let out = cook_for_site(Target::Out, &raw, &list(), "id", Some(BASE)).unwrap();
    assert_eq!(all(&out, "List-Id"), ["<dev.example.invalid>"]);
    assert_eq!(all(&out, "List-Help").len(), 1);
    assert_eq!(all(&out, "Archived-At").len(), 1);
    assert!(all(&out, "Archived-At")[0].contains("lists.example.invalid"));
    assert_eq!(all(&out, "X-Mailman-Version").len(), 1);
    assert!(all(&out, "X-Mailman-Version")[0].starts_with("listmngr "));
}

#[test]
fn the_sender_header_is_rewritten_to_the_bounces_address_unless_switched_off() {
    let raw = message("Sender: secretary@elsewhere.invalid\r\n");
    let out = cook_for(Target::Out, &raw, &list(), "id").unwrap();
    assert_eq!(all(&out, "Sender"), ["dev-bounces@example.invalid"]);
    let out = cook_for(Target::Out, &message(""), &list(), "id").unwrap();
    assert_eq!(all(&out, "Sender"), ["dev-bounces@example.invalid"]);

    let mut keep = list();
    keep.alter_messages.include_sender_header = false;
    let out = cook_for(Target::Out, &raw, &keep, "id").unwrap();
    assert_eq!(all(&out, "Sender"), ["secretary@elsewhere.invalid"]);
    let out = cook_for(Target::Out, &message(""), &keep, "id").unwrap();
    assert!(all(&out, "Sender").is_empty());
}

fn with_policy(policy: ReplyToMunging, address: &str, strip: bool) -> MailingList {
    let mut list = list();
    list.alter_messages.reply_goes_to_list = policy;
    list.alter_messages.reply_to_address = address.into();
    list.alter_messages.first_strip_reply_to = strip;
    list
}

#[test]
fn reply_to_follows_the_munging_policy_and_first_strip() {
    let original = message("Reply-To: Alice Alt <alt@elsewhere.invalid>\r\n");
    let cases: [(ReplyToMunging, &str, bool, Vec<&str>); 9] = [
        (
            ReplyToMunging::NoMunging,
            "",
            false,
            vec!["Alice Alt <alt@elsewhere.invalid>"],
        ),
        (ReplyToMunging::NoMunging, "", true, vec![]),
        (
            ReplyToMunging::PointToList,
            "",
            false,
            vec!["Alice Alt <alt@elsewhere.invalid>, dev@example.invalid"],
        ),
        (
            ReplyToMunging::PointToList,
            "",
            true,
            vec!["dev@example.invalid"],
        ),
        (
            ReplyToMunging::ExplicitHeader,
            "replies@example.invalid",
            false,
            vec!["Alice Alt <alt@elsewhere.invalid>, replies@example.invalid"],
        ),
        (
            ReplyToMunging::ExplicitHeader,
            "replies@example.invalid",
            true,
            vec!["replies@example.invalid"],
        ),
        (
            ReplyToMunging::ExplicitHeader,
            "",
            false,
            vec!["Alice Alt <alt@elsewhere.invalid>"],
        ),
        (
            ReplyToMunging::ExplicitHeaderOnly,
            "replies@example.invalid",
            false,
            vec!["replies@example.invalid"],
        ),
        (ReplyToMunging::ExplicitHeaderOnly, "", false, vec![]),
    ];
    for (policy, address, strip, expected) in cases {
        let out = cook_for(
            Target::Out,
            &original,
            &with_policy(policy, address, strip),
            "id",
        )
        .unwrap();
        assert_eq!(
            all(&out, "Reply-To"),
            expected,
            "{policy:?} {address:?} strip={strip}"
        );
    }
    // Without an inbound Reply-To the policies add only their own address.
    let out = cook_for(
        Target::Out,
        &message(""),
        &with_policy(ReplyToMunging::PointToList, "", false),
        "id",
    )
    .unwrap();
    assert_eq!(all(&out, "Reply-To"), ["dev@example.invalid"]);
    let out = cook_for(
        Target::Out,
        &message(""),
        &with_policy(ReplyToMunging::NoMunging, "", false),
        "id",
    )
    .unwrap();
    assert!(all(&out, "Reply-To").is_empty());
}

#[test]
fn reply_to_addresses_are_deduplicated_case_insensitively_and_names_encoded() {
    let raw = message("Reply-To: DEV@Example.invalid, alt@elsewhere.invalid\r\n");
    let out = cook_for(
        Target::Out,
        &raw,
        &with_policy(ReplyToMunging::PointToList, "", false),
        "id",
    )
    .unwrap();
    assert_eq!(
        all(&out, "Reply-To"),
        ["DEV@Example.invalid, alt@elsewhere.invalid"]
    );

    let raw = message("Reply-To: =?utf-8?B?Tmd1eeG7hW4gVsSDbg==?= <van@elsewhere.invalid>\r\n");
    let out = cook_for(
        Target::Out,
        &raw,
        &with_policy(ReplyToMunging::PointToList, "", false),
        "id",
    )
    .unwrap();
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("Reply-To: =?utf-8?B?"),
        "non-ASCII names travel RFC 2047 encoded: {text}"
    );
    assert!(text.is_ascii(), "the header block stays ASCII");
    let parsed = mail_parser::MessageParser::default().parse(&out).unwrap();
    let decoded: Vec<(Option<String>, String)> = parsed
        .reply_to()
        .unwrap()
        .iter()
        .map(|addr| {
            (
                addr.name().map(str::to_owned),
                addr.address().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        decoded,
        [
            (Some("Nguyễn Văn".into()), "van@elsewhere.invalid".into()),
            (None, "dev@example.invalid".into())
        ]
    );
}

#[test]
fn anonymous_lists_point_replies_at_the_list_before_the_policy_runs() {
    let mut anonymous = with_policy(ReplyToMunging::PointToList, "", false);
    anonymous.anonymous_list = true;
    let out = cook_for(
        Target::Out,
        &message("Reply-To: alt@elsewhere.invalid\r\n"),
        &anonymous,
        "id",
    )
    .unwrap();
    assert_eq!(all(&out, "Reply-To"), ["dev@example.invalid"]);
    assert_eq!(all(&out, "From"), ["dev@example.invalid"]);
    assert!(all(&out, "Sender").contains(&"dev-bounces@example.invalid".to_owned()));
}

#[test]
fn a_personalized_copy_gets_the_rfc_8058_pair_with_https_first() {
    let cooked = cook_for(Target::Out, &message(""), &list(), "id").unwrap();
    let url = "https://lists.example.invalid/unsubscribe/dev.example.invalid?token=T";
    let out = listmngr_mail::personalize::one_click_unsubscribe(&cooked, &list(), url).unwrap();
    assert_eq!(
        all(&out, "List-Unsubscribe"),
        [format!("<{url}>, <mailto:dev-leave@example.invalid>")]
    );
    assert_eq!(
        all(&out, "List-Unsubscribe-Post"),
        ["List-Unsubscribe=One-Click"]
    );
    assert_eq!(all(&out, "List-Id").len(), 1, "other headers untouched");

    let mut silent = list();
    silent.alter_messages.include_rfc2369_headers = false;
    let cooked = cook_for(Target::Out, &message(""), &silent, "id").unwrap();
    let out = listmngr_mail::personalize::one_click_unsubscribe(&cooked, &silent, url).unwrap();
    assert!(all(&out, "List-Unsubscribe").is_empty());
    assert!(all(&out, "List-Unsubscribe-Post").is_empty());
    assert!(
        listmngr_mail::personalize::one_click_unsubscribe(&cooked, &list(), "https://x\r\nBcc: y")
            .is_err(),
        "a URL cannot splice headers"
    );
}
