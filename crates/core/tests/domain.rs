use listmngr_core::{
    Config, DeliveryMode, DeliveryStatus, ListId, MailingList, MemberRole, ModerationAction,
    Preferences, SubscriptionMode, builtin_styles,
};

#[test]
fn list_id_normalizes_and_derives_addresses() {
    let id: ListId = "Developers.Example.COM".parse().unwrap();
    assert_eq!(id.as_str(), "developers.example.com");
    assert_eq!(id.posting_address(), "developers@example.com");
    assert_eq!(id.owner_address(), "developers-owner@example.com");
    assert!("missing-domain".parse::<ListId>().is_err());
}

#[test]
fn config_defaults_are_secure_and_file_overrides_are_layered() {
    let path = std::env::temp_dir().join(format!("listmngr-{}.toml", std::process::id()));
    std::fs::write(&path, "[site]\nname = \"Test Lists\"\n").unwrap();
    let config = Config::load(Some(&path)).unwrap();
    std::fs::remove_file(path).unwrap();
    assert_eq!(config.site.name, "Test Lists");
    assert!(!config.api.compat_basic_auth);
    assert_eq!(config.security.argon2.memory_kib, 65_536);
    assert!(
        config
            .api
            .compat_basic_auth_allow
            .iter()
            .any(|n| n.addr().is_loopback())
    );
}

#[test]
fn secret_file_overrides_inline_secret_and_redacted_view_hides_both() {
    let dir = tempfile::tempdir().unwrap();
    let secret = dir.path().join("database-url");
    std::fs::write(&secret, "postgres://app:file-password@db/listmngr\n").unwrap();
    let config_path = dir.path().join("listmngr.toml");
    std::fs::write(
        &config_path,
        format!(
            "[database]\nurl = \"postgres://app:inline-password@db/listmngr\"\nurl_file = {secret:?}\n"
        ),
    )
    .unwrap();

    let config = Config::load(Some(&config_path)).unwrap();
    assert_eq!(
        config.database.url,
        "postgres://app:file-password@db/listmngr"
    );
    let rendered = config.redacted_json().to_string();
    assert!(!rendered.contains("file-password"));
    assert!(!rendered.contains("inline-password"));
    assert!(!rendered.contains(secret.to_string_lossy().as_ref()));
    assert_eq!(rendered.matches("[REDACTED]").count(), 1);
}

#[test]
fn built_in_styles_apply_expected_list_defaults() {
    let styles = builtin_styles();
    assert_eq!(styles.len(), 3);
    let private = styles
        .iter()
        .find(|s| s.name() == "private-default")
        .unwrap();
    let mut list = MailingList::new("team.example.com".parse().unwrap(), "Team".into());
    private.apply(&mut list);
    assert!(!list.advertised);
    assert_eq!(list.archive_policy.as_str(), "private");
}

#[test]
fn idna_control_and_length_validation_is_strict() {
    use listmngr_core::Address;

    let address = Address::new("User@bücher.example", String::new()).unwrap();
    assert_eq!(address.email, "user@xn--bcher-kva.example");
    for invalid in [
        "a\r\n@example.com",
        "a\0@example.com",
        "a@@example.com",
        ".a@example.com",
        "a..b@example.com",
        "a@example",
    ] {
        assert!(Address::new(invalid, String::new()).is_err(), "{invalid:?}");
    }
    assert!(Address::new(&format!("{}@example.com", "a".repeat(65)), String::new()).is_err());
    assert!(Address::new(&format!("a@{}.com", "x".repeat(64)), String::new()).is_err());
    assert!(
        "list.bücher.example"
            .parse::<listmngr_core::ListId>()
            .is_ok()
    );
}

#[test]
fn phase_one_enums_have_stable_wire_values() {
    assert_eq!(MemberRole::Owner.as_str(), "owner");
    assert_eq!(SubscriptionMode::AsAddress.as_str(), "as_address");
    assert_eq!(DeliveryMode::MimeDigests.as_str(), "mime_digests");
    assert_eq!(DeliveryStatus::ByBounces.as_str(), "by_bounces");
    assert_eq!(ModerationAction::Hold.as_str(), "hold");
}

#[test]
fn preferences_overlay_only_replaces_present_values() {
    let system = Preferences {
        acknowledge_posts: Some(false),
        hide_address: Some(false),
        preferred_language: Some("en".into()),
        receive_list_copy: Some(true),
        receive_own_postings: Some(true),
        delivery_mode: Some(DeliveryMode::Regular),
        delivery_status: Some(DeliveryStatus::Enabled),
    };
    let user = Preferences {
        hide_address: Some(true),
        ..Preferences::default()
    };
    let member = Preferences {
        delivery_mode: Some(DeliveryMode::PlaintextDigests),
        ..Preferences::default()
    };
    let resolved = Preferences::resolve([&system, &user, &member]);
    assert_eq!(resolved.hide_address, Some(true));
    assert_eq!(resolved.delivery_mode, Some(DeliveryMode::PlaintextDigests));
    assert_eq!(resolved.preferred_language.as_deref(), Some("en"));
}
