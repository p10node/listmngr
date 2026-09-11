use assert_cmd::Command;
use listmngr_db::{Database, NewList};
use std::fmt::Write as _;
use std::path::Path;

fn command(root: &Path, url: &str) -> Command {
    let mut command = Command::cargo_bin("listmngr").unwrap();
    command
        .env_clear()
        .current_dir(root)
        .env("LISTMNGR__DATABASE__URL", url);
    command
}

fn setup(root: &Path) -> String {
    let url = format!("sqlite://{}?mode=rwc", root.join("fixture.db").display());
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let db = Database::connect(&url, 1).await.unwrap();
        db.migrate().await.unwrap();
        for domain in ["example.invalid", "other.invalid", "empty.invalid"] {
            db.domains().create(domain, "", None).await.unwrap();
        }
        for id in [
            "alpha.example.invalid",
            "alpha-join.example.invalid",
            "beta.other.invalid",
        ] {
            db.lists()
                .create(NewList {
                    list_id: id.parse().unwrap(),
                    display_name: "A display name is not a map key".into(),
                    style: "private-default".into(),
                })
                .await
                .unwrap();
        }
        db.pool().close().await;
    });
    url
}

#[test]
fn regen_publishes_exact_supported_recipients_in_a_fresh_generation() {
    let root = tempfile::tempdir().unwrap();
    let url = setup(root.path());
    let output = root.path().join("maps");
    let result = command(root.path(), &url)
        .args(["aliases", "regen", "--output"])
        .arg(&output)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let generation = std::path::PathBuf::from(String::from_utf8(result).unwrap().trim());
    assert_eq!(
        generation.parent(),
        Some(output.canonicalize().unwrap().as_path())
    );
    let recipients = std::fs::read_to_string(generation.join("recipients.regexp")).unwrap();
    let transport = std::fs::read_to_string(generation.join("transport.regexp")).unwrap();
    let domains = std::fs::read_to_string(generation.join("domains.regexp")).unwrap();
    let mut expected = std::collections::BTreeSet::new();
    for name in [
        "alpha@example.invalid",
        "alpha-join@example.invalid",
        "beta@other.invalid",
    ] {
        expected.insert(name.to_owned());
        let (local, host) = name.split_once('@').unwrap();
        for suffix in [
            "owner",
            "bounces",
            "join",
            "subscribe",
            "leave",
            "unsubscribe",
            "request",
            "confirm",
        ] {
            expected.insert(format!("{local}-{suffix}@{host}"));
        }
    }
    let mut expected_recipients = String::new();
    let mut expected_transport = String::new();
    for address in expected {
        let escaped = address.replace('.', "\\.");
        writeln!(expected_recipients, "/^{escaped}$/ OK").unwrap();
        writeln!(expected_transport, "/^{escaped}$/ lmtp:[127.0.0.1]:8024").unwrap();
    }
    assert_eq!(recipients, expected_recipients);
    assert_eq!(transport, expected_transport);
    assert_eq!(
        domains,
        "/^example\\.invalid$/ OK\n/^other\\.invalid$/ OK\n"
    );
    assert!(recipients.contains("bounces"));
    assert!(recipients.contains("owner"));
    assert!(!recipients.contains('+'));
    assert_eq!(std::fs::read_dir(&output).unwrap().count(), 1);
}

#[test]
fn map_targets_reject_non_destinations_before_creating_output() {
    let root = tempfile::tempdir().unwrap();
    let url = setup(root.path());
    let output = root.path().join("maps");
    for target in [
        "0.0.0.0:8024",
        "[::]:8024",
        "127.0.0.1:0",
        "[ff02::1]:8024",
        "224.0.0.1:8024",
        "[fe80::1%3]:8024",
    ] {
        command(root.path(), &url)
            .env("LISTMNGR__MTA__LMTP_LISTEN", target)
            .args(["aliases", "regen", "--output"])
            .arg(&output)
            .assert()
            .code(2);
        assert!(
            !output.exists(),
            "invalid target must not publish: {target}"
        );
    }
}

#[test]
fn explicit_ipv6_target_overrides_wildcard_listener_without_changing_config() {
    let root = tempfile::tempdir().unwrap();
    let url = setup(root.path());
    let output = root.path().join("maps");
    let result = command(root.path(), &url)
        .env("LISTMNGR__MTA__LMTP_LISTEN", "0.0.0.0:8024")
        .args([
            "aliases",
            "regen",
            "--lmtp-target",
            "[::1]:9124",
            "--output",
        ])
        .arg(&output)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let generation = std::path::PathBuf::from(String::from_utf8(result).unwrap().trim());
    let transport = std::fs::read_to_string(generation.join("transport.regexp")).unwrap();
    assert!(!transport.is_empty());
    assert!(
        transport
            .lines()
            .all(|line| line.ends_with(" lmtp:[::1]:9124"))
    );
}

fn generate(root: &Path, url: &str, output: &Path) -> std::path::PathBuf {
    let result = command(root, url)
        .args(["aliases", "regen", "--output"])
        .arg(output)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    std::path::PathBuf::from(String::from_utf8(result).unwrap().trim())
}

fn map_bytes(generation: &Path) -> Vec<Vec<u8>> {
    ["domains.regexp", "recipients.regexp", "transport.regexp"]
        .iter()
        .map(|file| std::fs::read(generation.join(file)).unwrap())
        .collect()
}

#[test]
fn regeneration_preserves_prior_generations_and_reflects_deletion() {
    let root = tempfile::tempdir().unwrap();
    let url = setup(root.path());
    let output = root.path().join("maps");
    let first = generate(root.path(), &url, &output);
    let original = map_bytes(&first);
    let second = generate(root.path(), &url, &output);
    assert_ne!(first, second);
    assert_eq!(original, map_bytes(&second));
    command(root.path(), &url)
        .args(["lists", "remove", "beta.other.invalid"])
        .assert()
        .success();
    let third = generate(root.path(), &url, &output);
    assert_eq!(original, map_bytes(&first));
    assert_eq!(original, map_bytes(&second));
    assert!(
        map_bytes(&third)
            .iter()
            .all(|bytes| !String::from_utf8_lossy(bytes).contains("other"))
    );
    assert_eq!(std::fs::read_dir(&output).unwrap().count(), 3);
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let db = Database::connect(&url, 1).await.unwrap();
        let before = db.audit().list().await.unwrap().len();
        generate(root.path(), &url, &output);
        assert_eq!(before, db.audit().list().await.unwrap().len());
        db.pool().close().await;
    });
}

#[test]
fn failed_database_or_filesystem_never_replaces_a_published_generation() {
    let root = tempfile::tempdir().unwrap();
    let url = setup(root.path());
    let output = root.path().join("maps");
    let generation = generate(root.path(), &url, &output);
    let original = map_bytes(&generation);
    command(root.path(), "sqlite::memory:")
        .args(["aliases", "regen", "--output"])
        .arg(&output)
        .assert()
        .code(10);
    let obstruction = root.path().join("not-a-directory");
    std::fs::write(&obstruction, b"must survive").unwrap();
    command(root.path(), &url)
        .args(["aliases", "regen", "--output"])
        .arg(&obstruction)
        .assert()
        .code(9);
    assert_eq!(std::fs::read(obstruction).unwrap(), b"must survive");
    assert_eq!(map_bytes(&generation), original);
    assert_eq!(std::fs::read_dir(output).unwrap().count(), 1);
}

#[test]
#[ignore = "requires explicit POSTMAP_BIN; queries only fixture maps, no MTA daemon"]
fn real_postfix_lookup_agrees_with_runtime_recipient_validation() {
    use listmngr_mail::lmtp::LmtpHandler;
    let root = tempfile::tempdir().unwrap();
    let url = setup(root.path());
    let generation = generate(root.path(), &url, &root.path().join("maps"));
    let postmap = std::env::var_os("POSTMAP_BIN")
        .expect("set POSTMAP_BIN to the reviewed postmap executable");
    let config = root.path().join("postfix");
    std::fs::create_dir(&config).unwrap();
    let mut config_text = String::from("myhostname = fixture.invalid\n");
    // Apple's system accounts have underscore names; upstream defaults alias
    // the same IDs and fail Postfix's duplicate-account safety check.
    if cfg!(target_os = "macos") {
        config_text.push_str("mail_owner = _postfix\nsetgid_group = _postdrop\n");
    }
    std::fs::write(config.join("main.cf"), config_text).unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let db = Database::connect(&url, 1).await.unwrap();
        let mut handler = listmngr_runners::InboundHandler {
            db: db.clone(),
            local_hostname: "fixture.invalid".into(),
            max_message_bytes: 1024,
            max_recipients: 10,
            command_timeout: std::time::Duration::from_secs(2),
            in_max_attempts: 3,
        };
        for (address, valid) in [
            ("alpha@example.invalid", true),
            ("ALPHA@EXAMPLE.INVALID", true),
            ("alpha-join@example.invalid", true),
            ("alpha-join-confirm@example.invalid", true),
            ("alpha-request@example.invalid", true),
            ("alpha-subscribe@example.invalid", true),
            ("alpha-leave@example.invalid", true),
            ("alpha-unsubscribe@example.invalid", true),
            ("alpha-confirm@example.invalid", true),
            ("beta@other.invalid", true),
            ("missing@example.invalid", false),
            ("alpha-owner@example.invalid", true),
            ("alpha-bounces@example.invalid", true),
            ("alpha-bounces+token@example.invalid", false),
            ("alpha-confirm+token@example.invalid", false),
            ("alpha+detail@example.invalid", false),
            ("alpha@exampleXinvalid", false),
            ("alpha@child.example.invalid", false),
            ("alpha@empty.invalid", false),
        ] {
            assert_eq!(
                handler.validate_recipient(address).await.is_ok(),
                valid,
                "{address}"
            );
            for (file, expected) in [
                ("recipients.regexp", "OK\n"),
                ("transport.regexp", "lmtp:[127.0.0.1]:8024\n"),
            ] {
                let mut query = Command::new(&postmap);
                query
                    .env_clear()
                    .current_dir(root.path())
                    .timeout(std::time::Duration::from_secs(10))
                    .arg("-c")
                    .arg(&config)
                    .args(["-q", address])
                    .arg(format!("regexp:{}", generation.join(file).display()));
                if valid {
                    query.assert().success().stdout(expected);
                } else {
                    query.assert().code(1).stdout("");
                }
            }
        }
        db.pool().close().await;
    });
}
