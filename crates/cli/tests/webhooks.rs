//! `listmngr webhooks` on the real binary: a webhook made with its secret
//! printed once, listed, changed, pinged, rotated and removed.
use assert_cmd::Command;
use serde_json::Value;

const KEY: &str = "0123456789abcdef0123456789abcdef";

fn command(dir: &std::path::Path, url: &str, key: Option<&str>) -> Command {
    let mut command = Command::cargo_bin("listmngr").unwrap();
    command
        .env_clear()
        .current_dir(dir)
        .env("LISTMNGR__DATABASE__URL", url);
    if let Some(key) = key {
        command.env("LISTMNGR__WEBHOOKS__SIGNING_KEY", key);
    }
    command
}

fn json_lines(output: &std::process::Output) -> Vec<Value> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn webhooks_are_managed_from_the_command_line() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("webhooks.db").display()
    );
    command(dir.path(), &url, Some(KEY))
        .arg("migrate")
        .assert()
        .success();
    // Without a signing key nothing can be created.
    let output = command(dir.path(), &url, None)
        .args(["webhooks", "add", "https://hooks.example.invalid/x"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    // Made, with the secret printed once.
    let output = command(dir.path(), &url, Some(KEY))
        .args([
            "webhooks",
            "add",
            "https://hooks.example.invalid/ops",
            "--events",
            "list.*,member.create",
            "--description",
            "ops",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let created = json_lines(&output).remove(0);
    let id = created["id"].as_str().unwrap().to_owned();
    let secret = created["secret"].as_str().unwrap().to_owned();
    assert_eq!(secret.len(), 64);
    assert_eq!(
        created["events"],
        serde_json::json!(["list.*", "member.create"])
    );
    assert_eq!(created["description"], "ops");
    assert_eq!(created["enabled"], true);
    // Listed without the secret.
    let output = command(dir.path(), &url, Some(KEY))
        .args(["webhooks", "ls"])
        .output()
        .unwrap();
    let listed = json_lines(&output);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["id"], id);
    assert!(listed[0].get("secret").is_none());
    // Changed.
    let output = command(dir.path(), &url, Some(KEY))
        .args(["webhooks", "set", &id, "--enabled", "false"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(json_lines(&output)[0]["enabled"], false);
    // Pinged, and the delivery listed.
    let output = command(dir.path(), &url, Some(KEY))
        .args(["webhooks", "ping", &id])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let ping = json_lines(&output).remove(0);
    assert_eq!(ping["event"], "ping");
    assert_eq!(ping["state"], "pending");
    let output = command(dir.path(), &url, Some(KEY))
        .args(["webhooks", "deliveries", &id])
        .output()
        .unwrap();
    let deliveries = json_lines(&output);
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0]["id"], ping["id"]);
    // Rotated: a new secret, printed once.
    let output = command(dir.path(), &url, Some(KEY))
        .args(["webhooks", "rotate", &id])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let rotated = json_lines(&output).remove(0);
    assert_ne!(rotated["secret"], secret);
    assert_ne!(rotated["secret_fingerprint"], created["secret_fingerprint"]);
    // Refused what it cannot take.
    let output = command(dir.path(), &url, Some(KEY))
        .args(["webhooks", "add", "http://hooks.example.invalid/x"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    // Removed.
    command(dir.path(), &url, Some(KEY))
        .args(["webhooks", "rm", &id])
        .assert()
        .success();
    let output = command(dir.path(), &url, Some(KEY))
        .args(["webhooks", "ls"])
        .output()
        .unwrap();
    assert!(json_lines(&output).is_empty());
    let output = command(dir.path(), &url, Some(KEY))
        .args(["webhooks", "ping", &id])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7), "{output:?}");
}
