use assert_cmd::Command;
use listmngr_db::{Database, NewList};
use listmngr_mail::lmtp::LmtpHandler;
use listmngr_runners::InboundHandler;
use std::{path::Path, time::Duration};

const REPORT: &[u8] = b"From: Mailer Daemon <daemon@example.invalid>\r\nAuto-Submitted: auto-generated\r\nSubject: delivery failure\r\n\r\nprivate-report-contents\x00\xff\r\n";

fn command(root: &Path, url: &str) -> Command {
    let mut command = Command::cargo_bin("listmngr").unwrap();
    command
        .env_clear()
        .current_dir(root)
        .env("LISTMNGR__DATABASE__URL", url);
    command
}

fn acknowledge_and_reopen(root: &Path, url: &str, id: &str) {
    command(root, url)
        .args([
            "queue",
            "acknowledge-bounce",
            id,
            "--reason",
            " reviewed report ",
        ])
        .assert()
        .success();
    let done = command(root, url)
        .args(["queue", "show", id])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let job: serde_json::Value = serde_json::from_slice(&done).unwrap();
    assert_eq!(job["state"], "done");
    assert_eq!(job["queue"], "bounces");
    command(root, url)
        .args(["queue", "acknowledge-bounce", id, "--reason", "repeat"])
        .assert()
        .failure();
    assert_eq!(
        command(root, url)
            .args(["queue", "show", id])
            .assert()
            .success()
            .get_output()
            .stdout,
        done
    );
    assert_eq!(
        command(root, url)
            .args(["queue", "show", id, "--raw"])
            .assert()
            .success()
            .get_output()
            .stdout,
        REPORT
    );
    let all = command(root, url)
        .args(["queue", "ls", "--queue", "bounces"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&all).unwrap(),
        serde_json::json!([job])
    );
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let db = Database::connect(url, 1).await.unwrap();
        let audits: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT target_id,diff,at FROM audit_log WHERE action='queue.acknowledge_bounce'")
            .fetch_all(db.pool()).await.unwrap();
        assert_eq!(audits.len(), 1);
        assert_eq!(audits[0].0, id);
        assert_eq!(serde_json::from_str::<serde_json::Value>(&audits[0].1).unwrap(),
            serde_json::json!({"previous_state":"ready", "state":"done", "reason":"reviewed report"}));
        for table in ["members", "bounce_events", "delivery_recipients"] {
            let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
                .fetch_one(db.pool()).await.unwrap();
            assert_eq!(count, 0);
        }
        db.pool().close().await;
    });
}

#[test]
fn cli_rejects_ordinary_job_and_actively_leased_bounce_without_mutation() {
    use listmngr_db::mail_queue::{NewMessage, Queue};
    let root = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        root.path().join("reject.db").display()
    );
    seed_inbox(&url);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (ordinary, lease) = runtime.block_on(async {
        let db = Database::connect(&url, 1).await.unwrap();
        let ordinary = db
            .mail_queue()
            .enqueue(
                NewMessage {
                    raw: REPORT.to_vec(),
                    external_id: "ordinary".into(),
                    context: "{}".into(),
                    queue: Queue::In,
                    max_attempts: 3,
                },
                100,
            )
            .await
            .unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        let lease = db
            .mail_queue()
            .claim(Queue::Bounces, "active", now, 60_000)
            .await
            .unwrap()
            .unwrap();
        db.pool().close().await;
        (ordinary, lease)
    });
    for job in [&ordinary, &lease.job] {
        command(root.path(), &url)
            .args([
                "queue",
                "acknowledge-bounce",
                &job.id.0.to_string(),
                "--reason",
                "reviewed",
            ])
            .assert()
            .failure();
    }
    runtime.block_on(async {
        let db = Database::connect(&url, 1).await.unwrap();
        assert_eq!(db.mail_queue().job(ordinary.id).await.unwrap(), ordinary);
        assert_eq!(db.mail_queue().job(lease.job.id).await.unwrap(), lease.job);
        db.mail_queue()
            .live()
            .heartbeat(&lease, 0, 60_000)
            .await
            .unwrap();
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE action='queue.acknowledge_bounce'",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(count, 0);
        assert_eq!(
            db.mail_queue()
                .message(lease.job.message_id)
                .await
                .unwrap()
                .raw,
            REPORT
        );
        db.pool().close().await;
    });
}

#[test]
fn state_filter_precedes_limit_and_preserves_default_listing() {
    let root = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        root.path().join("page.db").display()
    );
    seed_inbox(&url);
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let db = Database::connect(&url, 1).await.unwrap();
        // UUID zero-prefix rows sort before the production UUID-v7 ready job.
        sqlx::query("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<1000) INSERT INTO queue_jobs(id,message_id,queue,state,max_attempts,run_after) SELECT printf('00000000-0000-0000-0000-%012d',x), (SELECT message_id FROM queue_jobs LIMIT 1), 'bounces','done',3,100 FROM n")
            .execute(db.pool()).await.unwrap();
        db.pool().close().await;
    });
    for (args, length, state) in [
        (vec!["queue", "ls", "--queue", "bounces"], 1000, "done"),
        (
            vec!["queue", "ls", "--queue", "bounces", "--state", "ready"],
            1,
            "ready",
        ),
        (
            vec!["queue", "ls", "--queue", "bounces", "--state", "done"],
            1000,
            "done",
        ),
        (
            vec!["queue", "ls", "--queue", "bounces", "--state", "leased"],
            0,
            "leased",
        ),
        (
            vec!["queue", "ls", "--queue", "bounces", "--state", "shunted"],
            0,
            "shunted",
        ),
    ] {
        let output = command(root.path(), &url)
            .args(args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let jobs: Vec<serde_json::Value> = serde_json::from_slice(&output).unwrap();
        assert_eq!(jobs.len(), length);
        assert!(jobs.iter().all(|job| job["state"] == state));
    }
}

fn seed_inbox(url: &str) {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let db = Database::connect(url, 1).await.unwrap();
        db.migrate().await.unwrap();
        db.domains()
            .create("example.invalid", "", None)
            .await
            .unwrap();
        db.lists()
            .create(NewList {
                list_id: "list.example.invalid".parse().unwrap(),
                display_name: "fixture".into(),
                style: "legacy-default".into(),
            })
            .await
            .unwrap();
        let mut intake = InboundHandler {
            db: db.clone(),
            local_hostname: "fixture.invalid".into(),
            max_message_bytes: 4096,
            max_recipients: 4,
            command_timeout: Duration::from_secs(3),
            in_max_attempts: 3,
        };
        let recipients: Vec<String> = vec![
            "list-bounces@example.invalid".into(),
            "list@example.invalid".into(),
        ];
        for recipient in &recipients {
            intake.validate_recipient(recipient).await.unwrap();
        }
        let outcomes = intake.deliver(None, &recipients, REPORT).await;
        assert_eq!(
            outcomes.iter().map(|o| o.code).collect::<Vec<_>>(),
            [250, 550]
        );
        db.pool().close().await;
    });
}

#[test]
fn bounce_without_message_id_is_inspectable_via_cli_without_becoming_a_post() {
    let root = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        root.path().join("inbox.db").display()
    );
    seed_inbox(&url);
    let output = command(root.path(), &url)
        .args(["queue", "ls", "--queue", "bounces"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let jobs: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(jobs.as_array().unwrap().len(), 1);
    assert_eq!(jobs[0]["queue"], "bounces");
    assert_eq!(jobs[0]["state"], "ready");
    let id = jobs[0]["id"].as_str().unwrap();
    let metadata = command(root.path(), &url)
        .args(["queue", "show", id])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(!String::from_utf8_lossy(&metadata).contains("private-report-contents"));
    let raw = command(root.path(), &url)
        .args(["queue", "show", id, "--raw"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(raw, REPORT);
    let ready = command(root.path(), &url)
        .args(["queue", "ls", "--queue", "bounces", "--state", "ready"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&ready).unwrap(),
        jobs
    );
    acknowledge_and_reopen(root.path(), &url, id);
    let ready = command(root.path(), &url)
        .args(["queue", "ls", "--queue", "bounces", "--state", "ready"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&ready).unwrap(),
        serde_json::json!([])
    );
    command(root.path(), &url)
        .args(["queue", "ls", "--state", "unknown"])
        .assert()
        .failure();
    let ordinary = command(root.path(), &url)
        .args(["queue", "ls", "--queue", "in"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&ordinary).unwrap(),
        serde_json::json!([])
    );
}
