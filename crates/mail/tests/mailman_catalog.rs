//! Mailman's translated templates, as `tests/compat/import_mailman_templates.py`
//! vendored them: only for templates whose English here is Mailman's, each
//! using no placeholder Mailman's English does not, and served in the
//! recipient's language with English behind it.
use listmngr_mail::templates::{MAX_BODY_BYTES, builtin, builtin_in, builtin_language};
use listmngr_mail::templates_mailman::{IMPORTED, LANGUAGES, english};
use std::collections::BTreeSet;

fn words(text: &str) -> Vec<&str> {
    text.split_whitespace().collect()
}

fn placeholders(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'$' {
            let braced = bytes.get(index + 1) == Some(&b'{');
            let start = index + 1 + usize::from(braced);
            let end = bytes[start..]
                .iter()
                .position(|byte| !(byte.is_ascii_alphanumeric() || *byte == b'_'))
                .map_or(bytes.len(), |offset| start + offset);
            if end > start {
                out.insert(text[start..end].to_owned());
            }
            index = end.max(index + 1);
        } else {
            index += 1;
        }
    }
    out
}

#[test]
fn every_imported_template_keeps_mailmans_english() {
    for name in IMPORTED {
        let ours = builtin(name).unwrap_or_else(|| panic!("{name} is not in the catalog"));
        let mailman = english(name).unwrap();
        if *name == "list:admin:notice:removal" {
            // "removed" where Mailman says "unsubscribed": the same event.
            assert_eq!(
                words(ours).join(" ").replace("removed", "unsubscribed"),
                words(mailman).join(" ")
            );
            continue;
        }
        assert_eq!(
            words(ours),
            words(mailman),
            "{name}: reworded here, so Mailman's translations no longer fit it"
        );
    }
}

#[test]
fn every_translation_uses_only_mailmans_english_placeholders() {
    assert!(LANGUAGES.len() >= 20, "{LANGUAGES:?}");
    let mut bodies = 0;
    for language in LANGUAGES {
        for name in IMPORTED {
            let Some(body) = listmngr_mail::templates_mailman::builtin(language, name) else {
                continue;
            };
            bodies += 1;
            let reference = english(name).unwrap();
            assert_ne!(body, reference, "{language}/{name} is only the English");
            assert!(body.len() <= MAX_BODY_BYTES, "{language}/{name}");
            let extra: Vec<_> = placeholders(body)
                .difference(&placeholders(reference))
                .cloned()
                .collect();
            assert!(extra.is_empty(), "{language}/{name} uses {extra:?}");
        }
    }
    assert!(bodies > 250, "{bodies}");
}

#[test]
fn the_recipients_language_picks_the_translation_with_english_behind_it() {
    let hold = "list:user:notice:hold";
    let french = builtin_in(hold, "fr").unwrap();
    assert!(french.contains("est en attente de validation"), "{french}");
    assert_eq!(builtin_language(hold, "fr-CA").unwrap().1, "fr");
    assert_eq!(builtin_language(hold, "pt-BR").unwrap().1, "pt-BR");
    assert_eq!(builtin_language(hold, "pt-PT").unwrap().1, "pt");
    assert_eq!(builtin_language(hold, "zh-Hans").unwrap().1, "zh-Hans");
    // Reworded here, so never Mailman's translation.
    let welcome = builtin_language("list:user:notice:welcome", "fr").unwrap();
    assert_eq!(
        welcome,
        (builtin("list:user:notice:welcome").unwrap(), "en")
    );
    // This project's own Vietnamese still wins for Vietnamese.
    assert_eq!(builtin_language(hold, "vi").unwrap().1, "vi");
    // A language nobody translated is English.
    assert_eq!(
        builtin_language(hold, "ar").unwrap(),
        (builtin(hold).unwrap(), "en")
    );
    assert_eq!(builtin_language(hold, "xx").unwrap().1, "en");
}
