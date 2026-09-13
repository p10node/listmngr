use assert_cmd::Command;
use listmngr_core::{MemberRole, SubscriptionMode};
use listmngr_db::{Database, NewList, NewMember};
use serde_json::{Value, json};

fn cli(dir: &std::path::Path, url: &str, args: &[&str]) -> std::process::Output {
    Command::cargo_bin("listmngr")
        .unwrap()
        .env_clear()
        .current_dir(dir)
        .env("LISTMNGR__DATABASE__URL", url)
        .args(args)
        .output()
        .unwrap()
}
#[test]
fn sweep_cli_failure_summary_is_bounded_private_and_advances_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("failed.db").display()
    );
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let db=Database::connect(&url,1).await.unwrap(); db.migrate().await.unwrap();
        db.domains().create("example.invalid","",None).await.unwrap();
        let id="cli.example.invalid".parse().unwrap();
        db.lists().create(NewList{list_id:id,display_name:String::new(),style:"legacy-default".into()}).await.unwrap();
        let id="cli.example.invalid".parse().unwrap();
        db.lists().update(&id,&json!({"process_bounces":true})).await.unwrap();
        for email in ["Failed@Example.invalid","Healthy@Example.invalid"] {
            db.members().create(NewMember{list_id:id.clone(),email:email.into(),role:MemberRole::Member,subscription_mode:SubscriptionMode::AsAddress,display_name:String::new()}).await.unwrap();
        }
        sqlx::query("UPDATE preferences SET delivery_status='by_bounces' WHERE id IN (SELECT preferences_id FROM members)").execute(db.pool()).await.unwrap();
        sqlx::query("UPDATE addresses SET original_email='private-fixture-secret<>@example.invalid' WHERE email='failed@example.invalid'").execute(db.pool()).await.unwrap();
        db.pool().close().await;
    });
    let out = cli(dir.path(), &url, &["bounce", "sweep", "--limit", "2"]);
    assert!(!out.status.success());
    assert!(out.stdout.len() < 1024);
    assert!(out.stderr.len() < 1024);
    for bytes in [&out.stdout, &out.stderr] {
        assert!(!String::from_utf8_lossy(bytes).contains("private-fixture-secret"));
        assert!(!String::from_utf8_lossy(bytes).contains(&url));
    }
    let s: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(s["scanned"], 2);
    assert_eq!(s["failed"], 1);
    assert_eq!(s["warned"], 1);
    let cursor = s["next_cursor"].as_str().unwrap();
    let next = cli(dir.path(), &url, &["bounce", "sweep", "--after", cursor]);
    assert!(next.status.success());
    let next: Value = serde_json::from_slice(&next.stdout).unwrap();
    assert_eq!(next["scanned"], 0);
    assert_eq!(next["next_cursor"], Value::Null);
}

#[test]
fn sweep_cli_publishes_warns_then_removes_on_owned_database() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("sweep.db").display()
    );
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let db = Database::connect(&url,1).await.unwrap(); db.migrate().await.unwrap();
        db.domains().create("example.invalid","",None).await.unwrap();
        let id = "cli.example.invalid".parse().unwrap();
        db.lists().create(NewList { list_id: id, display_name: "CLI".into(), style:"legacy-default".into() }).await.unwrap();
        let id = "cli.example.invalid".parse().unwrap();
        db.lists().update(&id,&json!({"process_bounces":true,"bounce_you_are_disabled_warnings":1,"bounce_you_are_disabled_warnings_interval":0})).await.unwrap();
        db.members().create(NewMember {list_id:id,email:"Original@Example.invalid".into(),role:MemberRole::Member,subscription_mode:SubscriptionMode::AsAddress,display_name:String::new()}).await.unwrap();
        sqlx::query("UPDATE preferences SET delivery_status='by_bounces' WHERE id IN (SELECT preferences_id FROM members)").execute(db.pool()).await.unwrap();
        db.pool().close().await;
    });
    for (warned, removed) in [(1, 0), (0, 1), (0, 0)] {
        let output = cli(dir.path(), &url, &["bounce", "sweep"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let summary: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(summary["warned"], warned);
        assert_eq!(summary["removed"], removed);
        assert_eq!(summary["failed"], 0);
    }
    for args in [
        vec!["bounce", "sweep", "--limit", "0"],
        vec!["bounce", "sweep", "--limit", "1001"],
        vec!["bounce", "sweep", "--after", "bad"],
        vec!["bounce", "sweep", "--now", "2026-01-01"],
    ] {
        assert_eq!(cli(dir.path(), &url, &args).status.code(), Some(2));
    }
    rt.block_on(async {
        let db = Database::connect(&url, 1).await.unwrap();
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workflow_notices")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(n, 1);
        let recipient: String = sqlx::query_scalar("SELECT email FROM delivery_recipients")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(recipient, "Original@Example.invalid");
        db.pool().close().await;
    });
}
