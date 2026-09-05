use assert_cmd::Command;
use predicates::prelude::*;
use sha2::{Digest, Sha256};
use sqlx::Row;

fn database_url(dir: &tempfile::TempDir) -> String {
    format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("fixture.db").display()
    )
}

fn cli(dir: &tempfile::TempDir, args: &[&str]) -> Command {
    let mut command = Command::cargo_bin("listmngr").unwrap();
    command
        .current_dir(dir.path())
        .env_clear()
        .env("LISTMNGR__DATABASE__URL", database_url(dir))
        .args(args)
        .timeout(std::time::Duration::from_secs(15));
    command
}

fn create_user(dir: &tempfile::TempDir) -> String {
    let output = cli(
        dir,
        &[
            "user",
            "create",
            "user@example.com",
            "--display-name",
            "Fixture",
            "--password-stdin",
        ],
    )
    .write_stdin("Orbit!Cobalt7-River$Quartz\n")
    .assert()
    .success()
    .get_output()
    .clone();
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    body["id"].as_str().unwrap().to_owned()
}

#[test]
fn duplicate_resources_and_nonempty_domain_delete_have_stable_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    cli(&dir, &["migrate"]).assert().success();
    cli(&dir, &["domains", "add", "example.com"])
        .assert()
        .success();
    cli(
        &dir,
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
    create_user(&dir);
    for args in [
        vec!["domains", "add", "example.com"],
        vec![
            "lists",
            "create",
            "dev.example.com",
            "--display-name",
            "Duplicate",
        ],
        vec![
            "user",
            "create",
            "user@example.com",
            "--display-name",
            "Duplicate",
            "--password-stdin",
        ],
        vec!["domains", "rm", "example.com"],
    ] {
        cli(&dir, &args)
            .write_stdin("Orbit!Cobalt7-River$Quartz\n")
            .assert()
            .code(6)
            .stderr(predicate::str::contains("error[CLI-CONFLICT]"))
            .stderr(predicate::str::contains("Orbit!Cobalt7-River$Quartz").not());
    }
    cli(&dir, &["lists", "ls"])
        .assert()
        .success()
        .stdout("dev.example.com\n");
}

#[test]
fn member_lookup_and_delete_use_canonical_idna_addresses() {
    let dir = tempfile::tempdir().unwrap();
    cli(&dir, &["migrate"]).assert().success();
    cli(&dir, &["domains", "add", "example.com"])
        .assert()
        .success();
    cli(
        &dir,
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
    let output = cli(
        &dir,
        &["members", "add", "dev.example.com", "User@bücher.example"],
    )
    .assert()
    .success()
    .get_output()
    .clone();
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let expected = format!("{} dev.example.com member\n", body["id"].as_str().unwrap());
    for address in ["USER@BÜCHER.EXAMPLE", "user@xn--bcher-kva.example"] {
        cli(&dir, &["members", "find", address])
            .assert()
            .success()
            .stdout(expected.clone());
    }
    cli(&dir, &["members", "find", "other@bücher.example"])
        .assert()
        .success()
        .stdout("");
    cli(
        &dir,
        &["members", "del", "dev.example.com", "USER@BÜCHER.EXAMPLE"],
    )
    .assert()
    .success();
    cli(&dir, &["members", "find", "user@xn--bcher-kva.example"])
        .assert()
        .success()
        .stdout("");
}

#[tokio::test]
async fn cli_token_is_emitted_once_hashed_and_revocation_or_expiry_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    cli(&dir, &["migrate"]).assert().success();
    let user = create_user(&dir);
    let output = cli(&dir, &["token", "create", &user, "fixture"])
        .assert()
        .success()
        .stderr("")
        .get_output()
        .clone();
    let printed = String::from_utf8(output.stdout).unwrap();
    assert_eq!(printed.lines().count(), 1);
    let token = printed.trim();
    let parts = token.splitn(3, '_').collect::<Vec<_>>();
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0], "lm");
    let db = listmngr_db::Database::connect(&database_url(&dir), 1)
        .await
        .unwrap();
    let row = sqlx::query("SELECT id,token_hash FROM api_tokens")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let id: String = row.get("id");
    let hash: String = row.get("token_hash");
    assert_eq!(id, parts[1]);
    assert_eq!(hash, format!("{:x}", Sha256::digest(parts[2].as_bytes())));
    assert_ne!(hash, parts[2]);
    assert_eq!(
        db.tokens()
            .authenticate_without_usage(token)
            .await
            .unwrap()
            .user_id
            .to_string(),
        user
    );
    assert!(
        db.tokens()
            .authenticate_without_usage(&format!("{token}tampered"))
            .await
            .is_err()
    );
    cli(&dir, &["token", "revoke", &id])
        .assert()
        .success()
        .stdout(predicate::str::contains(parts[2]).not())
        .stderr("");
    assert!(db.tokens().authenticate_without_usage(token).await.is_err());
    // Expiration belongs to the repository contract; create an already-expired
    // token in this disposable fixture instead of changing development data/time.
    let expired = db
        .tokens()
        .create(
            user.parse().unwrap(),
            "expired",
            &["system:read"],
            Some("1970-01-01T00:00:00Z".parse().unwrap()),
        )
        .await
        .unwrap();
    assert!(
        db.tokens()
            .authenticate_without_usage(&expired.token)
            .await
            .is_err()
    );
    db.pool().close().await;
}
