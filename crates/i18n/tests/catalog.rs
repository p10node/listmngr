//! Language negotiation and message lookup with English fallback.
use listmngr_i18n::{SUPPORTED, is_supported, message, negotiate};

#[test]
fn supported_languages_are_english_and_vietnamese() {
    assert_eq!(SUPPORTED, &["en", "vi"]);
    assert!(is_supported("en"));
    assert!(is_supported("vi"));
    assert!(!is_supported("fr"));
}

#[test]
fn negotiation_matches_exact_then_primary_tag_then_falls_back_to_english() {
    assert_eq!(negotiate("vi"), "vi");
    assert_eq!(negotiate("vi-VN"), "vi");
    assert_eq!(negotiate("VI"), "vi");
    assert_eq!(negotiate("en-GB"), "en");
    assert_eq!(negotiate("fr"), "en");
    assert_eq!(negotiate(""), "en");
    assert_eq!(negotiate("not a tag!!"), "en");
}

#[test]
fn choose_takes_the_first_preference_a_catalog_serves() {
    use listmngr_i18n::{choose, supported_match};
    assert_eq!(supported_match("fr"), None);
    assert_eq!(supported_match("vi-VN"), Some("vi"));
    assert_eq!(choose(["fr", "vi", "en"]), "vi");
    assert_eq!(choose(["", "de"]), "en");
    assert_eq!(choose(["en-US", "vi"]), "en");
    assert_eq!(choose([]), "en");
}

#[test]
fn messages_resolve_in_the_requested_language_with_arguments() {
    let en = message(
        "en",
        "notice-welcome-subject",
        &[("display_name", "Dev Chat")],
    );
    assert_eq!(en, "Welcome to the \"Dev Chat\" mailing list");
    let vi = message(
        "vi",
        "notice-welcome-subject",
        &[("display_name", "Dev Chat")],
    );
    assert_eq!(vi, "Chào mừng bạn đến với hộp thư chung \"Dev Chat\"");
}

#[test]
fn unknown_languages_and_missing_messages_fall_back_without_panicking() {
    assert_eq!(
        message("ar", "notice-welcome-subject", &[("display_name", "X")]),
        "Welcome to the \"X\" mailing list"
    );
    // A message only English has still resolves for Vietnamese callers.
    assert!(!message("vi", "notice-help-subject", &[]).is_empty());
    // An unknown message id yields the id itself rather than an empty subject.
    assert_eq!(message("en", "no-such-message", &[]), "no-such-message");
}

/// Every placeable any catalog uses, so a message resolves rather than falling
/// back to its own id.
const ARGUMENTS: &[(&str, &str)] = &[
    ("display_name", "x"),
    ("listname", "x"),
    ("member", "x"),
    ("action", "x"),
    ("sender", "x"),
    ("token", "x"),
    ("count", "1"),
    ("list", "x"),
    ("name", "x"),
    ("email", "x"),
    ("address", "x"),
    ("mode", "x"),
    ("status", "x"),
    ("site_name", "x"),
    ("state", "x"),
    ("position", "1"),
    ("policy", "x"),
    ("reason", "x"),
    ("held", "1"),
    ("requests", "1"),
    ("done", "1"),
    ("skipped", "0"),
    ("role", "x"),
    ("month", "2024-01"),
    ("query", "x"),
    ("tag", "x"),
    ("category", "x"),
];

#[test]
fn every_english_message_resolves_with_the_known_placeables() {
    for id in listmngr_i18n::message_ids() {
        for language in listmngr_i18n::SUPPORTED {
            assert_ne!(
                message(language, id, ARGUMENTS),
                id,
                "{language}: {id} does not resolve"
            );
        }
    }
}

#[test]
fn every_english_message_has_a_vietnamese_translation() {
    for id in listmngr_i18n::message_ids() {
        if id == "confirm-subject" || id.starts_with("web-language-") {
            // Deliberately identical: a machine-parsed subject, and language
            // names shown as endonyms in every language.
            continue;
        }
        assert_ne!(
            message("vi", id, ARGUMENTS),
            message("en", id, ARGUMENTS),
            "{id} is not translated"
        );
    }
}

#[test]
fn the_confirmation_subject_is_literal_in_every_catalog() {
    for language in listmngr_i18n::SUPPORTED {
        assert_eq!(
            listmngr_i18n::message(language, "confirm-subject", &[("token", "abc123")]),
            "confirm abc123",
            "{language}: reply-to-confirm parses this subject verbatim"
        );
    }
}

#[test]
fn the_receipt_subject_translates_the_action_word() {
    assert_eq!(
        listmngr_i18n::message("vi", "notice-receipt-subject", &[("action", "join")]),
        "Yêu cầu tham gia hộp thư chung đã hoàn tất"
    );
    assert_eq!(
        listmngr_i18n::message("vi", "notice-receipt-subject", &[("action", "leave")]),
        "Yêu cầu rời khỏi hộp thư chung đã hoàn tất"
    );
    assert_eq!(
        listmngr_i18n::message("en", "notice-receipt-subject", &[("action", "join")]),
        "List join request completed"
    );
}

#[test]
fn the_interface_speaks_its_own_languages_and_notices_speak_mailmans_too() {
    // Mailman's translations never change what the interface negotiates.
    assert_eq!(listmngr_i18n::supported_match("fr"), None);
    assert_eq!(negotiate("fr"), "en");
    let languages: Vec<_> = listmngr_i18n::notice_languages().collect();
    assert_eq!(&languages[..2], ["en", "vi"]);
    assert!(languages.len() >= 25, "{languages:?}");
    assert_eq!(listmngr_i18n::notice_match("fr-CA"), Some("fr"));
    assert_eq!(listmngr_i18n::notice_match("pt-BR"), Some("pt-BR"));
    assert_eq!(listmngr_i18n::notice_match("ar"), None);
    assert_eq!(listmngr_i18n::negotiate_notice("ar"), "en");
    assert_eq!(
        listmngr_i18n::choose_notice(["ar", "de", "vi"]),
        "de",
        "the first preference a notice can be written in"
    );
    let codes: Vec<_> = listmngr_i18n::NOTICE_LANGUAGE_OPTIONS
        .iter()
        .map(|(code, _)| *code)
        .collect();
    assert_eq!(codes, languages);
}

#[test]
fn a_notice_subject_comes_from_mailmans_translation_with_english_behind_it() {
    assert_eq!(
        message(
            "fr",
            "notice-hold-subject",
            &[("listname", "dev@example.invalid")]
        ),
        "Votre message à dev@example.invalid attend la validation d'un modérateur"
    );
    assert_eq!(
        message("de-AT", "notice-no-subject", &[]),
        message("de", "notice-no-subject", &[])
    );
    // Mailman has no such subject, so a French notice gets English.
    assert_eq!(
        message("fr", "notice-help-subject", &[]),
        message("en", "notice-help-subject", &[])
    );
    // The interface's strings stay English in a notice-only language.
    assert_eq!(
        message("fr", "web-title-error", &[]),
        message("en", "web-title-error", &[])
    );
}

#[test]
fn every_notice_language_resolves_its_subjects_and_its_name() {
    let args = [
        ("display_name", "Dev"),
        ("listname", "dev@example.invalid"),
        ("sender", "a@example.invalid"),
        ("member", "b@example.invalid"),
        ("count", "3"),
        ("action", "join"),
        ("site_name", "Example"),
    ];
    let subjects: Vec<_> = listmngr_i18n::message_ids()
        .into_iter()
        .filter(|id| {
            id.starts_with("notice-") && id.ends_with("-subject") || *id == "notice-no-subject"
        })
        .collect();
    for language in listmngr_i18n::notice_languages() {
        for id in &subjects {
            let text = message(language, id, &args);
            assert!(!text.is_empty() && text != *id, "{language}/{id}");
            assert!(!text.contains('{'), "{language}/{id}: {text}");
        }
    }
    for (code, id) in listmngr_i18n::NOTICE_LANGUAGE_OPTIONS {
        for interface in SUPPORTED {
            let name = message(interface, id, &[]);
            assert_ne!(name, *id, "{code} has no name in {interface}");
        }
    }
    assert_eq!(message("vi", "web-language-ja", &[]), "日本語");
}
