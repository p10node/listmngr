use assert_cmd::Command;
use predicates::prelude::*;
use sqlx::Row;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::thread;

fn sqlite_url(dir: &tempfile::TempDir) -> String {
    format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("listmngr.db").display()
    )
}

fn cli(url: &str, args: &[&str]) -> Command {
    let mut command = Command::cargo_bin("listmngr").unwrap();
    command.env("LISTMNGR__DATABASE__URL", url).args(args);
    command
}

fn write_status_config(path: &Path, address: std::net::SocketAddr) {
    std::fs::write(
        path,
        format!(
            "[database]\nurl = \"postgres://user:never-print-this@invalid/db\"\n[web]\nlisten = \"{address}\"\n"
        ),
    )
    .unwrap();
}

fn status_server(statuses: &[u16]) -> (std::net::SocketAddr, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let statuses = statuses.to_vec();
    let handle = thread::spawn(move || {
        for status in statuses {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).unwrap();
            let reason = if status == 200 {
                "OK"
            } else {
                "Service Unavailable"
            };
            write!(
                stream,
                "HTTP/1.1 {status} {reason}\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
            )
            .unwrap();
        }
    });
    (address, handle)
}

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
        "--password-stdin",
    ])
    .write_stdin("long enough password\n")
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
    .failure()
    .stderr("error[CLI-VALIDATION]: invalid input\n");
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

fn run_status(address: std::net::SocketAddr) -> assert_cmd::assert::Assert {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("status.toml");
    write_status_config(&config, address);
    Command::cargo_bin("listmngr")
        .unwrap()
        .args(["--config", config.to_str().unwrap(), "status"])
        .assert()
}

#[test]
fn status_uses_http_health_and_readiness_without_opening_the_database() {
    let (address, server) = status_server(&[200, 200]);
    run_status(address)
        .success()
        .stdout("service: healthy and ready\n")
        .stderr(predicate::str::contains("never-print-this").not());
    server.join().unwrap();
}

#[test]
fn status_distinguishes_unreachable_unhealthy_and_not_ready() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let unreachable = listener.local_addr().unwrap();
    drop(listener);
    run_status(unreachable)
        .code(3)
        .stderr("error[CLI-STATUS-UNREACHABLE]: service is unreachable\n");

    for (responses, code, id, message) in [
        (
            &[503][..],
            4,
            "CLI-STATUS-UNHEALTHY",
            "service is unhealthy",
        ),
        (
            &[200, 503][..],
            5,
            "CLI-STATUS-NOT-READY",
            "service is healthy but not ready",
        ),
    ] {
        let (address, server) = status_server(responses);
        run_status(address)
            .code(code)
            .stderr(format!("error[{id}]: {message}\n"));
        server.join().unwrap();
    }
}

fn user_create_args<'a>(password_input: &'a [&'a str]) -> Vec<&'a str> {
    let mut args = vec![
        "user",
        "create",
        "alice@example.com",
        "--display-name",
        "Alice",
    ];
    args.extend_from_slice(password_input);
    args
}

#[test]
fn passwords_are_read_from_stdin_and_never_rendered() {
    let dir = tempfile::tempdir().unwrap();
    let url = sqlite_url(&dir);
    cli(&url, &["migrate"]).assert().success();
    let secret = "correct horse battery staple";
    cli(&url, &user_create_args(&["--password-stdin"]))
        .write_stdin(format!("{secret}\n"))
        .assert()
        .success()
        .stdout(predicate::str::contains(secret).not())
        .stderr(predicate::str::contains(secret).not());

    cli(&url, &user_create_args(&["--password", secret]))
        .assert()
        .code(2)
        .stdout(predicate::str::contains(secret).not())
        .stderr(predicate::str::contains(secret).not())
        .stderr(predicate::str::contains("error[CLI-USAGE]"));
}

#[cfg(unix)]
#[test]
fn password_file_descriptor_supports_automation_without_argv_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let url = sqlite_url(&dir);
    cli(&url, &["migrate"]).assert().success();
    let secret_file = dir.path().join("password");
    std::fs::write(&secret_file, "another correct horse password\n").unwrap();
    let binary = assert_cmd::cargo::cargo_bin("listmngr");
    Command::new("sh")
        .env("LISTMNGR__DATABASE__URL", url)
        .args([
            "-c",
            "exec 3<\"$PASSWORD_FILE\"; exec \"$BINARY\" user create bob@example.com --display-name Bob --password-fd 3",
        ])
        .env("PASSWORD_FILE", &secret_file)
        .env("BINARY", binary)
        .assert()
        .success()
        .stdout(predicate::str::contains("another correct horse password").not())
        .stderr(predicate::str::contains("another correct horse password").not());
}

fn create_user(url: &str, email: &str) -> listmngr_core::UserId {
    let output = cli(
        url,
        &[
            "user",
            "create",
            email,
            "--display-name",
            "Fixture User",
            "--password-stdin",
        ],
    )
    .write_stdin("correct horse battery staple\n")
    .output()
    .unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    body["id"].as_str().unwrap().parse().unwrap()
}

#[test]
fn duplicate_resources_and_nonempty_domain_delete_have_stable_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    let url = sqlite_url(&dir);
    cli(&url, &["migrate"]).assert().success();
    cli(&url, &["domains", "add", "example.com"])
        .assert()
        .success();
    for args in [
        vec!["domains", "add", "example.com"],
        vec![
            "lists",
            "create",
            "dev.example.com",
            "--display-name",
            "Dev Again",
        ],
    ] {
        if args[0] == "lists" {
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
        }
        cli(&url, &args)
            .assert()
            .code(6)
            .stderr("error[CLI-CONFLICT]: operation conflicts with existing data\n");
    }
    create_user(&url, "duplicate@example.com");
    cli(
        &url,
        &[
            "user",
            "create",
            "duplicate@example.com",
            "--display-name",
            "Duplicate",
            "--password-stdin",
        ],
    )
    .write_stdin("correct horse battery staple\n")
    .assert()
    .code(6)
    .stderr("error[CLI-CONFLICT]: operation conflicts with existing data\n");
    cli(&url, &["domains", "rm", "example.com"])
        .assert()
        .code(6)
        .stderr("error[CLI-CONFLICT]: operation conflicts with existing data\n");
}

#[test]
fn invalid_identifiers_idna_email_and_password_are_redacted_and_nonzero() {
    let dir = tempfile::tempdir().unwrap();
    let url = sqlite_url(&dir);
    cli(&url, &["migrate"]).assert().success();
    for args in [
        vec!["domains", "add", "bad domain"],
        vec!["members", "find", "not-an-email"],
    ] {
        cli(&url, &args)
            .assert()
            .code(2)
            .stderr("error[CLI-VALIDATION]: invalid input\n");
    }
    cli(&url, &["lists", "remove", "missing-domain"])
        .assert()
        .code(2)
        .stderr("error[CLI-USAGE]: invalid command line\n");
    cli(
        &url,
        &[
            "user",
            "create",
            "weak@example.com",
            "--display-name",
            "Weak",
            "--password-stdin",
        ],
    )
    .write_stdin("weak\n")
    .assert()
    .code(2)
    .stderr("error[CLI-VALIDATION]: invalid input\n")
    .stdout(predicate::str::contains("weak").not());
}

#[test]
fn database_and_migration_failures_are_stable_and_do_not_leak_credentials() {
    let secret = "database-fixture-credential";
    cli(
        &format!("postgres://user:{secret}@127.0.0.1:1/listmngr"),
        &["domains", "ls"],
    )
    .assert()
    .code(10)
    .stderr("error[CLI-DATABASE]: database operation failed\n")
    .stderr(predicate::str::contains(secret).not());

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("readonly.db");
    std::fs::File::create(&path).unwrap();
    let url = format!("sqlite://{}?mode=ro", path.display());
    cli(&url, &["migrate"])
        .assert()
        .code(10)
        .stderr("error[CLI-MIGRATION]: database migration failed\n");
}

#[tokio::test]
async fn token_is_printed_once_stored_hash_only_and_revoke_or_expiry_rejects_it() {
    let dir = tempfile::tempdir().unwrap();
    let url = sqlite_url(&dir);
    cli(&url, &["migrate"]).assert().success();
    let user = create_user(&url, "token@example.com");
    let output = cli(&url, &["token", "create", &user.to_string(), "automation"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let token = String::from_utf8(output.stdout).unwrap();
    let token = token.trim();
    assert_eq!(token.matches("lm_").count(), 1);

    let db = listmngr_db::Database::connect(&url, 1).await.unwrap();
    let row = sqlx::query("SELECT id,token_hash FROM api_tokens")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let id: listmngr_core::TokenId = row.get::<String, _>("id").parse().unwrap();
    let hash: String = row.get("token_hash");
    assert_eq!(hash.len(), 64);
    assert!(!hash.contains(token));
    db.tokens().authenticate_without_usage(token).await.unwrap();
    cli(&url, &["token", "revoke", &id.to_string()])
        .assert()
        .success();
    assert!(db.tokens().authenticate_without_usage(token).await.is_err());

    let second = db
        .tokens()
        .create(user, "expired", &["system:read"], None)
        .await
        .unwrap();
    sqlx::query("UPDATE api_tokens SET expires_at=? WHERE id=?")
        .bind("1970-01-01T00:00:00Z")
        .bind(second.id.to_string())
        .execute(db.pool())
        .await
        .unwrap();
    assert!(
        db.tokens()
            .authenticate_without_usage(&second.token)
            .await
            .is_err()
    );
}

#[cfg(unix)]
#[test]
fn config_precedence_applies_environment_then_secure_secret_file() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let secret = dir.path().join("database-url");
    std::fs::write(&secret, "postgres://user:fixture-secret@db/listmngr\n").unwrap();
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
    let config = dir.path().join("listmngr.toml");
    std::fs::write(
        &config,
        format!(
            "[database]\nurl = \"sqlite://file.db\"\nurl_file = {secret:?}\n[web]\nlisten = \"127.0.0.1:7000\"\n"
        ),
    )
    .unwrap();
    Command::cargo_bin("listmngr")
        .unwrap()
        .env("LISTMNGR__DATABASE__URL", "sqlite://environment.db")
        .env("LISTMNGR__WEB__LISTEN", "127.0.0.1:7001")
        .args(["--config", config.to_str().unwrap(), "info"])
        .assert()
        .success()
        .stdout(predicate::str::contains("database: postgresql"))
        .stdout(predicate::str::contains("web: 127.0.0.1:7001"))
        .stdout(predicate::str::contains("fixture-secret").not())
        .stderr(predicate::str::contains("fixture-secret").not());
}
