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
    std::fs::write(&secret, "sqlite://file-sentinel\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let config_path = dir.path().join("listmngr.toml");
    std::fs::write(
        &config_path,
        format!("[database]\nurl = \"sqlite://inline-sentinel\"\nurl_file = {secret:?}\n"),
    )
    .unwrap();

    let config = Config::load(Some(&config_path)).unwrap();
    assert_eq!(config.database.url, "sqlite://file-sentinel");
    let rendered = config.redacted_json().to_string();
    assert!(!rendered.contains("file-sentinel"));
    assert!(!rendered.contains("inline-sentinel"));
    assert!(!rendered.contains(secret.to_string_lossy().as_ref()));
    assert_eq!(rendered.matches("[REDACTED]").count(), 1);
}

#[cfg(unix)]
#[test]
fn secret_file_permissions_fail_closed_without_disclosing_path_or_value() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let secret = dir.path().join("SENSITIVE-PATH-SENTINEL");
    std::fs::write(&secret, "sqlite://sensitive-value-sentinel\n").unwrap();
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o644)).unwrap();
    let config_path = dir.path().join("listmngr.toml");
    std::fs::write(&config_path, format!("[database]\nurl_file = {secret:?}\n")).unwrap();

    let rendered = Config::load(Some(&config_path)).unwrap_err().to_string();
    assert!(rendered.contains("must not be accessible by group or other users"));
    assert!(!rendered.contains("sensitive-value-sentinel"));
    assert!(!rendered.contains("SENSITIVE-PATH-SENTINEL"));
}

#[test]
fn enabling_mail_role_without_the_explicit_trusted_relay_mode_fails_closed() {
    for smtp_tls in ["opportunistic", "none", ""] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("listmngr.toml");
        std::fs::write(
            &path,
            format!("[mta]\nenabled = true\nsmtp_tls = {smtp_tls:?}\n"),
        )
        .unwrap();
        let error = Config::load(Some(&path)).unwrap_err().to_string();
        assert!(
            error.contains("plaintext_trusted_relay"),
            "smtp_tls={smtp_tls:?} must fail closed, got: {error}"
        );
    }
    // The explicit, honestly-scoped mode is accepted.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("listmngr.toml");
    std::fs::write(
        &path,
        "[mta]\nenabled = true\nsmtp_tls = \"plaintext_trusted_relay\"\n",
    )
    .unwrap();
    let config = Config::load(Some(&path)).unwrap();
    assert!(config.mta.enabled);
    // Disabled mail role never validates smtp_tls: existing web-only configs
    // and tests must not start failing because of an unrelated default.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("listmngr.toml");
    std::fs::write(&path, "[site]\nname = \"web only\"\n").unwrap();
    assert!(!Config::load(Some(&path)).unwrap().mta.enabled);
}

#[test]
fn required_starttls_configuration_is_admitted() {
    let dir = tempfile::tempdir_in(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target")).unwrap();
    let path = dir.path().join("starttls.toml");
    std::fs::write(&path, "[mta]\nenabled = true\nsmtp_tls = 'required'\nsmtp_tls_server_name = 'relay.example.invalid'\n").unwrap();
    let config = Config::load(Some(&path)).expect("required STARTTLS must be admitted");
    assert_eq!(config.mta.smtp_tls, "required");
    assert_eq!(
        config.mta.smtp_tls_server_name.as_deref(),
        Some("relay.example.invalid")
    );
}

#[test]
fn config_rejects_invalid_api_rate_limits() {
    for (key, spec) in [
        ("api", ""),
        ("api", "0/min"),
        ("api", "10"),
        ("api", "10/fortnight"),
        ("api", "many/min"),
        ("api_pre_auth", "0/min"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("listmngr.toml");
        std::fs::write(&path, format!("[security.rate_limit]\n{key} = {spec:?}\n")).unwrap();
        let error = Config::load(Some(&path)).unwrap_err();
        assert!(
            error
                .to_string()
                .contains(&format!("security.rate_limit.{key}")),
            "unexpected error for {key}={spec:?}: {error}"
        );
    }
}

#[test]
fn config_accepts_only_enforced_second_factor_roles() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("listmngr.toml");
    std::fs::write(&path, "[security]\nrequire_2fa_for = [\"list_owner\"]\n").unwrap();
    let error = Config::load(Some(&path)).unwrap_err().to_string();
    assert!(error.contains("security.require_2fa_for"), "{error}");
    assert!(error.contains("list_owner"), "{error}");
    std::fs::write(&path, "[security]\nrequire_2fa_for = []\n").unwrap();
    assert!(
        Config::load(Some(&path))
            .unwrap()
            .security
            .require_2fa_for
            .is_empty()
    );
    std::fs::write(&path, "[security]\nrequire_2fa_for = [\"server_owner\"]\n").unwrap();
    assert_eq!(
        Config::load(Some(&path)).unwrap().security.require_2fa_for,
        ["server_owner"]
    );
}

#[test]
fn oidc_providers_are_validated_and_secrets_come_from_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("listmngr.toml");
    for (name, body, fragment) in [
        (
            "bad slug",
            "[[web.oidc]]\nname = \"Google!\"\ndisplay_name = \"Google\"\nissuer = \"https://accounts.google.com\"\nclient_id = \"x\"\nclient_secret = \"y\"\n",
            "web.oidc[].name",
        ),
        (
            "plain http issuer",
            "[[web.oidc]]\nname = \"corp\"\ndisplay_name = \"Corp\"\nissuer = \"http://idp.example.com\"\nclient_id = \"x\"\nclient_secret = \"y\"\n",
            "issuer",
        ),
        (
            "no secret",
            "[[web.oidc]]\nname = \"corp\"\ndisplay_name = \"Corp\"\nissuer = \"https://idp.example.com\"\nclient_id = \"x\"\n",
            "client_secret",
        ),
        (
            "duplicate",
            "[[web.oidc]]\nname = \"corp\"\ndisplay_name = \"Corp\"\nissuer = \"https://idp.example.com\"\nclient_id = \"x\"\nclient_secret = \"y\"\n[[web.oidc]]\nname = \"corp\"\ndisplay_name = \"Corp 2\"\nissuer = \"https://idp2.example.com\"\nclient_id = \"x\"\nclient_secret = \"y\"\n",
            "unique",
        ),
    ] {
        std::fs::write(&path, body).unwrap();
        let error = Config::load(Some(&path)).unwrap_err().to_string();
        assert!(error.contains(fragment), "{name}: {error}");
        assert!(
            !error.contains("client_secret = ") && !error.contains("\"y\""),
            "{name}: a secret leaked: {error}"
        );
    }
    let secret = dir.path().join("oidc-secret");
    std::fs::write(&secret, "PRIVATE-SECRET-SENTINEL\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    std::fs::write(
        &path,
        format!(
            "[[web.oidc]]\nname = \"google\"\ndisplay_name = \"Google\"\nissuer = \"https://accounts.google.com/\"\nclient_id = \"x\"\nclient_secret_file = {secret:?}\nscopes = [\"email\"]\n"
        ),
    )
    .unwrap();
    let config = Config::load(Some(&path)).unwrap();
    let provider = &config.web.oidc[0];
    assert_eq!(
        provider.issuer, "https://accounts.google.com",
        "trailing slash trimmed"
    );
    assert_eq!(
        provider
            .client_secret
            .as_ref()
            .map(listmngr_core::SmtpAuthSecret::expose),
        Some("PRIVATE-SECRET-SENTINEL")
    );
    let shown = format!("{provider:?}");
    assert!(
        !shown.contains("PRIVATE-SECRET-SENTINEL"),
        "Debug redacts the secret: {shown}"
    );
    let shown = serde_json::to_string(&config.web).unwrap();
    assert!(
        !shown.contains("PRIVATE-SECRET-SENTINEL"),
        "serialization redacts the secret: {shown}"
    );
    assert_eq!(
        provider.scopes,
        ["openid", "email"],
        "openid is always requested"
    );
    assert!(
        Config::default().web.oidc.is_empty(),
        "no provider by default"
    );
}

#[test]
fn secret_file_read_errors_are_generic_and_valid_utf8_still_loads() {
    let dir = tempfile::tempdir().unwrap();
    let secret = dir.path().join("PRIVATE-PATH-SENTINEL");
    std::fs::write(&secret, b"PRIVATE-VALUE-SENTINEL\xff").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let config_path = dir.path().join("listmngr.toml");
    std::fs::write(&config_path, format!("[database]\nurl_file = {secret:?}\n")).unwrap();
    let error = Config::load(Some(&config_path)).unwrap_err().to_string();
    assert_eq!(error, "validation failed: cannot read database.url_file");
    std::fs::write(&secret, "sqlite://fixture-value\n").unwrap();
    assert_eq!(
        Config::load(Some(&config_path)).unwrap().database.url,
        "sqlite://fixture-value"
    );
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

/// The `hyperkitty` archiver's settings: a URL needs a key, the key one
/// way only, and the key file never in the redacted view.
#[test]
fn hyperkitty_archiver_settings_are_checked_and_the_key_file_is_hidden() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("listmngr.toml");
    std::fs::write(
        &path,
        "[archive.archivers]\nhyperkitty_url = \"https://lists.example.invalid/hyperkitty\"\n",
    )
    .unwrap();
    assert!(Config::load(Some(&path)).is_err(), "a URL without a key");
    std::fs::write(
        &path,
        "[archive.archivers]\nhyperkitty_url = \"ftp://lists.example.invalid\"\nhyperkitty_api_key = \"k\"\n",
    )
    .unwrap();
    assert!(Config::load(Some(&path)).is_err(), "not an http(s) URL");
    std::fs::write(&path, "[archive.archivers]\nhyperkitty_api_key = \"k\"\n").unwrap();
    assert!(Config::load(Some(&path)).is_err(), "a key without a URL");
    let secret = dir.path().join("hk.key");
    std::fs::write(&secret, "archiver-key-sentinel\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    std::fs::write(
        &path,
        format!(
            "[archive.archivers]\nhyperkitty_url = \"https://lists.example.invalid/hyperkitty/\"\nhyperkitty_api_key = \"k\"\nhyperkitty_api_key_file = {secret:?}\n"
        ),
    )
    .unwrap();
    assert!(Config::load(Some(&path)).is_err(), "the key one way only");
    std::fs::write(
        &path,
        format!(
            "[archive.archivers]\nhyperkitty_url = \"https://lists.example.invalid/hyperkitty/\"\nhyperkitty_api_key_file = {secret:?}\n"
        ),
    )
    .unwrap();
    let config = Config::load(Some(&path)).unwrap();
    assert_eq!(
        config.archive.archivers.hyperkitty_api_key(),
        Some("archiver-key-sentinel")
    );
    let rendered = config.redacted_json().to_string();
    assert!(!rendered.contains("sentinel"), "{rendered}");
    assert!(!rendered.contains("hyperkitty_api_key_file"), "{rendered}");
    assert!(
        rendered.contains("lists.example.invalid/hyperkitty"),
        "{rendered}"
    );
}

/// `[web] tls`: the three settings together, an address of its own, the
/// files present and the key the owner's alone.
#[test]
fn web_tls_settings_are_checked_together_and_the_key_must_be_private() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("listmngr.toml");
    let cert = dir.path().join("cert.pem");
    let key = dir.path().join("key.pem");
    std::fs::write(&cert, "not a certificate\n").unwrap();
    std::fs::write(&key, "not a key\n").unwrap();
    std::fs::write(
        &path,
        format!("[web.tls]\nlisten = \"127.0.0.1:8443\"\ncert_file = {cert:?}\n"),
    )
    .unwrap();
    assert!(Config::load(Some(&path)).is_err(), "the three go together");
    std::fs::write(
        &path,
        format!("[web]\nlisten = \"127.0.0.1:8443\"\n[web.tls]\nlisten = \"127.0.0.1:8443\"\ncert_file = {cert:?}\nkey_file = {key:?}\n"),
    )
    .unwrap();
    assert!(Config::load(Some(&path)).is_err(), "an address of its own");
    std::fs::write(
        &path,
        format!(
            "[web.tls]\nlisten = \"127.0.0.1:8443\"\ncert_file = {cert:?}\nkey_file = {key:?}\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(Config::load(Some(&path)).is_err(), "a readable key");
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let config = Config::load(Some(&path)).unwrap();
    assert!(config.web.tls.enabled());
    assert_eq!(config.web.tls.listen.as_deref(), Some("127.0.0.1:8443"));
    std::fs::write(
        &path,
        format!("[web.tls]\nlisten = \"127.0.0.1:8443\"\ncert_file = {cert:?}\nkey_file = \"{}/missing.pem\"\n", dir.path().display()),
    )
    .unwrap();
    assert!(Config::load(Some(&path)).is_err(), "a missing file");
    assert!(!Config::default().web.tls.enabled());
}

#[test]
fn web_tls_acme_settings_are_checked_and_exclude_own_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("listmngr.toml");
    let cert = dir.path().join("cert.pem");
    std::fs::write(&cert, "not a certificate\n").unwrap();
    let cache = dir.path().join("acme");
    let load = |body: String| {
        std::fs::write(&path, body).unwrap();
        Config::load(Some(&path))
    };
    assert!(
        load(format!(
            "[web.tls]\nlisten = \"127.0.0.1:8443\"\nacme_domains = [\"lists.example.com\"]\nacme_cache_dir = {cache:?}\ncert_file = {cert:?}\n"
        ))
        .is_err(),
        "own files and ACME are exclusive"
    );
    assert!(
        load(
            "[web.tls]\nlisten = \"127.0.0.1:8443\"\nacme_domains = [\"lists.example.com\"]\n"
                .into()
        )
        .is_err(),
        "the cache directory is needed"
    );
    assert!(
        load(format!(
            "[web.tls]\nacme_domains = [\"lists.example.com\"]\nacme_cache_dir = {cache:?}\n"
        ))
        .is_err(),
        "listen is needed"
    );
    assert!(
        load("[web.tls]\nlisten = \"127.0.0.1:8443\"\n".into()).is_err(),
        "listen alone is nothing to serve"
    );
    for domain in [
        "Lists.Example.com",
        "*.example.com",
        "",
        "lists.example.com.",
    ] {
        assert!(
            load(format!(
                "[web.tls]\nlisten = \"127.0.0.1:8443\"\nacme_domains = [{domain:?}]\nacme_cache_dir = {cache:?}\n"
            ))
            .is_err(),
            "{domain:?} is not a plain lowercase host name"
        );
    }
    assert!(
        load(format!(
            "[web.tls]\nlisten = \"127.0.0.1:8443\"\nacme_domains = [\"lists.example.com\"]\nacme_cache_dir = {cache:?}\nacme_directory_url = \"http://acme.example.com/directory\"\n"
        ))
        .is_err(),
        "the directory is https"
    );
    assert!(
        load(format!(
            "[web.tls]\nlisten = \"127.0.0.1:8443\"\nacme_domains = [\"lists.example.com\"]\nacme_cache_dir = {cache:?}\nacme_contact = \"mailto:postmaster@example.com\"\n"
        ))
        .is_err(),
        "the contact is a bare address"
    );
    assert!(
        load(format!(
            "[web.tls]\nlisten = \"127.0.0.1:8443\"\nacme_domains = [\"lists.example.com\"]\nacme_cache_dir = {cache:?}\nacme_ca_file = \"{}/missing.pem\"\n",
            dir.path().display()
        ))
        .is_err(),
        "a CA file that exists"
    );
    assert!(
        load(format!(
            "[web.tls]\nlisten = \"127.0.0.1:8443\"\nacme_domains = [\"lists.example.com\"]\nacme_cache_dir = {cert:?}\n"
        ))
        .is_err(),
        "a cache path that is a file"
    );
    let config = load(format!(
        "[web.tls]\nlisten = \"127.0.0.1:8443\"\nacme_domains = [\"lists.example.com\", \"lists.example.org\"]\nacme_cache_dir = {cache:?}\nacme_contact = \"postmaster@example.com\"\nacme_ca_file = {cert:?}\n"
    ))
    .unwrap();
    assert!(config.web.tls.enabled());
    assert!(config.web.tls.acme());
    assert_eq!(config.web.tls.acme_domains.len(), 2);
    assert_eq!(
        config.web.tls.acme_directory_url,
        "https://acme-v02.api.letsencrypt.org/directory"
    );
    assert!(!Config::default().web.tls.acme());
}
