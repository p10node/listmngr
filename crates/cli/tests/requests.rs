//! `listmngr requests`: the moderator queue for subscription requests on a
//! list whose policy holds them.
use assert_cmd::Command;
use listmngr_db::workflows::SubscriptionAction;
use listmngr_db::{Database, NewList};
use predicates::prelude::PredicateBooleanExt as _;
use std::path::Path;

fn command(root: &Path, url: &str) -> Command {
    let mut command = Command::cargo_bin("listmngr").unwrap();
    command
        .env_clear()
        .current_dir(root)
        .env("LISTMNGR__DATABASE__URL", url);
    command
}

const LIST: &str = "dev.example.invalid";

fn setup(root: &Path) -> String {
    let url = format!("sqlite://{}?mode=rwc", root.join("fixture.db").display());
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let db = Database::connect(&url, 1).await.unwrap();
        db.migrate().await.unwrap();
        db.domains()
            .create("example.invalid", "", None)
            .await
            .unwrap();
        let list = db
            .lists()
            .create(NewList {
                list_id: LIST.parse().unwrap(),
                display_name: "Dev".into(),
                style: "legacy-default".into(),
            })
            .await
            .unwrap();
        db.lists()
            .update(
                &list.id,
                &serde_json::json!({"subscription_policy": "moderate"}),
            )
            .await
            .unwrap();
        for email in ["first@example.invalid", "second@example.invalid"] {
            db.workflows()
                .request(
                    &list.id,
                    email,
                    SubscriptionAction::Join,
                    chrono::Utc::now().timestamp_millis(),
                )
                .await
                .unwrap();
        }
        db.pool().close().await;
    });
    url
}

fn requests(root: &Path, url: &str) -> Vec<serde_json::Value> {
    let output = command(root, url)
        .args(["requests", "ls", LIST])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn a_moderator_lists_and_decides_waiting_requests() {
    let root = tempfile::tempdir().unwrap();
    let url = setup(root.path());
    let waiting = requests(root.path(), &url);
    assert_eq!(waiting.len(), 2);
    assert_eq!(waiting[0]["email"], "first@example.invalid");
    assert_eq!(waiting[0]["action"], "join");
    assert_eq!(waiting[0]["list_id"], LIST);

    let first = waiting[0]["id"].as_str().unwrap();
    command(root.path(), &url)
        .args(["requests", "accept", first])
        .assert()
        .success()
        .stdout(predicates::str::contains("accept"));
    let second = waiting[1]["id"].as_str().unwrap().to_owned();
    command(root.path(), &url)
        .args(["requests", "reject", &second])
        .assert()
        .success();
    assert!(requests(root.path(), &url).is_empty());

    command(root.path(), &url)
        .args(["members", "ls", LIST])
        .assert()
        .success()
        .stdout(predicates::str::contains("first@example.invalid"))
        .stdout(predicates::str::contains("second@example.invalid").not());

    // A decided request is gone: deciding it again is a not-found exit code.
    command(root.path(), &url)
        .args(["requests", "accept", first])
        .assert()
        .code(7);
}
