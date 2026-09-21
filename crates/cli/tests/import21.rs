//! `listmngr import21` on the real binary: a Mailman 2.1 `config.pck`
//! planned with `--dry-run`, then applied to an existing list.
use assert_cmd::Command;
use listmngr_db::{Database, NewList};
use std::path::PathBuf;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../import/tests/fixtures/mailman21-full.pck")
}

#[test]
fn import21_plans_dry_then_applies_the_pickle_to_the_list() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("import.db").display()
    );
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let db = Database::connect(&url, 1).await.unwrap();
        db.migrate().await.unwrap();
        db.domains()
            .create("example.invalid", "", None)
            .await
            .unwrap();
        db.lists()
            .create(NewList {
                list_id: "rust-users.example.invalid".parse().unwrap(),
                display_name: "Rust".into(),
                style: "legacy-default".into(),
            })
            .await
            .unwrap();
        db.pool().close().await;
    });
    // A list that does not exist is refused before anything is read in.
    Command::cargo_bin("listmngr")
        .unwrap()
        .env_clear()
        .current_dir(dir.path())
        .env("LISTMNGR__DATABASE__URL", &url)
        .arg("import21")
        .arg("nobody.example.invalid")
        .arg(fixture())
        .assert()
        .failure();
    // The dry run prints the plan and its warnings and changes nothing.
    let output = Command::cargo_bin("listmngr")
        .unwrap()
        .env_clear()
        .current_dir(dir.path())
        .env("LISTMNGR__DATABASE__URL", &url)
        .arg("import21")
        .arg("rust-users.example.invalid")
        .arg(fixture())
        .arg("--dry-run")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let plan: serde_json::Value = serde_json::from_slice(output.stdout.trim_ascii()).unwrap();
    assert_eq!(plan["settings"]["subject_prefix"], "[Rust] ");
    assert_eq!(plan["settings"]["preferred_language"], "vi");
    assert_eq!(plan["bans"].as_array().unwrap().len(), 2);
    assert_eq!(plan["members"][0]["email"], "alice@example.invalid");
    assert_eq!(plan["members"][0]["role"], "member");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("warning: "), "{stderr}");
    assert!(stderr.contains("^[unclosed"), "{stderr}");
    // The real run reports what it did.
    let output = Command::cargo_bin("listmngr")
        .unwrap()
        .env_clear()
        .current_dir(dir.path())
        .env("LISTMNGR__DATABASE__URL", &url)
        .arg("import21")
        .arg("rust-users.example.invalid")
        .arg(fixture())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(output.stdout.trim_ascii()).unwrap();
    assert_eq!(report["members"], 5);
    assert_eq!(report["owners"], 2);
    assert_eq!(report["moderators"], 1);
    assert_eq!(report["nonmembers"], 3);
    assert_eq!(report["bans"], 2);
    assert_eq!(report["skipped"], 0);
    // A second run keeps the existing subscriptions and reports them skipped.
    let output = Command::cargo_bin("listmngr")
        .unwrap()
        .env_clear()
        .current_dir(dir.path())
        .env("LISTMNGR__DATABASE__URL", &url)
        .arg("import21")
        .arg("rust-users.example.invalid")
        .arg(fixture())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(output.stdout.trim_ascii()).unwrap();
    assert_eq!(report["members"], 0);
    assert_eq!(report["nonmembers"], 0);
    assert_eq!(report["skipped"], 5 + 2 + 1 + 3);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("^[unclosed"), "{stderr}");
    drop(rt);
}
