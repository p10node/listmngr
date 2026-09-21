//! `listmngr import3` on the real binary, against a server that answers
//! what a real Mailman 3.3.10 core answered (the recorded fixtures of
//! `crates/import/tests/fixtures/mailman3`).
use assert_cmd::Command;
use listmngr_db::Database;
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../import/tests/fixtures/mailman3")
}

/// The recorded answer for one REST path, or an empty collection.
fn answer(path: &str) -> String {
    let read = |name: &str| std::fs::read_to_string(fixtures().join(format!("{name}.json"))).ok();
    let body = match path {
        "domains" => read("domains"),
        "bans" => read("bans-global"),
        "users" => read("users"),
        path if path.starts_with("users/") => {
            let (id, tail) = path
                .trim_start_matches("users/")
                .split_once('/')
                .unwrap_or(("", ""));
            let file = match tail {
                "addresses" => "user-addresses",
                "preferences" => "user-preferences",
                _ => "user-preferred",
            };
            let all: serde_json::Value =
                serde_json::from_str(&read(file).unwrap_or_default()).unwrap_or_default();
            let found = all.get(id).cloned().unwrap_or(serde_json::Value::Null);
            if tail == "preferred_address" && found.is_null() {
                return "404".to_owned();
            }
            Some(found.to_string())
        }
        path if path.starts_with("lists?") => read("lists"),
        path if path.starts_with("members/") => {
            let id = path
                .trim_start_matches("members/")
                .trim_end_matches("/preferences");
            let all: serde_json::Value =
                serde_json::from_str(&read("preferences").unwrap_or_default()).unwrap_or_default();
            Some(
                all.get(id)
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}))
                    .to_string(),
            )
        }
        path => {
            let rest = path.trim_start_matches("lists/");
            let (list, tail) = rest.split_once('/').unwrap_or((rest, ""));
            let prefix = if list == "announce.other.invalid" {
                "announce-"
            } else {
                ""
            };
            match tail {
                "config" => read(&format!("{prefix}config")),
                "bans" => read(&format!(
                    "{prefix}bans{}",
                    if prefix.is_empty() { "-list" } else { "" }
                )),
                "header-matches" => read(&format!("{prefix}header-matches")),
                "held" => read(&format!("{prefix}held")),
                "requests" => read(&format!("{prefix}requests")),
                "uris" => read(&format!("{prefix}uris")),
                tail if tail.starts_with("roster/") => {
                    let role = tail.trim_start_matches("roster/");
                    read(&format!("{prefix}roster-{role}"))
                        .or_else(|| read("announce-roster-member"))
                }
                _ => None,
            }
        }
    };
    body.unwrap_or_else(|| r#"{"http_etag": "\"z\"", "start": 0, "total_size": 0}"#.to_owned())
}

/// A core on a free port; every request must carry the credentials.
async fn core() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buffer = vec![0; 8192];
                let read = stream.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
                let target = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/")
                    .to_owned();
                let authorized = request
                    .to_lowercase()
                    .contains("authorization: basic cmvzdgfkbwluonbhc3n3b3jkagvyzq==");
                let body = answer(target.trim_start_matches("/3.1/"));
                let response = if !authorized {
                    "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_owned()
                } else if body == "404" {
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_owned()
                } else {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                };
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
            });
        }
    });
    port
}

#[test]
fn import3_plans_dry_then_writes_the_site_it_read() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("import3.db").display()
    );
    let password = dir.path().join("rest.pass");
    // restadmin:passwordhere
    std::fs::write(&password, "passwordhere\n").unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let port = rt.block_on(async {
        let db = Database::connect(&url, 1).await.unwrap();
        db.migrate().await.unwrap();
        db.pool().close().await;
        core().await
    });
    let root = format!("http://127.0.0.1:{port}/3.1");
    let run = |args: Vec<&str>| {
        let mut command = Command::cargo_bin("listmngr").unwrap();
        command
            .env_clear()
            .current_dir(dir.path())
            .env("LISTMNGR__DATABASE__URL", &url)
            .arg("import3")
            .arg("--rest")
            .arg(&root)
            .arg("--password-file")
            .arg(&password);
        for arg in args {
            command.arg(arg);
        }
        command.output().unwrap()
    };
    // A wrong password file is refused, and the password is not printed.
    let wrong = dir.path().join("wrong.pass");
    std::fs::write(&wrong, "hunter2hunter2\n").unwrap();
    let output = Command::cargo_bin("listmngr")
        .unwrap()
        .env_clear()
        .current_dir(dir.path())
        .env("LISTMNGR__DATABASE__URL", &url)
        .args(["import3", "--rest", &root, "--password-file"])
        .arg(&wrong)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(11), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error[CLI-IMPORT-SOURCE]"), "{stderr}");
    assert!(!stderr.contains("hunter2hunter2"), "{stderr}");
    // The dry run prints the plan and writes nothing.
    let output = run(vec!["--dry-run"]);
    assert!(output.status.success(), "{output:?}");
    let plan: serde_json::Value = serde_json::from_slice(output.stdout.trim_ascii()).unwrap();
    assert_eq!(plan["domains"][0]["mail_host"], "example.invalid");
    assert_eq!(plan["lists"].as_array().unwrap().len(), 2);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("warning: "),
        "{output:?}"
    );
    // One list only.
    let output = run(vec!["--list", "rust-users.example.invalid", "--dry-run"]);
    assert!(output.status.success(), "{output:?}");
    let plan: serde_json::Value = serde_json::from_slice(output.stdout.trim_ascii()).unwrap();
    assert_eq!(plan["lists"].as_array().unwrap().len(), 1);
    // The real run reports what it wrote, and a second one skips it all.
    let output = run(vec![]);
    assert!(output.status.success(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(output.stdout.trim_ascii()).unwrap();
    assert_eq!(report["domains"], 2);
    assert_eq!(report["users"], 9);
    assert_eq!(report["lists"], 2);
    assert_eq!(report["members"], 4);
    assert_eq!(report["owners"], 1);
    assert_eq!(report["site_bans"], 1);
    assert_eq!(report["header_matches"], 2);
    assert_eq!(report["held"], 1);
    assert_eq!(report["requests"], 1);
    let output = run(vec![]);
    assert!(output.status.success(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(output.stdout.trim_ascii()).unwrap();
    assert_eq!(report["lists"], 0);
    assert_eq!(report["members"], 0);
    assert_eq!(report["skipped"], 22);
    drop(rt);
}
