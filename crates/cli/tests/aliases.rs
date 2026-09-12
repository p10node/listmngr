use assert_cmd::Command;
use listmngr_db::{Database, NewList};
use predicates::prelude::PredicateBooleanExt as _;
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

fn generations(output: &Path) -> usize {
    std::fs::read_dir(output)
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("generation-")
        })
        .count()
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
    // VERP bounce addresses follow the exact rows, one pattern per list.
    for pattern in [
        "alpha-bounces\\+[^@=]+=[^@=]+@example\\.invalid",
        "alpha-join-bounces\\+[^@=]+=[^@=]+@example\\.invalid",
        "beta-bounces\\+[^@=]+=[^@=]+@other\\.invalid",
    ] {
        writeln!(expected_recipients, "/^{pattern}$/ OK").unwrap();
        writeln!(expected_transport, "/^{pattern}$/ lmtp:[127.0.0.1]:8024").unwrap();
    }
    assert_eq!(recipients, expected_recipients);
    assert_eq!(transport, expected_transport);
    assert_eq!(
        domains,
        "/^example\\.invalid$/ OK\n/^other\\.invalid$/ OK\n"
    );
    assert!(recipients.contains("bounces"));
    assert!(recipients.contains("owner"));
    assert!(
        !recipients
            .lines()
            .any(|line| line.contains('+') && !line.contains("bounces")),
        "plus extensions exist only for VERP bounces"
    );
    assert_eq!(generations(&output), 1);
    assert_eq!(
        std::fs::read_link(output.join("current")).unwrap(),
        Path::new(generation.file_name().unwrap()),
        "`current` selects the new generation"
    );
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
    assert_eq!(generations(&output), 3);
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
    assert_eq!(generations(&output), 1);
}

#[test]
fn exim_maps_come_from_the_flag_or_the_configuration() {
    let root = tempfile::tempdir().unwrap();
    let url = setup(root.path());
    let output = root.path().join("maps");
    let result = command(root.path(), &url)
        .args(["aliases", "regen", "--mta", "exim", "--output"])
        .arg(&output)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let generation = std::path::PathBuf::from(String::from_utf8(result).unwrap().trim());
    assert_eq!(
        std::fs::read_to_string(generation.join("exim_domains")).unwrap(),
        "example.invalid\nother.invalid\n"
    );
    let recipients = std::fs::read_to_string(generation.join("exim_recipients")).unwrap();
    assert!(recipients.contains("alpha-owner@example.invalid\n"));
    assert!(!generation.join("recipients.regexp").exists());

    // `[mta] incoming = "exim"` with `map_directory` needs no flags at all.
    let configured = root.path().join("configured");
    let result = command(root.path(), &url)
        .env("LISTMNGR__MTA__INCOMING", "exim")
        .env("LISTMNGR__MTA__MAP_DIRECTORY", &configured)
        .args(["aliases", "regen"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let generation = std::path::PathBuf::from(String::from_utf8(result).unwrap().trim());
    assert_eq!(
        generation.parent(),
        Some(configured.canonicalize().unwrap().as_path())
    );
    assert!(generation.join("exim_recipients").exists());
}

#[test]
fn list_creation_and_removal_regenerate_the_maps_when_an_mta_is_configured() {
    let root = tempfile::tempdir().unwrap();
    let url = setup(root.path());
    let maps = root.path().join("maps");
    let configured = |root: &Path| {
        let mut command = command(root, &url);
        command
            .env("LISTMNGR__MTA__INCOMING", "postfix")
            .env("LISTMNGR__MTA__MAP_DIRECTORY", &maps)
            .env("LISTMNGR__MTA__LMTP_MAP_TARGET", "listmngr:8024");
        command
    };
    configured(root.path())
        .args([
            "lists",
            "create",
            "gamma.example.invalid",
            "--display-name",
            "Gamma",
        ])
        .assert()
        .success()
        .stderr(predicates::str::contains("MTA maps regenerated"));
    let current = maps.join("current");
    let transport = std::fs::read_to_string(current.join("transport.regexp")).unwrap();
    assert!(transport.contains("/^gamma@example\\.invalid$/ lmtp:[listmngr]:8024\n"));
    configured(root.path())
        .args(["lists", "remove", "gamma.example.invalid"])
        .assert()
        .success();
    let transport = std::fs::read_to_string(current.join("transport.regexp")).unwrap();
    assert!(!transport.contains("gamma"));
    assert_eq!(generations(&maps), 2);

    // Without an MTA nothing is written, and an unwritable directory does
    // not undo the list change.
    command(root.path(), &url)
        .args([
            "lists",
            "create",
            "delta.example.invalid",
            "--display-name",
            "Delta",
        ])
        .assert()
        .success()
        .stderr(predicates::str::contains("MTA maps").not());
    assert_eq!(generations(&maps), 2);
    let obstruction = root.path().join("obstruction");
    std::fs::write(&obstruction, b"not a directory").unwrap();
    command(root.path(), &url)
        .env("LISTMNGR__MTA__INCOMING", "postfix")
        .env("LISTMNGR__MTA__MAP_DIRECTORY", &obstruction)
        .args(["lists", "remove", "delta.example.invalid"])
        .assert()
        .success()
        .stderr(predicates::str::contains(
            "warning: MTA maps not regenerated",
        ));
    command(root.path(), &url)
        .args(["lists", "ls"])
        .assert()
        .success()
        .stdout(predicates::str::contains("delta").not());
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
            verp_delimiter: "+".into(),
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

#[test]
#[ignore = "requires explicit POSTMAP_BIN; compiles and queries only fixture maps, no MTA daemon"]
fn real_postmap_compiles_hash_maps_that_answer_exact_lookups() {
    let root = tempfile::tempdir().unwrap();
    let url = setup(root.path());
    let postmap = std::env::var_os("POSTMAP_BIN")
        .expect("set POSTMAP_BIN to the reviewed postmap executable");
    let config = root.path().join("postfix");
    std::fs::create_dir(&config).unwrap();
    let mut config_text = String::from("myhostname = fixture.invalid\n");
    if cfg!(target_os = "macos") {
        config_text.push_str("mail_owner = _postfix\nsetgid_group = _postdrop\n");
    }
    std::fs::write(config.join("main.cf"), config_text).unwrap();
    let output = root.path().join("maps");
    let result = command(root.path(), &url)
        .env("MAIL_CONFIG", &config)
        .env("LISTMNGR__MTA__TRANSPORT_FILE_TYPE", "hash")
        .env("LISTMNGR__MTA__POSTMAP_COMMAND", &postmap)
        .args(["aliases", "regen", "--output"])
        .arg(&output)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let generation = std::path::PathBuf::from(String::from_utf8(result).unwrap().trim());
    assert!(generation.join("postfix_lmtp.db").exists());
    assert!(generation.join("postfix_domains.db").exists());
    for (key, file, expected) in [
        (
            "alpha@example.invalid",
            "postfix_lmtp",
            Some("lmtp:[127.0.0.1]:8024\n"),
        ),
        (
            "alpha-bounces@example.invalid",
            "postfix_lmtp",
            Some("lmtp:[127.0.0.1]:8024\n"),
        ),
        ("alpha-bounces+x=y@example.invalid", "postfix_lmtp", None),
        ("missing@example.invalid", "postfix_lmtp", None),
        (
            "example.invalid",
            "postfix_domains",
            Some("example.invalid\n"),
        ),
        ("empty.invalid", "postfix_domains", None),
    ] {
        let mut query = Command::new(&postmap);
        query
            .env_clear()
            .current_dir(root.path())
            .timeout(std::time::Duration::from_secs(10))
            .arg("-c")
            .arg(&config)
            .args(["-q", key])
            .arg(format!("hash:{}", generation.join(file).display()));
        match expected {
            Some(value) => {
                query.assert().success().stdout(value);
            }
            None => {
                query.assert().code(1).stdout("");
            }
        }
    }
}
