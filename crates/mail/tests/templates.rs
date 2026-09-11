//! Template engine: Mailman names, built-in catalog, `$placeholder`
//! expansion with Python `string.Template` semantics, URI parsing and the
//! bounded loaders.
use listmngr_mail::templates::{
    NAMES, Placeholders, Source, builtin, expand, is_known_name, load, parse_uri,
};

#[test]
fn every_mailman_template_name_has_an_english_builtin() {
    for name in NAMES {
        assert!(is_known_name(name), "{name}");
        let body = builtin(name).unwrap_or_else(|| panic!("no built-in for {name}"));
        // Bodies are plain text; a stray CR would corrupt the generated MIME.
        assert!(!body.contains('\r'), "{name} carries CR");
    }
    assert!(NAMES.contains(&"list:user:notice:welcome"));
    assert!(NAMES.contains(&"list:admin:action:post"));
    assert!(NAMES.contains(&"list:user:notice:hold"));
    assert!(NAMES.contains(&"domain:admin:notice:new-list"));
    assert!(!is_known_name("list:user:notice:made-up"));
    assert!(builtin("list:user:notice:made-up").is_none());
}

#[test]
fn expansion_follows_python_string_template_semantics() {
    let values = Placeholders::new()
        .set("listname", "dev@example.invalid")
        .set("display_name", "Dev")
        .set("reasons", "too big; no subject");
    assert_eq!(
        expand("Post to $listname ($display_name)", &values),
        "Post to dev@example.invalid (Dev)"
    );
    assert_eq!(expand("${listname}!", &values), "dev@example.invalid!");
    assert_eq!(expand("costs $$5", &values), "costs $5");
    // Unknown placeholders are left untouched, as `safe_substitute` does.
    assert_eq!(
        expand("$unknown and ${also_unknown}", &values),
        "$unknown and ${also_unknown}"
    );
    // A lone `$` that starts no identifier is literal.
    assert_eq!(expand("100$ or $ alone", &values), "100$ or $ alone");
    assert_eq!(expand("$reasons", &values), "too big; no subject");
}

#[test]
fn expansion_never_re_expands_substituted_values() {
    let values = Placeholders::new().set("subject", "$listname");
    assert_eq!(expand("$subject", &values), "$listname");
}

#[test]
fn uris_parse_into_builtin_file_or_https_sources() {
    assert_eq!(
        parse_uri("mailman:///list:user:notice:welcome").unwrap(),
        Source::Builtin("list:user:notice:welcome".into())
    );
    assert_eq!(
        parse_uri("mailman:///vi/list:user:notice:welcome").unwrap(),
        Source::Builtin("list:user:notice:welcome".into()),
        "a language segment selects the same built-in"
    );
    assert_eq!(
        parse_uri("file:///etc/listmngr/templates/welcome.txt").unwrap(),
        Source::File("/etc/listmngr/templates/welcome.txt".into())
    );
    assert_eq!(
        parse_uri("https://templates.example.invalid/welcome.txt").unwrap(),
        Source::Https("https://templates.example.invalid/welcome.txt".into())
    );
    for bad in [
        "",
        "mailman:///",
        "mailman:///list:user:notice:made-up",
        "file://relative/path.txt",
        "file:///etc/../etc/passwd",
        "http://insecure.example.invalid/x.txt",
        "ftp://x.invalid/x",
        "https://",
        "https://x.invalid/a\nb",
        "mailman:///list:user:notice:welcome/../x",
    ] {
        assert!(parse_uri(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn file_sources_are_read_bounded_and_must_be_utf8() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("welcome.txt");
    std::fs::write(&path, "Hello $display_name\n").unwrap();
    assert_eq!(
        load(&Source::File(path.clone()), "en").unwrap(),
        "Hello $display_name\n"
    );
    std::fs::write(&path, vec![b'x'; 70_000]).unwrap();
    assert!(
        load(&Source::File(path.clone()), "en").is_err(),
        "over the size cap"
    );
    std::fs::write(&path, [0xff, 0xfe, b'a']).unwrap();
    assert!(load(&Source::File(path), "en").is_err(), "not UTF-8");
    assert!(load(&Source::File(dir.path().join("missing.txt")), "en").is_err());
}

#[test]
fn builtin_sources_serve_vietnamese_fall_back_to_english_and_https_is_refused() {
    let welcome = builtin("list:user:notice:welcome").unwrap();
    let vi = load(&Source::Builtin("list:user:notice:welcome".into()), "vi").unwrap();
    assert_ne!(vi, welcome);
    assert!(vi.contains("Chào mừng"));
    assert_eq!(
        load(&Source::Builtin("list:user:notice:welcome".into()), "vi-VN").unwrap(),
        vi,
        "a regional tag selects the same catalog"
    );
    assert_eq!(
        load(&Source::Builtin("list:user:notice:welcome".into()), "fr").unwrap(),
        welcome,
        "no French catalog, so English"
    );
    // Vietnamese bodies keep every placeholder the English body uses.
    for name in listmngr_mail::templates::NAMES {
        let en = builtin(name).unwrap();
        let vi = listmngr_mail::templates::builtin_in(name, "vi").unwrap();
        for word in en
            .split(|c: char| !(c.is_alphanumeric() || c == '$' || c == '_' || c == '{' || c == '}'))
        {
            if let Some(placeholder) = word.strip_prefix('$') {
                let placeholder = placeholder.trim_matches(['{', '}']);
                assert!(
                    vi.contains(&format!("${placeholder}"))
                        || vi.contains(&format!("${{{placeholder}}}")),
                    "{name}: vi lacks ${placeholder}"
                );
            }
        }
    }
    assert!(
        load(
            &Source::Https("https://templates.example.invalid/x.txt".into()),
            "en"
        )
        .is_err(),
        "remote templates are not fetched by this runtime"
    );
}
