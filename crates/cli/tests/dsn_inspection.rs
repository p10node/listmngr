use assert_cmd::Command;
use listmngr_db::{Database, NewList};
use listmngr_mail::lmtp::LmtpHandler;
use listmngr_runners::InboundHandler;
use serde_json::json;
use std::{path::Path, time::Duration};

const REPORT: &[u8] = b"From: MAILER-DAEMON <daemon@example.invalid>\r\nContent-Type: multipart/report; report-type=delivery-status; boundary=report\r\n\r\n--report\r\nContent-Type: text/plain\r\n\r\nprivate explanation\r\n--report\r\nContent-Type: message/delivery-status\r\n\r\nReporting-MTA: dns; mx.example.invalid\r\n\r\nFinal-Recipient: rfc822; alice@example.invalid\r\nAction: failed\r\nStatus: 5.1.1\r\nDiagnostic-Code: smtp; private diagnostic\r\n--report--\r\n";

async fn seed_member(db: &Database) -> listmngr_core::Member {
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: "list.example.invalid".parse().unwrap(),
            display_name: "fixture".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    db.members()
        .create(listmngr_db::NewMember {
            list_id: list.id,
            email: "alice@example.invalid".into(),
            role: listmngr_core::MemberRole::Member,
            subscription_mode: listmngr_core::SubscriptionMode::AsAddress,
            display_name: "Fixture recipient".into(),
        })
        .await
        .unwrap()
}

async fn assert_no_delivery_outcomes(db: &Database) {
    for table in ["bounce_events", "delivery_recipients"] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(count, 0);
    }
}

async fn member_state(db: &Database, member: &listmngr_core::Member) -> serde_json::Value {
    json!({
        "member":db.members().get(member.id).await.unwrap(),
        "preferences":db.preferences().get(member.preferences_id).await.unwrap(),
    })
}

fn cli(root: &Path, url: &str) -> Command {
    let mut command = Command::cargo_bin("listmngr").unwrap();
    command
        .env_clear()
        .current_dir(root)
        .env("LISTMNGR__DATABASE__URL", url);
    command
}

#[test]
fn malformed_and_non_bounce_jobs_are_not_inspected_or_mutated() {
    use listmngr_db::mail_queue::{NewMessage, Queue};
    let root = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        root.path().join("negative.db").display()
    );
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let db = runtime.block_on(async {
        let db = Database::connect(&url, 1).await.unwrap();
        db.migrate().await.unwrap();
        db
    });
    let delayed = String::from_utf8(REPORT.to_vec())
        .unwrap()
        .replace("alice@example.invalid", "bob@example.invalid")
        .replace("Action: failed", "Action: delayed")
        .replace("Status: 5.1.1", "Status: 4.2.2");
    for (index, (queue, raw, accepted)) in [
        (Queue::In, REPORT, false),
        (
            Queue::Bounces,
            b"Subject: private-invalid-report\r\n\r\nopaque".as_slice(),
            false,
        ),
        (Queue::Bounces, delayed.as_bytes(), true),
    ]
    .into_iter()
    .enumerate()
    {
        let (job, audits) = runtime.block_on(async {
            let job = db
                .mail_queue()
                .enqueue(
                    NewMessage {
                        raw: raw.to_vec(),
                        external_id: format!("fixture-{index}"),
                        context: "{}".into(),
                        queue,
                        max_attempts: 3,
                    },
                    100,
                )
                .await
                .unwrap();
            let audits: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
                .fetch_one(db.pool())
                .await
                .unwrap();
            (job, audits)
        });
        let assertion = cli(root.path(), &url)
            .args(["queue", "show", &job.id.0.to_string(), "--dsn"])
            .assert();
        if accepted {
            let output = assertion.success().get_output().stdout.clone();
            let result: serde_json::Value = serde_json::from_slice(&output).unwrap();
            assert_eq!(
                result,
                json!({"untrusted":true,"recipients":[{"final_recipient":"bob@example.invalid", "action":"delayed", "status":"4.2.2"}]})
            );
        } else {
            let output = assertion.failure().get_output().clone();
            assert!(output.stdout.is_empty());
            assert!(!String::from_utf8_lossy(&output.stderr).contains("private"));
        }
        runtime.block_on(async {
            assert_eq!(db.mail_queue().job(job.id).await.unwrap(), job);
            assert_eq!(
                db.mail_queue().message(job.message_id).await.unwrap().raw,
                raw
            );
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
                .fetch_one(db.pool())
                .await
                .unwrap();
            assert_eq!(count, audits);
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM bounce_events")
                .fetch_one(db.pool())
                .await
                .unwrap();
            assert_eq!(count, 0);
        });
    }
    runtime.block_on(db.pool().close());
}

#[test]
fn inspect_dsn_is_explicit_untrusted_and_read_only() {
    let root = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}?mode=rwc", root.path().join("dsn.db").display());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (db, job, audits, member) = runtime.block_on(async {
        let db = Database::connect(&url, 1).await.unwrap();
        db.migrate().await.unwrap();
        let member = seed_member(&db).await;
        let mut intake = InboundHandler {
            db: db.clone(),
            local_hostname: "fixture.invalid".into(),
            max_message_bytes: 4096,
            max_recipients: 4,
            command_timeout: Duration::from_secs(3),
            in_max_attempts: 3,
            verp_delimiter: "+".into(),
        };
        let recipients: Vec<String> = vec!["list-bounces@example.invalid".into()];
        intake.validate_recipient(&recipients[0]).await.unwrap();
        assert_eq!(intake.deliver(None, &recipients, REPORT).await[0].code, 250);
        let id: String = sqlx::query_scalar("SELECT id FROM queue_jobs WHERE queue='bounces'")
            .fetch_one(db.pool())
            .await
            .unwrap();
        let job = db
            .mail_queue()
            .job(listmngr_db::mail_queue::JobId(id.parse().unwrap()))
            .await
            .unwrap();
        let audits: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
            .fetch_one(db.pool())
            .await
            .unwrap();
        (db, job, audits, member)
    });
    let before = runtime.block_on(member_state(&db, &member));
    let id = job.id.0.to_string();
    let output = cli(root.path(), &url)
        .args(["queue", "show", &id, "--dsn"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(
        report,
        json!({"untrusted":true,"recipients":[{
            "final_recipient":"alice@example.invalid", "action":"failed", "status":"5.1.1"
        }]})
    );
    assert!(!String::from_utf8_lossy(&output).contains("private"));
    cli(root.path(), &url)
        .args(["queue", "show", &id, "--dsn", "--raw"])
        .assert()
        .failure();
    assert_eq!(
        cli(root.path(), &url)
            .args(["queue", "show", &id, "--raw"])
            .assert()
            .success()
            .get_output()
            .stdout,
        REPORT
    );
    runtime.block_on(async {
        assert_eq!(db.mail_queue().job(job.id).await.unwrap(), job);
        assert_eq!(
            db.mail_queue().message(job.message_id).await.unwrap().raw,
            REPORT
        );
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(count, audits);
        assert_eq!(member_state(&db, &member).await, before);
        assert_no_delivery_outcomes(&db).await;
        db.pool().close().await;
    });
}
