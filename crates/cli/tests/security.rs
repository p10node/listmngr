use assert_cmd::Command;
use predicates::prelude::*;

fn cli(url: &str, args: &[&str]) -> Command {
    let mut command = Command::cargo_bin("listmngr").unwrap();
    for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("LISTMNGR")) {
        command.env_remove(key);
    }
    command.env("LISTMNGR__DATABASE__URL", url).args(args);
    command.timeout(std::time::Duration::from_secs(15));
    command
}
fn sqlite_url(dir: &tempfile::TempDir) -> String {
    format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("fixture.db").display()
    )
}
const fn create_args() -> [&'static str; 6] {
    [
        "user",
        "create",
        "alice@example.com",
        "--display-name",
        "Alice",
        "--password-stdin",
    ]
}

#[tokio::test]
async fn stdin_password_create_and_passwd_persist_without_disclosure() {
    let dir = tempfile::tempdir().unwrap();
    let url = sqlite_url(&dir);
    cli(&url, &["migrate"]).assert().success();
    let original = "Orbit!Cobalt7-River$Quartz";
    let changed = "Nebula!Copper8-Ocean$Jasper";
    let output = cli(&url, &create_args())
        .write_stdin(format!("{original}\r\n"))
        .assert()
        .success()
        .stdout(predicate::str::contains(original).not())
        .stderr(predicate::str::contains(original).not())
        .get_output()
        .clone();
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let id = body["id"].as_str().unwrap();
    let db = listmngr_db::Database::connect(&url, 1).await.unwrap();
    assert!(
        db.users()
            .verify_password(id.parse().unwrap(), original)
            .await
            .unwrap()
    );
    cli(&url, &["user", "passwd", id, "--password-stdin"])
        .write_stdin(format!("{changed}\n"))
        .assert()
        .success()
        .stdout("password changed\n")
        .stderr(predicate::str::contains(changed).not());
    assert!(
        db.users()
            .verify_password(id.parse().unwrap(), changed)
            .await
            .unwrap()
    );
    assert!(
        !db.users()
            .verify_password(id.parse().unwrap(), original)
            .await
            .unwrap()
    );
}

#[test]
fn argv_password_and_parse_values_are_never_disclosed() {
    for args in [
        vec![
            "user",
            "create",
            "alice@example.com",
            "--display-name",
            "Alice",
            "--password",
            "ARGV-SECRET",
        ],
        vec!["user", "passwd", "ARGV-SECRET", "--password-stdin"],
    ] {
        cli("sqlite::memory:", &args)
            .assert()
            .code(2)
            .stderr("error[CLI-USAGE]: invalid command line\n")
            .stdout(predicate::str::contains("ARGV-SECRET").not());
    }
}

#[test]
fn runtime_errors_have_typed_redacted_categories() {
    let dir = tempfile::tempdir().unwrap();
    let url = sqlite_url(&dir);
    cli(&url, &["migrate"]).assert().success();
    cli(&url, &["domains", "add", "example.com"])
        .assert()
        .success();
    for (args, code, category) in [
        (vec!["domains", "add", "example.com"], 6, "CLI-CONFLICT"),
        (
            vec!["domains", "rm", "absent.example.com"],
            7,
            "CLI-NOT-FOUND",
        ),
        (
            vec!["domains", "add", "PRIVATE-SENTINEL invalid"],
            2,
            "CLI-VALIDATION",
        ),
        (
            vec![
                "members",
                "sync",
                "dev.example.com",
                "PRIVATE-SENTINEL/missing",
            ],
            9,
            "CLI-IO",
        ),
    ] {
        cli(&url, &args)
            .assert()
            .code(code)
            .stderr(predicate::str::contains(format!("error[{category}]")))
            .stderr(predicate::str::contains("PRIVATE-SENTINEL").not())
            .stderr(predicate::str::contains("correlation="));
    }
    cli("invalid-PRIVATE-SENTINEL", &["domains", "ls"])
        .assert()
        .code(10)
        .stderr(predicate::str::contains("error[CLI-DATABASE]"))
        .stderr(predicate::str::contains("PRIVATE-SENTINEL").not());
    let readonly = dir.path().join("readonly.db");
    std::fs::File::create(&readonly).unwrap();
    cli(
        &format!("sqlite://{}?mode=ro", readonly.display()),
        &["migrate"],
    )
    .assert()
    .code(10)
    .stderr(predicate::str::contains("error[CLI-MIGRATION]"));
}

#[test]
fn password_input_is_bounded_without_accepting_a_truncated_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let url = sqlite_url(&dir);
    cli(&url, &["migrate"]).assert().success();
    for value in [
        format!("Orbit!Cobalt7-River$Quartz{}", "X".repeat(16_385)),
        format!("{}\nignored-tail", "Orbit!Cobalt7-River$Quartz".repeat(700)),
        format!(
            "{}{}TAIL",
            "Orbit!Cobalt7-River$Quartz".repeat(700),
            "\n".repeat(10)
        ),
    ] {
        cli(&url, &create_args())
            .write_stdin(value)
            .assert()
            .code(2)
            .stderr(predicate::str::contains("error[CLI-VALIDATION]"))
            .stdout("");
    }
    cli(&url, &create_args())
        .write_stdin(vec![0xff, 0xfe])
        .assert()
        .code(9)
        .stderr(predicate::str::contains("error[CLI-IO]"));
    cli(&url, &create_args())
        .write_stdin("weak\n")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("weak").not());
}

#[test]
fn cli_adapter_validation_and_missing_members_have_stable_categories() {
    let dir = tempfile::tempdir().unwrap();
    let url = sqlite_url(&dir);
    cli(&url, &["migrate"]).assert().success();
    cli(&url, &["domains", "add", "example.com"])
        .assert()
        .success();
    cli(
        &url,
        &[
            "lists",
            "create",
            "dev.example.com",
            "--display-name",
            "Dev",
        ],
    )
    .assert()
    .success();
    let duplicate = dir.path().join("duplicate.txt");
    std::fs::write(&duplicate, "a@example.com\nA@EXAMPLE.COM\n").unwrap();
    for args in [
        vec!["conf", "--key", "UNKNOWN-KEY-SENTINEL"],
        vec!["members", "find", "INVALID-EMAIL-SENTINEL"],
        vec![
            "members",
            "del",
            "dev.example.com",
            "INVALID-EMAIL-SENTINEL",
        ],
        vec![
            "members",
            "sync",
            "dev.example.com",
            duplicate.to_str().unwrap(),
        ],
    ] {
        cli(&url, &args)
            .assert()
            .code(2)
            .stderr(predicate::str::contains("error[CLI-VALIDATION]"))
            .stderr(predicate::str::contains("SENTINEL").not());
    }
    cli(
        &url,
        &["members", "del", "dev.example.com", "absent@example.com"],
    )
    .assert()
    .code(7)
    .stderr(predicate::str::contains("error[CLI-NOT-FOUND]"));
    for command in ["status", "serve"] {
        cli(&url, &[command])
            .env("LISTMNGR__WEB__LISTEN", "INVALID-LISTEN-SENTINEL")
            .assert()
            .code(2)
            .stderr(predicate::str::contains("error[CLI-VALIDATION]"))
            .stderr(predicate::str::contains("SENTINEL").not());
    }
}

#[test]
fn oversized_password_is_rejected_without_waiting_for_eof() {
    use std::io::Write;
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let dir = tempfile::tempdir().unwrap();
    let url = sqlite_url(&dir);
    cli(&url, &["migrate"]).assert().success();
    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("listmngr"))
        .env_clear()
        .env("LISTMNGR__DATABASE__URL", &url)
        .args(create_args())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    input.write_all(&vec![b'X'; 1027]).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let completed = loop {
        if child.try_wait().unwrap().is_some() {
            break true;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            break false;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let output = child.wait_with_output().unwrap();
    drop(input);
    assert!(
        completed,
        "reader waited for EOF after exceeding its byte budget"
    );
    assert_eq!(output.status.code(), Some(2));
}

#[cfg(unix)]
#[tokio::test]
async fn password_fd_create_and_passwd_persist_and_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let url = sqlite_url(&dir);
    cli(&url, &["migrate"]).assert().success();
    let file = dir.path().join("fixture-password");
    let secret = "Orbit!Cobalt7-River$Quartz";
    std::fs::write(&file, format!("{secret}\n")).unwrap();
    let output = Command::new("sh")
        .env_clear()
        .env("LISTMNGR__DATABASE__URL", &url)
        .args(["-c", "exec 3<\"$1\"; shift; exec \"$@\"", "fixture"])
        .arg(&file)
        .arg(assert_cmd::cargo::cargo_bin("listmngr"))
        .args([
            "user",
            "create",
            "alice@example.com",
            "--display-name",
            "Alice",
            "--password-fd",
            "3",
        ])
        .timeout(std::time::Duration::from_secs(15))
        .assert()
        .success()
        .stdout(predicate::str::contains(secret).not())
        .get_output()
        .clone();
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let id = body["id"].as_str().unwrap();
    let changed = "Nebula!Copper8-Ocean$Jasper";
    cli(&url, &["user", "passwd", id, "--password-fd", "0"])
        .write_stdin(format!("{changed}\n"))
        .assert()
        .success();
    let db = listmngr_db::Database::connect(&url, 1).await.unwrap();
    assert!(
        db.users()
            .verify_password(id.parse().unwrap(), changed)
            .await
            .unwrap()
    );
    for (fd, code) in [("2147483647", 9), ("FD-SECRET", 2)] {
        cli(&url, &["user", "passwd", id, "--password-fd", fd])
            .assert()
            .code(code)
            .stderr(predicate::str::contains(fd).not());
    }
    cli(
        &url,
        &[
            "user",
            "passwd",
            id,
            "--password-fd",
            "0",
            "--password-stdin",
        ],
    )
    .assert()
    .code(2);
    cli(&url, &["user", "passwd", id, "--password-fd", "0"])
        .write_stdin(format!("{}\nTAIL", "X".repeat(1024)))
        .assert()
        .code(2);
    assert!(
        db.users()
            .verify_password(id.parse().unwrap(), changed)
            .await
            .unwrap()
    );
}
