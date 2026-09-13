use assert_cmd::Command;
use listmngr_db::{
    Database,
    mail_queue::{ChildJob, JobId, NewMessage, Queue, RecipientOutcome},
};
use serde_json::Value;

struct Fixture {
    dir: tempfile::TempDir,
    url: String,
    job: JobId,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("recovery.db").display()
        );
        let job = tokio::runtime::Runtime::new().unwrap().block_on(async {
            let db = Database::connect(&url, 1).await.unwrap();
            db.migrate().await.unwrap();
            db.mail_queue()
                .enqueue(
                    NewMessage {
                        raw: b"Message-ID: <recovery@example.invalid>\r\n\r\nfixture".to_vec(),
                        external_id: "<recovery@example.invalid>".into(),
                        context: "{}".into(),
                        queue: Queue::In,
                        max_attempts: 3,
                    },
                    100,
                )
                .await
                .unwrap();
            let input = db
                .mail_queue()
                .claim(Queue::In, "fixture", 100, 1000)
                .await
                .unwrap()
                .unwrap();
            db.mail_queue()
                .complete_with_children(
                    &input,
                    101,
                    &[ChildJob {
                        queue: Queue::Out,
                        max_attempts: 3,
                        recipients: vec![
                            "unknown@example.invalid".into(),
                            "sent@example.invalid".into(),
                        ],
                    }],
                )
                .await
                .unwrap();
            let out = db
                .mail_queue()
                .claim(Queue::Out, "fixture", 102, 1000)
                .await
                .unwrap()
                .unwrap();
            db.mail_queue()
                .begin_delivery(
                    &out,
                    103,
                    &[
                        "unknown@example.invalid".into(),
                        "sent@example.invalid".into(),
                    ],
                )
                .await
                .unwrap();
            db.mail_queue()
                .finish_delivery(
                    &out,
                    104,
                    &[
                        (
                            "unknown@example.invalid".into(),
                            RecipientOutcome::Ambiguous,
                            "lost final reply".into(),
                        ),
                        (
                            "sent@example.invalid".into(),
                            RecipientOutcome::Sent,
                            "250 accepted".into(),
                        ),
                    ],
                    100,
                )
                .await
                .unwrap();
            db.pool().close().await;
            out.job.id
        });
        Self { dir, url, job }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::cargo_bin("listmngr").unwrap();
        command
            .env_clear()
            .current_dir(self.dir.path())
            .env("LISTMNGR__DATABASE__URL", &self.url)
            .args(args);
        command
    }

    fn json(&self, args: &[&str]) -> Value {
        serde_json::from_slice(&self.command(args).assert().success().get_output().stdout).unwrap()
    }
}

#[test]
fn inspect_distinguishes_ambiguous_from_sent_without_exporting_mail() {
    let f = Fixture::new();
    let rows = f.json(&["queue", "recipients", &f.job.0.to_string()]);
    assert_eq!(rows[0]["email"], "sent@example.invalid");
    assert_eq!(rows[0]["status"], "sent");
    assert_eq!(rows[1]["email"], "unknown@example.invalid");
    assert_eq!(rows[1]["status"], "ambiguous");
    assert_eq!(rows[1]["detail"], "lost final reply");
    assert!(rows[1].get("attempt_token").is_none());
    assert!(rows[1].get("raw").is_none());
}

#[test]
fn retry_requires_explicit_duplicate_risk_and_only_requeues_unknown_recipient() {
    let f = Fixture::new();
    let id = f.job.0.to_string();
    let args = [
        "queue",
        "resolve",
        &id,
        "unknown@example.invalid",
        "--outcome",
        "retry",
        "--reason",
        "relay logs checked",
    ];
    f.command(&args).assert().code(2);
    assert_eq!(
        f.json(&["queue", "recipients", &id])[1]["status"],
        "ambiguous"
    );
    let mut permitted = args.to_vec();
    permitted.push("--acknowledge-duplicate-risk");
    f.command(&permitted).assert().success();
    let rows = f.json(&["queue", "recipients", &id]);
    assert_eq!(rows[0]["status"], "sent");
    assert_eq!(rows[1]["status"], "pending");
    assert_eq!(f.json(&["queue", "show", &id])["state"], "ready");
    f.command(&permitted).assert().code(6);
    let mut sent = permitted.clone();
    sent[3] = "sent@example.invalid";
    f.command(&sent).assert().code(6);
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let db = Database::connect(&f.url, 1).await.unwrap();
        let events: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='queue.resolve'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(events, 1);
        assert_eq!(
            db.mail_queue().pending_recipients(f.job).await.unwrap(),
            vec!["unknown@example.invalid"]
        );
        db.pool().close().await;
    });
}
