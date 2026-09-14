//! Every string a template asks the catalog for must exist in every language.
use std::path::Path;

fn catalog_ids() -> Vec<String> {
    let templates = Path::new(env!("CARGO_MANIFEST_DIR")).join("templates");
    let mut ids = Vec::new();
    for entry in std::fs::read_dir(templates).expect("templates directory") {
        let text = std::fs::read_to_string(entry.expect("entry").path()).expect("template");
        for call in text.split("shell.t(\"").skip(1) {
            ids.push(call.split('"').next().expect("a closing quote").to_owned());
        }
    }
    ids.sort();
    ids.dedup();
    assert!(ids.len() > 50, "the templates translate their strings");
    ids
}

#[test]
fn every_template_string_is_translated_in_every_language() {
    for id in catalog_ids() {
        for language in listmngr_i18n::SUPPORTED {
            let message = listmngr_i18n::message(language, &id, &[]);
            assert_ne!(message, id, "{language}: {id} is missing from the catalog");
            assert!(!message.is_empty(), "{language}: {id} is empty");
        }
    }
}
