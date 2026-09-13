use super::*;

#[test]
fn smtp_auth_config_rejects_unsafe_policy_and_redacts() {
    for (mode, user, password) in [
        (
            "plaintext_trusted_relay",
            Some("owned-user"),
            Some("owned-password"),
        ),
        ("required", Some(""), Some("owned-password")),
        ("required", Some("owned-user"), Some("")),
        ("required", Some("owned\0user"), Some("owned-password")),
        ("required", Some("owned-user"), Some("owned\npassword")),
        ("required", Some("owned-user"), None),
        ("required", None, Some("owned-password")),
    ] {
        let mut config = listmngr_core::Config::default();
        config.mta.enabled = true;
        config.mta.smtp_tls = mode.into();
        config.mta.smtp_auth_username = user.map(Into::into);
        config.mta.smtp_auth_password = password.map(Into::into);
        assert!(
            MailRoleConfig::from_core(&config).is_err(),
            "unsafe auth admitted"
        );
        let debug = format!("{config:?} {}", config.redacted_json());
        assert!(!debug.contains("owned"));
    }
    let mut config = listmngr_core::Config::default();
    config.mta.enabled = true;
    config.mta.smtp_tls = "required".into();
    config.mta.smtp_auth_username = Some("x".repeat(256).into());
    config.mta.smtp_auth_password = Some("fake-password".into());
    assert!(MailRoleConfig::from_core(&config).is_err());
}

#[test]
fn smtp_auth_password_file_controls() {
    let dir = tempfile::tempdir_in(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target")).unwrap();
    let path = dir.path().join("password");
    let mut config = listmngr_core::Config::default();
    config.mta.smtp_tls = "required".into();
    config.mta.smtp_auth_username = Some("owned-user".into());
    config.mta.smtp_auth_password_file = Some(path.clone());
    assert!(config.mta.smtp_auth_credentials().is_err());
    for value in ["", "x\0y", "x\ry", &"x".repeat(256)] {
        std::fs::write(&path, value).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert!(config.mta.smtp_auth_credentials().is_err());
    }
    std::fs::write(&path, "owned-password\r\n").unwrap();
    assert!(config.mta.smtp_auth_credentials().is_ok());
    config.mta.smtp_auth_password = Some("other-owned-password".into());
    assert!(config.mta.smtp_auth_credentials().is_err());
    config.mta.smtp_auth_password = None;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(config.mta.smtp_auth_credentials().is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        config.mta.smtp_auth_password_file = Some(link);
        assert!(
            config.mta.smtp_auth_credentials().is_err(),
            "symlink secret admitted"
        );
    }
}
