use assert_cmd::Command;
use listmngr_db::Database;
use serde_json::Value;
use std::time::Duration;

#[path = "support/doctor_dns.rs"]
mod dns_fixture;
#[path = "support/doctor_regressions.rs"]
mod regressions;

#[tokio::test]
async fn doctor_checks_mail_domain_dns_with_local_resolver() {
    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let address = socket.local_addr().unwrap();
    let server = tokio::spawn(dns_fixture::serve(socket));
    for (host, success) in [
        ("healthy.test", true),
        ("implicit.test", true),
        ("null.test", false),
        ("missing.test", false),
    ] {
        let (_dir, url) = migrated().await;
        let db = Database::connect(&url, 1).await.unwrap();
        db.domains().create(host, "", None).await.unwrap();
        db.pool().close().await;
        tokio::task::spawn_blocking(move || {
            let result = report(
                command(&url).args(["--dns-server", &address.to_string()]),
                success,
            );
            assert_eq!(status(&result, "dns"), if success { "ok" } else { "fail" });
            let checks = result["checks"].as_array().unwrap();
            assert!(checks.iter().any(|check| check["domain"] == host));
        })
        .await
        .unwrap();
    }
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

fn command(url: &str) -> Command {
    let mut command = Command::cargo_bin("listmngr").unwrap();
    for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("LISTMNGR")) {
        command.env_remove(key);
    }
    command.env_remove("RUST_LOG");
    command
        .arg("doctor")
        .env("LISTMNGR__DATABASE__URL", url)
        .timeout(Duration::from_secs(12));
    command
}

fn report(command: &mut Command, success: bool) -> Value {
    let output = command.output().unwrap();
    assert_eq!(output.status.success(), success, "{output:?}");
    assert_eq!(output.status.code(), Some(if success { 0 } else { 12 }));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("SENTINEL"));
    let value: Value = serde_json::from_slice(&output.stdout).expect("doctor JSON");
    assert_eq!(value["ok"], success);
    value
}

fn status<'a>(report: &'a Value, id: &str) -> &'a str {
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["id"] == id)
        .unwrap()["status"]
        .as_str()
        .unwrap()
}

async fn migrated() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("fixture.sqlite").display()
    );
    let db = Database::connect(&url, 1).await.unwrap();
    db.migrate().await.unwrap();
    db.pool().close().await;
    (dir, url)
}

#[tokio::test]
async fn doctor_checks_smtp_greeting_without_sending_commands() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (_dir, url) = migrated().await;
    for (greeting, success) in [
        ("220-fixture\r\n220 ready\r\n", true),
        ("500 private-SENTINEL\r\n", false),
        ("", false),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = async move {
            tokio::time::timeout(Duration::from_secs(8), async {
                let (mut stream, _) = listener.accept().await.unwrap();
                stream.write_all(greeting.as_bytes()).await.unwrap();
                let mut bytes = Vec::new();
                stream.read_to_end(&mut bytes).await.unwrap();
                assert!(bytes.is_empty(), "doctor sent SMTP commands");
            })
            .await
            .unwrap();
        };
        let url = url.clone();
        let child = tokio::task::spawn_blocking(move || {
            let mut cmd = command(&url);
            cmd.env("LISTMNGR__MTA__ENABLED", "true")
                .env("LISTMNGR__MTA__SMTP_TLS", "plaintext_trusted_relay")
                .env("LISTMNGR__MTA__SMTP_RELAY", address.to_string());
            let result = report(&mut cmd, success);
            assert_eq!(status(&result, "mta"), if success { "ok" } else { "fail" });
            assert!(!result.to_string().contains("SENTINEL"));
        });
        let ((), result) = tokio::join!(server, child);
        result.unwrap();
    }
}

#[tokio::test]
async fn doctor_database_is_read_only_and_requires_current_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("database-SENTINEL.sqlite");
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let missing = report(&mut command(&url), false);
    assert_eq!(status(&missing, "database"), "fail");
    assert!(!path.exists(), "doctor must not create a missing database");
    let db = Database::connect(&url, 1).await.unwrap();
    db.pool().close().await;
    let before = std::fs::read(&path).unwrap();
    let unmigrated = report(&mut command(&url), false);
    assert_eq!(status(&unmigrated, "database"), "fail");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let db = Database::connect(&url, 1).await.unwrap();
    db.migrate().await.unwrap();
    db.pool().close().await;
    let before = std::fs::read(&path).unwrap();
    let healthy = report(&mut command(&url), true);
    assert_eq!(status(&healthy, "database"), "ok");
    assert_eq!(status(&healthy, "mta"), "skip");
    assert_eq!(status(&healthy, "dns"), "skip");
    assert!(!healthy.to_string().contains("SENTINEL"));
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
