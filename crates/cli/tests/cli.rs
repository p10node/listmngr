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
    run(&[
        "lists",
        "create",
        "dev.example.com",
        "--display-name",
        "Developers",
    ])
    .assert()
    .success();
    run(&[
        "user",
        "create",
        "alice@example.com",
        "--display-name",
        "Alice",
        "--password",
        "long enough password",
    ])
    .assert()
    .success();
    run(&["members", "add", "dev.example.com", "alice@example.com"])
        .assert()
        .success();
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
