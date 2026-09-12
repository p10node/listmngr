//! Mailman's personalization on a cooked copy: `$user_*` placeholders,
//! the `To:` rewrite of `personalize = full`, and header safety.
use listmngr_core::MailingList;
use listmngr_mail::personalize::{Recipient, placeholders, rewrite_to};
use listmngr_mail::templates::expand;

fn recipient() -> Recipient {
    Recipient {
        email: "alice@example.invalid".into(),
        delivered_to: "Alice@Example.invalid".into(),
        display_name: "Alice Nguyễn".into(),
        language: "vi".into(),
    }
}

#[test]
fn user_placeholders_extend_the_list_placeholders() {
    let list = MailingList::new("dev.example.invalid".parse().unwrap(), "Dev".into());
    let values = placeholders(&list, &recipient());
    assert_eq!(
        expand(
            "$listname $user_email $user_address $user_delivered_to $user_name $user_language",
            &values
        ),
        "dev@example.invalid alice@example.invalid alice@example.invalid Alice@Example.invalid Alice Nguyễn vi"
    );
    assert_eq!(
        expand("$member", &values),
        "=?utf-8?B?QWxpY2UgTmd1eeG7hW4=?= <alice@example.invalid>"
    );
    assert_eq!(
        expand("$user_optionsurl", &values),
        "$user_optionsurl",
        "unsupplied placeholders stay"
    );
}

#[test]
fn full_personalization_rewrites_to_and_nothing_else() {
    let raw = b"From: author@example.invalid\r\nTo: dev@example.invalid\r\nCc: cc@example.invalid\r\nTo: extra@example.invalid\r\nSubject: s\r\n\r\nbody\r\n";
    let out = rewrite_to(raw, &recipient()).unwrap();
    let fields = listmngr_mail::facts::header_fields(&out);
    let to: Vec<&str> = fields
        .iter()
        .filter(|(n, _)| n == "To")
        .map(|(_, v)| v.as_str())
        .collect();
    assert_eq!(
        to,
        ["=?utf-8?B?QWxpY2UgTmd1eeG7hW4=?= <alice@example.invalid>"]
    );
    assert!(
        fields
            .iter()
            .any(|(n, v)| n == "Cc" && v == "cc@example.invalid")
    );
    assert!(out.ends_with(b"\r\n\r\nbody\r\n"));
    let plain = Recipient {
        display_name: String::new(),
        ..recipient()
    };
    let out = rewrite_to(raw, &plain).unwrap();
    assert!(String::from_utf8_lossy(&out).contains("To: alice@example.invalid\r\n"));
    let hostile = Recipient {
        email: "x@example.invalid\r\nBcc: y".into(),
        ..recipient()
    };
    assert!(rewrite_to(raw, &hostile).is_err());
}
