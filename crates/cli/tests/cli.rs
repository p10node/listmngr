use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn phase_zero_commands_report_version_config_and_info() {
    Command::cargo_bin("listmngr")
        .unwrap()
        .arg("version")
        .assert()
        .success()
        .stdout(predicate::str::contains("listmngr 0.1.0"));
    Command::cargo_bin("listmngr")
        .unwrap()
        .arg("conf")
        .arg("--key")
        .arg("site.name")
        .assert()
        .success()
        .stdout("Example Lists\n");
    Command::cargo_bin("listmngr")
        .unwrap()
        .arg("info")
        .assert()
        .success()
        .stdout(predicate::str::contains("database:"));
}

#[test]
fn cli_conf_and_info_never_expose_database_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("listmngr.toml");
    std::fs::write(
        &path,
        "[database]\nurl = \"postgres://listmngr:top-secret@localhost/listmngr\"\n",
    )
    .unwrap();
    for command in ["conf", "info"] {
        Command::cargo_bin("listmngr")
            .unwrap()
            .args(["--config", path.to_str().unwrap(), command])
            .assert()
            .success()
            .stdout(predicate::str::contains("top-secret").not())
            .stderr(predicate::str::contains("top-secret").not());
    }
}

#[test]
fn config_precedence_is_defaults_then_file_then_env_then_secret_file() {
    let dir = tempfile::tempdir().unwrap();
    let secret = dir.path().join("database-url");
    std::fs::write(&secret, "sqlite://secret-file-sentinel\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let config = dir.path().join("listmngr.toml");
    std::fs::write(
        &config,
        format!(
            "[site]\nname = \"file-value\"\n[database]\nurl = \"sqlite://file-value\"\nurl_file = {secret:?}\n"
        ),
    )
    .unwrap();

    Command::cargo_bin("listmngr")
        .unwrap()
        .args([
            "--config",
            config.to_str().unwrap(),
            "conf",
            "--key",
            "site.name",
        ])
        .env("LISTMNGR__SITE__NAME", "environment-value")
        .assert()
        .success()
        .stdout("environment-value\n");
    Command::cargo_bin("listmngr")
        .unwrap()
        .args(["--config", config.to_str().unwrap(), "info"])
        .env("LISTMNGR__DATABASE__URL", "postgres://environment-value")
        .assert()
        .success()
        .stdout(predicate::str::contains("database: sqlite"))
        .stdout(predicate::str::contains("secret-file-sentinel").not())
        .stderr(predicate::str::contains("secret-file-sentinel").not());
}

#[test]
fn runtime_database_errors_have_an_id_and_redact_connection_input() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("SENSITIVE-CONNECTION-SENTINEL");
    Command::cargo_bin("listmngr")
        .unwrap()
        .args(["domains", "ls"])
        .env(
            "LISTMNGR__DATABASE__URL",
            format!("sqlite://{}?mode=ro", missing.display()),
        )
        .assert()
        .code(10)
        .stdout(predicate::str::contains("SENSITIVE-CONNECTION-SENTINEL").not())
        .stderr(predicate::str::contains("SENSITIVE-CONNECTION-SENTINEL").not())
        .stderr(predicate::str::contains("error[CLI-DATABASE]"))
        .stderr(predicate::str::contains("correlation="))
        .stderr(predicate::str::contains("operation failed"));
}

#[test]
fn phase_one_cli_crud_operates_on_sqlite() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("listmngr.db");
    let url = format!("sqlite://{}?mode=rwc", db.display());
    let run = |args: &[&str]| {
        let mut command = Command::cargo_bin("listmngr").unwrap();
        command.env("LISTMNGR__DATABASE__URL", &url).args(args);
        command
    };
    run(&["migrate"]).assert().success();
    run(&["domains", "add", "example.com"]).assert().success();
    run(&["domains", "ls"])
        .assert()
        .success()
        .stdout(predicate::str::contains("example.com"));
    run(&[
        "lists",
        "create",
        "dev.example.com",
        "--display-name",
        "Developers",
    ])
    .assert()
    .success();
    run(&["lists", "ls"])
        .assert()
        .success()
        .stdout(predicate::str::contains("dev.example.com"));
    let user_output = run(&[
        "user",
        "create",
        "alice@example.com",
        "--display-name",
        "Alice",
        "--password-stdin",
    ])
    .write_stdin("long enough password\n")
    .output()
    .unwrap();
    assert!(user_output.status.success());
    let user: serde_json::Value = serde_json::from_slice(&user_output.stdout).unwrap();
    let user_id = user["id"].as_str().unwrap();
    run(&["user", "passwd", user_id, "--password-stdin"])
        .write_stdin("Orbit!Cobalt7-River$Quartz\n")
        .assert()
        .success();
    let token_output = run(&["token", "create", user_id, "cli-test", "--scopes", "admin"])
        .output()
        .unwrap();
    assert!(token_output.status.success());
    let token = String::from_utf8(token_output.stdout).unwrap();
    let token_id = token
        .trim()
        .strip_prefix("lm_")
        .unwrap()
        .split_once('_')
        .unwrap()
        .0;
    run(&["token", "revoke", token_id]).assert().success();
    run(&["members", "add", "dev.example.com", "alice@example.com"])
        .assert()
        .success();
    run(&["members", "ls", "dev.example.com"])
        .assert()
        .success()
        .stdout(predicate::str::contains("alice@example.com"));
    run(&["members", "find", "alice@example.com"])
        .assert()
        .success()
        .stdout(predicate::str::contains("dev.example.com"));
    run(&["members", "del", "dev.example.com", "alice@example.com"])
        .assert()
        .success();
    run(&["lists", "remove", "dev.example.com"])
        .assert()
        .success();
    run(&["domains", "rm", "example.com"]).assert().success();
}

async fn assert_sync_dry_run_has_zero_writes(
    run: &(impl Fn(&[&str]) -> Command + Sync),
    db: &listmngr_db::Database,
    valid: &std::path::Path,
    audit_count: usize,
) {
    run(&[
        "members",
        "sync",
        "dev.example.com",
        valid.to_str().unwrap(),
        "--dry-run",
    ])
    .assert()
    .success()
    .stdout(predicate::str::contains(
        "dry-run role=member add=1 remove=0 retain=1",
    ));
    assert_eq!(db.audit().list().await.unwrap().len(), audit_count);
    assert!(
        db.members()
            .find("new@example.com")
            .await
            .unwrap()
            .is_empty()
    );
}

fn seed_sync_list(run: &impl Fn(&[&str]) -> Command) {
    run(&["migrate"]).assert().success();
    run(&["domains", "add", "example.com"]).assert().success();
    run(&[
        "lists",
        "create",
        "dev.example.com",
        "--display-name",
        "Dev",
    ])
    .assert()
    .success();
    run(&[
        "members",
        "add",
        "dev.example.com",
        "owner@example.com",
        "--role",
        "owner",
        "--mode",
        "as_user",
    ])
    .assert()
    .success();
    run(&[
        "members",
        "add",
        "dev.example.com",
        "keep@example.com",
        "--mode",
        "as_user",
        "--display-name",
        "Keep Me",
    ])
    .assert()
    .success();
}

#[tokio::test]
async fn members_sync_is_atomic_role_scoped_and_dry_run_has_zero_writes() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("listmngr.db");
    let url = format!("sqlite://{}?mode=rwc", db_path.display());
    let run = |args: &[&str]| {
        let mut command = Command::cargo_bin("listmngr").unwrap();
        command.env("LISTMNGR__DATABASE__URL", &url).args(args);
        command
    };
    seed_sync_list(&run);

    let db = listmngr_db::Database::connect(&url, 1).await.unwrap();
    let before = db.audit().list().await.unwrap().len();
    let valid = dir.path().join("valid.txt");
    std::fs::write(&valid, "keep@example.com\nnew@example.com\n").unwrap();
    assert_sync_dry_run_has_zero_writes(&run, &db, &valid, before).await;

    let malformed = dir.path().join("malformed.txt");
    std::fs::write(&malformed, "replacement@example.com\nlate-invalid\n").unwrap();
    run(&[
        "members",
        "sync",
        "dev.example.com",
        malformed.to_str().unwrap(),
    ])
    .assert()
    .code(2)
    .stderr(predicate::str::contains("error[CLI-VALIDATION]"));
    assert_eq!(db.audit().list().await.unwrap().len(), before);
    assert_eq!(
        db.members()
            .roster(
                &"dev.example.com".parse().unwrap(),
                listmngr_core::MemberRole::Member
            )
            .await
            .unwrap()
            .len(),
        1
    );

    run(&[
        "members",
        "sync",
        "dev.example.com",
        valid.to_str().unwrap(),
    ])
    .assert()
    .success()
    .stdout(predicate::str::contains(
        "synced role=member added=1 removed=0 retained=1",
    ));
    assert_eq!(db.audit().list().await.unwrap().len(), before + 1);
    let kept = db
        .members()
        .find("keep@example.com")
        .await
        .unwrap()
        .remove(0);
    assert_eq!(
        kept.subscription_mode,
        listmngr_core::SubscriptionMode::AsUser
    );
    assert_eq!(kept.display_name, "Keep Me");
    assert_eq!(
        db.members()
            .find("owner@example.com")
            .await
            .unwrap()
            .remove(0)
            .role,
        listmngr_core::MemberRole::Owner
    );
}

#[test]
fn backup_and_restore_carry_a_site_between_two_sqlite_files() {
    let dir = tempfile::tempdir().unwrap();
    let on = |name: &str| {
        let url = format!("sqlite://{}?mode=rwc", dir.path().join(name).display());
        move |args: &[&str]| {
            let mut command = Command::cargo_bin("listmngr").unwrap();
            command.env("LISTMNGR__DATABASE__URL", &url).args(args);
            command
        }
    };
    let source = on("source.db");
    source(&["migrate"]).assert().success();
    source(&["domains", "add", "example.com"])
        .assert()
        .success();
    source(&[
        "lists",
        "create",
        "dev.example.com",
        "--display-name",
        "Developers",
    ])
    .assert()
    .success();
    let backup = dir.path().join("backup");
    source(&["backup", backup.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"database\":\"sqlite\""))
        .stdout(predicate::str::contains("\"message_store\":\"db\""));
    assert!(backup.join("manifest.json").is_file());
    assert!(backup.join("tables").join("mailing_lists.jsonl").is_file());
    // Into the same directory again: refused as invalid input (the
    // detail is in the log, never on stderr).
    source(&["backup", backup.to_str().unwrap()])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("error[CLI-VALIDATION]"));
    let target = on("target.db");
    // Not migrated yet: refused, and nothing is written.
    target(&["restore", backup.to_str().unwrap()])
        .assert()
        .failure();
    target(&["migrate"]).assert().success();
    target(&["restore", backup.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"from\":\"sqlite\""));
    target(&["domains", "ls"])
        .assert()
        .success()
        .stdout(predicate::str::contains("example.com"));
    target(&["lists", "ls"])
        .assert()
        .success()
        .stdout(predicate::str::contains("dev.example.com"));
    // Once more into the same database: not empty, so invalid input.
    target(&["restore", backup.to_str().unwrap()])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("error[CLI-VALIDATION]"));
}
