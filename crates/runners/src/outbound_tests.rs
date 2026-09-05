use super::*;
use listmngr_db::{
    NewList,
    mail_queue::{ChildJob, JobState, NewMessage},
};

async fn fixture(
    context: &str,
    raw: &[u8],
) -> (Database, Lease, MailRoleConfig, tokio::net::TcpListener) {
    fixture_with_recipients(context, raw, vec!["member@example.invalid".into()]).await
}

async fn fixture_with_recipients(
    context: &str,
    raw: &[u8],
    recipients: Vec<String>,
) -> (Database, Lease, MailRoleConfig, tokio::net::TcpListener) {
    fixture_at("sqlite::memory:", context, raw, recipients).await
}

pub(super) async fn fixture_at(
    url: &str,
    context: &str,
    raw: &[u8],
    recipients: Vec<String>,
) -> (Database, Lease, MailRoleConfig, tokio::net::TcpListener) {
    let db = Database::connect(url, 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "test", None)
        .await
        .unwrap();
    db.lists()
        .create(NewList {
            list_id: "test.example.invalid".parse().unwrap(),
            display_name: "Test".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: raw.to_vec(),
                external_id: "out-test".into(),
                context: context.into(),
                queue: Queue::In,
                max_attempts: 5,
            },
            now,
        )
        .await
        .unwrap();
    let source = db
        .mail_queue()
        .claim(Queue::In, "in", now, 20000)
        .await
        .unwrap()
        .unwrap();
    db.mail_queue()
        .complete_with_children(
            &source,
            now,
            &[ChildJob {
                queue: Queue::Out,
                max_attempts: 5,
                recipients,
            }],
        )
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::Out, "out", now, 20000)
        .await
        .unwrap()
        .unwrap();
    let sink = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut role = MailRoleConfig::from_core(&listmngr_core::Config::default()).unwrap();
    role.smtp_relay = sink.local_addr().unwrap();
    role.command_timeout = Duration::from_millis(50);
    (db, lease, role, sink)
}

#[tokio::test]
async fn mixed_and_missing_outcomes_persist_and_retry_only_pending_recipients() {
    let recipients: Vec<String> = (0..5).map(|n| format!("r{n}@example.invalid")).collect();
    let raw = b"Subject: mixed\r\n\r\nbody";
    let (db, lease, _role, _sink) = fixture_with_recipients(
        "{\"list_id\":\"test.example.invalid\"}",
        raw,
        recipients.clone(),
    )
    .await;
    finish_delivery(
        &db,
        &lease,
        &recipients,
        &[
            RecipientStatus::Sent,
            RecipientStatus::PermanentFailure("550 rejected".into()),
            RecipientStatus::Ambiguous("remote outcome unknown".into()),
            RecipientStatus::TransientFailure("451 deferred".into()),
            // Fifth recipient deliberately has no outcome: it must stay pending.
        ],
    )
    .await;
    let states: Vec<(String, String)> = sqlx::query_as(
        "SELECT email,status FROM delivery_recipients WHERE job_id=$1 ORDER BY email",
    )
    .bind(lease.job.id.0.to_string())
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(
        states,
        recipients
            .iter()
            .cloned()
            .zip(["sent", "failed", "ambiguous", "pending", "pending"].map(str::to_owned))
            .collect::<Vec<_>>()
    );
    let job = db.mail_queue().job(lease.job.id).await.unwrap();
    assert_eq!(job.state, JobState::Ready);
    let retry = db
        .mail_queue()
        .claim(Queue::Out, "retry", job.run_after, 20_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(retry.job.id, job.id);
    assert_eq!(
        db.mail_queue().pending_recipients(job.id).await.unwrap(),
        recipients[3..]
    );
    assert_eq!(
        db.mail_queue().message(job.message_id).await.unwrap().raw,
        raw
    );
}

#[tokio::test]
async fn invalid_context_or_cooking_never_connects_and_retains_bytes() {
    for (context, raw) in [
        (
            "{broken",
            b"Approved: secret\r\nBcc: hidden\r\n\r\nbody".as_slice(),
        ),
        (
            "{\"list_id\":\"test.example.invalid\"}",
            b"no header boundary".as_slice(),
        ),
    ] {
        let (db, lease, role, sink) = fixture(context, raw).await;
        deliver_one(&db, &role, lease.clone()).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(20), sink.accept())
                .await
                .is_err(),
            "invalid message reached SMTP"
        );
        assert_eq!(
            db.mail_queue().job(lease.job.id).await.unwrap().state,
            JobState::Shunted
        );
        assert_eq!(
            db.mail_queue()
                .message(lease.job.message_id)
                .await
                .unwrap()
                .raw,
            raw
        );
    }
}

#[tokio::test]
async fn list_lookup_failure_never_connects_and_retries() {
    let (db, lease, role, sink) = fixture(
        "{\"list_id\":\"test.example.invalid\"}",
        b"Subject: test\r\n\r\nbody",
    )
    .await;
    sqlx::query("ALTER TABLE mailing_lists RENAME TO unavailable_lists")
        .execute(db.pool())
        .await
        .unwrap();
    deliver_one(&db, &role, lease.clone()).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(20), sink.accept())
            .await
            .is_err(),
        "dependency error reached SMTP"
    );
    assert_eq!(
        db.mail_queue().job(lease.job.id).await.unwrap().state,
        JobState::Ready
    );
}
