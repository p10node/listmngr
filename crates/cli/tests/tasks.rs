//! `listmngr tasks run` and `listmngr notify`: the sweep and the reminder
//! Mailman's task runner and `notify` command provide, run by hand.
use assert_cmd::Command;
use listmngr_core::{MemberRole, SubscriptionMode};
use listmngr_db::workflows::SubscriptionAction;
use listmngr_db::{Database, NewList, NewMember};
use predicates::prelude::*;
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

/// A list with a moderated join waiting, an owner to tell, and an old,
/// unanswered confirmation of a second list to expire.
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
        db.members()
            .create(NewMember {
                list_id: list.id.clone(),
                email: "owner@example.invalid".into(),
                display_name: String::new(),
                role: MemberRole::Owner,
                subscription_mode: SubscriptionMode::AsAddress,
            })
            .await
            .unwrap();
        db.workflows()
            .request(
                &list.id,
                "joiner@example.net",
                SubscriptionAction::Join,
                chrono::Utc::now().timestamp_millis(),
            )
            .await
            .unwrap();
        // An unanswered confirmation from long ago on a confirm list.
        let old = db
            .lists()
            .create(NewList {
                list_id: "old.example.invalid".parse().unwrap(),
                display_name: "Old".into(),
                style: "legacy-default".into(),
            })
            .await
            .unwrap();
        db.workflows()
            .request(
                &old.id,
                "late@example.net",
                SubscriptionAction::Join,
                chrono::Utc::now().timestamp_millis() - 10 * 86_400_000,
            )
            .await
            .unwrap();
        db.pool().close().await;
    });
    url
}

async fn count(url: &str, sql: &str) -> i64 {
    let db = Database::connect(url, 1).await.unwrap();
    let count = sqlx::query_scalar(sql).fetch_one(db.pool()).await.unwrap();
    db.pool().close().await;
    count
}

#[test]
fn the_sweep_runs_once_and_reports_what_it_did() {
    let root = tempfile::tempdir().unwrap();
    let url = setup(root.path());
    let output = command(root.path(), &url)
        .args(["tasks", "run"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let summary: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(summary["expired_workflows"], 1, "{summary}");
    assert_eq!(summary["collected_jobs"], 0, "{summary}");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    assert_eq!(
        runtime.block_on(count(
            &url,
            "SELECT COUNT(*) FROM subscription_workflows WHERE state='pending_moderation'"
        )),
        1,
        "the moderated request outlives the sweep"
    );
    // A retention below the floor is refused at configuration load.
    command(root.path(), &url)
        .env("LISTMNGR__MAILMAN__FINISHED_JOB_RETENTION_SECS", "10")
        .args(["tasks", "run"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("CLI-VALIDATION"));
}

#[test]
fn notify_reminds_the_owners_and_a_dry_run_only_reports() {
    let root = tempfile::tempdir().unwrap();
    let url = setup(root.path());
    command(root.path(), &url)
        .args(["notify", "--dry-run"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains(
                r#"dev.example.invalid: {"held_messages":0,"subscriptions":1,"unsubscriptions":0}"#,
            )
            .and(predicate::str::contains("old.example.invalid").not()),
        );
    let runtime = tokio::runtime::Runtime::new().unwrap();
    assert_eq!(
        runtime.block_on(count(&url, "SELECT COUNT(*) FROM workflow_notices")),
        1,
        "only the old list's confirmation mail so far"
    );
    command(root.path(), &url)
        .args(["notify", "--list", LIST])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 list(s) notified"));
    assert_eq!(
        runtime.block_on(count(&url, "SELECT COUNT(*) FROM workflow_notices")),
        2,
        "the owner's reminder is queued"
    );
}
