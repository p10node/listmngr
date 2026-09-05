use listmngr_db::{
    Database,
    mail_queue::{NewMessage, Queue},
};

fn input(raw: &[u8], context: &str) -> NewMessage {
    NewMessage {
        raw: raw.to_vec(),
        external_id: "<same@example.org>".into(),
        context: context.into(),
        queue: Queue::In,
        max_attempts: 2,
    }
}

#[tokio::test]
async fn intake_preserves_distinct_bytes_and_submission_identity() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let raw = b"Message-ID: <same@example.org>\r\n\r\n\x00\xffraw\r\n";
    let a = db
        .mail_queue()
        .enqueue(input(raw, "list-a"), 100)
        .await
        .unwrap();
    let b = db
        .mail_queue()
        .enqueue(input(b"different\n\x80", "list-b"), 101)
        .await
        .unwrap();
    let c = db
        .mail_queue()
        .enqueue(input(raw, "list-b"), 102)
        .await
        .unwrap();
    assert_ne!(a.message_id, c.message_id);
    let stored = db.mail_queue().message(a.message_id).await.unwrap();
    assert_eq!(stored.raw, raw);
    assert_eq!(stored.context, "list-a");
    assert_eq!(stored.external_id, "<same@example.org>");
    assert_eq!(
        db.mail_queue().message(b.message_id).await.unwrap().raw,
        b"different\n\x80"
    );
    assert_eq!(
        db.mail_queue()
            .message(c.message_id)
            .await
            .unwrap()
            .store_key,
        stored.store_key
    );
    assert_eq!(db.mail_queue().job(a.id).await.unwrap(), a);
    let counts: (i64, i64, i64, i64) = sqlx::query_as("SELECT (SELECT COUNT(*) FROM message_blobs), (SELECT COUNT(*) FROM messages), (SELECT COUNT(*) FROM queue_jobs), (SELECT COUNT(*) FROM audit_log WHERE action='queue.enqueue')").fetch_one(db.pool()).await.unwrap();
    assert_eq!(counts, (2, 3, 3, 3));
}

#[tokio::test]
async fn claim_obeys_due_time_recovers_expiry_and_increments_attempts() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let job = db
        .mail_queue()
        .enqueue(input(b"claim raw", "route"), 100)
        .await
        .unwrap();
    assert!(
        db.mail_queue()
            .claim(Queue::In, "a", 99, 10)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        db.mail_queue()
            .claim(Queue::Out, "a", 100, 10)
            .await
            .unwrap()
            .is_none()
    );
    let first = db
        .mail_queue()
        .claim(Queue::In, "a", 100, 10)
        .await
        .unwrap()
        .expect("due job must be claimed");
    assert_eq!(first.job.id, job.id);
    assert_eq!(first.job.attempts, 1);
    assert_eq!(first.job.locked_by.as_deref(), Some("a"));
    assert_eq!(first.job.lease_until, Some(110));
    assert!(
        db.mail_queue()
            .claim(Queue::In, "b", 109, 10)
            .await
            .unwrap()
            .is_none()
    );
    let second = db
        .mail_queue()
        .claim(Queue::In, "b", 110, 10)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.job.id, job.id);
    assert_eq!(second.job.attempts, 2);
    assert_eq!(second.job.locked_by.as_deref(), Some("b"));
    assert_eq!(
        db.mail_queue().message(job.message_id).await.unwrap().raw,
        b"claim raw"
    );
}

#[tokio::test]
async fn fenced_transitions_retry_then_shunt_at_budget() {
    use listmngr_db::mail_queue::JobState;
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let job = db
        .mail_queue()
        .enqueue(input(b"retry raw", "route"), 100)
        .await
        .unwrap();
    let old = db
        .mail_queue()
        .claim(Queue::In, "same-worker", 100, 10)
        .await
        .unwrap()
        .unwrap();
    assert!(
        db.mail_queue().ack(&old, 110).await.is_err(),
        "expiry alone must fence ack"
    );
    let lease = db
        .mail_queue()
        .claim(Queue::In, "same-worker", 110, 10)
        .await
        .unwrap()
        .unwrap();
    assert!(db.mail_queue().ack(&old, 111).await.is_err());
    assert!(db.mail_queue().retry(&old, 111, 20, "stale").await.is_err());
    assert!(db.mail_queue().shunt(&old, 111, "stale").await.is_err());
    let shunted = db
        .mail_queue()
        .retry(&lease, 111, 20, "budget exhausted")
        .await
        .unwrap();
    assert_eq!(shunted.state, JobState::Shunted);
    assert_eq!(shunted.queue, Queue::Shunt);
    assert_eq!(shunted.attempts, 2);
    assert_eq!(shunted.last_error, "budget exhausted");
    assert!(shunted.locked_by.is_none());
    assert!(
        db.mail_queue()
            .claim(Queue::Shunt, "c", 999, 10)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(db.mail_queue().job(job.id).await.unwrap(), shunted);
    let job = db
        .mail_queue()
        .enqueue(input(b"second", "route"), 200)
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::In, "a", 200, 10)
        .await
        .unwrap()
        .unwrap();
    let retry = db
        .mail_queue()
        .retry(&lease, 201, 30, "temporary")
        .await
        .unwrap();
    assert_eq!(retry.state, JobState::Ready);
    assert_eq!(retry.run_after, 231);
    assert_eq!(retry.attempts, 1);
    assert!(
        db.mail_queue()
            .claim(Queue::In, "a", 230, 10)
            .await
            .unwrap()
            .is_none()
    );
    let lease = db
        .mail_queue()
        .claim(Queue::In, "b", 231, 10)
        .await
        .unwrap()
        .unwrap();
    let done = db.mail_queue().ack(&lease, 232).await.unwrap();
    assert_eq!(done.state, JobState::Done);
    assert!(db.mail_queue().ack(&lease, 233).await.is_err());
    assert_eq!(
        db.mail_queue().message(job.message_id).await.unwrap().raw,
        b"second"
    );
    assert!(
        db.mail_queue()
            .claim(Queue::In, "c", 999, 10)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn crashed_final_attempt_is_shunted_without_an_extra_delivery() {
    use listmngr_db::mail_queue::JobState;
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let mut msg = input(b"poison", "route");
    msg.max_attempts = 1;
    let job = db.mail_queue().enqueue(msg, 100).await.unwrap();
    db.mail_queue()
        .claim(Queue::In, "crashed", 100, 10)
        .await
        .unwrap()
        .unwrap();
    assert!(
        db.mail_queue()
            .claim(Queue::In, "recovery", 110, 10)
            .await
            .unwrap()
            .is_none(),
        "exhausted expired job must not be delivered again"
    );
    let shunted = db.mail_queue().job(job.id).await.unwrap();
    assert_eq!(shunted.state, JobState::Shunted);
    assert_eq!(shunted.queue, Queue::Shunt);
    assert_eq!(shunted.attempts, 1);
    assert!(!shunted.last_error.is_empty());
    assert!(
        db.mail_queue()
            .claim(Queue::Shunt, "recovery", 120, 10)
            .await
            .unwrap()
            .is_none()
    );
}

#[derive(Debug)]
struct SqliteFixture {
    dir: std::path::PathBuf,
    url: String,
}
impl SqliteFixture {
    fn new() -> Self {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/mail-queue-fixtures")
            .join(uuid::Uuid::now_v7().to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let url = format!("sqlite://{}?mode=rwc", dir.join("queue.db").display());
        Self { dir, url }
    }
}
impl Drop for SqliteFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn concurrent_claims(db: &Database) {
    let job = db
        .mail_queue()
        .enqueue(input(b"concurrent\x00\xff", "route"), 100)
        .await
        .unwrap();
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(8));
    let mut tasks = tokio::task::JoinSet::new();
    for n in 0..8 {
        let db = db.clone();
        let barrier = barrier.clone();
        tasks.spawn(async move {
            barrier.wait().await;
            db.mail_queue()
                .claim(Queue::In, &format!("worker-{n}"), 100, 10)
                .await
                .unwrap()
        });
    }
    let mut claims = Vec::new();
    while let Some(result) = tasks.join_next().await {
        if let Some(lease) = result.unwrap() {
            claims.push(lease);
        }
    }
    assert_eq!(
        claims.len(),
        1,
        "one durable job may have only one winning claimant"
    );
    assert_eq!(claims[0].job.id, job.id);
    let recovered = db
        .mail_queue()
        .claim(Queue::In, "recovered", 110, 10)
        .await
        .unwrap()
        .unwrap();
    assert!(db.mail_queue().ack(&claims[0], 111).await.is_err());
    db.mail_queue().ack(&recovered, 111).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sqlite_concurrent_claims_and_reopen_preserve_raw_bytes() {
    let fixture = SqliteFixture::new();
    let db = Database::connect(&fixture.url, 8).await.unwrap();
    db.migrate().await.unwrap();
    concurrent_claims(&db).await;
    let a = db
        .mail_queue()
        .enqueue(input(b"persist\r\n\x00\xff", "a"), 1000)
        .await
        .unwrap();
    let b = db
        .mail_queue()
        .enqueue(input(b"persist\n\x00\xfe", "b"), 1000)
        .await
        .unwrap();
    db.pool().close().await;
    let reopened = Database::connect(&fixture.url, 2).await.unwrap();
    assert_eq!(
        reopened
            .mail_queue()
            .message(a.message_id)
            .await
            .unwrap()
            .raw,
        b"persist\r\n\x00\xff"
    );
    assert_eq!(
        reopened
            .mail_queue()
            .message(b.message_id)
            .await
            .unwrap()
            .raw,
        b"persist\n\x00\xfe"
    );
    assert_eq!(reopened.mail_queue().job(a.id).await.unwrap(), a);
    reopened.pool().close().await;
}

async fn block_audit(db: &Database, action: &str) {
    sqlx::query(&format!("CREATE TRIGGER fail_queue_audit BEFORE INSERT ON audit_log WHEN NEW.action='{action}' BEGIN SELECT RAISE(ABORT, 'audit sabotage'); END"))
        .execute(db.pool()).await.unwrap();
}
async fn unblock_audit(db: &Database) {
    sqlx::query("DROP TRIGGER fail_queue_audit")
        .execute(db.pool())
        .await
        .unwrap();
}

#[tokio::test]
async fn audit_failure_rolls_back_intake_claim_and_every_transition() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    block_audit(&db, "queue.enqueue").await;
    assert!(
        db.mail_queue()
            .enqueue(input(b"must roll back", "x"), 100)
            .await
            .is_err()
    );
    let counts: (i64, i64, i64, i64) = sqlx::query_as("SELECT (SELECT COUNT(*) FROM message_blobs), (SELECT COUNT(*) FROM messages), (SELECT COUNT(*) FROM queue_jobs), (SELECT COUNT(*) FROM audit_log)").fetch_one(db.pool()).await.unwrap();
    assert_eq!(counts, (0, 0, 0, 0));
    unblock_audit(&db).await;
    let job = db
        .mail_queue()
        .enqueue(input(b"survives\x80", "x"), 100)
        .await
        .unwrap();
    block_audit(&db, "queue.claim").await;
    assert!(
        db.mail_queue()
            .claim(Queue::In, "a", 100, 10)
            .await
            .is_err()
    );
    assert_eq!(db.mail_queue().job(job.id).await.unwrap(), job);
    unblock_audit(&db).await;
    let lease = db
        .mail_queue()
        .claim(Queue::In, "a", 100, 10)
        .await
        .unwrap()
        .unwrap();
    for action in ["queue.ack", "queue.retry", "queue.shunt"] {
        block_audit(&db, action).await;
        let result = match action {
            "queue.ack" => db.mail_queue().ack(&lease, 101).await,
            "queue.retry" => db.mail_queue().retry(&lease, 101, 20, "failure").await,
            _ => db.mail_queue().shunt(&lease, 101, "failure").await,
        };
        assert!(result.is_err(), "{action} must surface audit failure");
        assert_eq!(db.mail_queue().job(job.id).await.unwrap(), lease.job);
        unblock_audit(&db).await;
    }
    db.mail_queue()
        .shunt(&lease, 102, "manual quarantine")
        .await
        .unwrap();
    let actions: Vec<String> =
        sqlx::query_scalar("SELECT action FROM audit_log WHERE target_id=$1 ORDER BY at,id")
            .bind(job.id.0.to_string())
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(actions, ["queue.enqueue", "queue.claim", "queue.shunt"]);
    assert_eq!(
        db.mail_queue().message(job.message_id).await.unwrap().raw,
        b"survives\x80"
    );
}

#[tokio::test]
async fn invalid_budget_is_typed_validation_without_writes() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let mut msg = input(b"invalid", "x");
    msg.max_attempts = 0;
    assert!(matches!(
        db.mail_queue().enqueue(msg, 100).await,
        Err(listmngr_core::Error::Validation(_))
    ));
    assert!(db.mail_queue().claim(Queue::In, "", 100, 10).await.is_err());
    assert!(db.mail_queue().claim(Queue::In, "a", 100, 0).await.is_err());
    assert!(
        db.mail_queue()
            .claim(Queue::In, "a", i64::MAX, 1)
            .await
            .is_err()
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM messages")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; creates and drops only a unique test schema"]
async fn postgres_isolated_mail_queue_contract() {
    let url = std::env::var("TEST_POSTGRES_URL").expect("TEST_POSTGRES_URL is required");
    assert!(url.starts_with("postgres://") || url.starts_with("postgresql://"));
    assert!(
        !url.contains("options="),
        "fixture requires URL without pre-existing startup options"
    );
    let admin = Database::connect(&url, 1).await.unwrap();
    let schema = format!("mail_queue_test_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(admin.pool())
        .await
        .unwrap();
    let separator = if url.contains('?') { '&' } else { '?' };
    let fixture_url = format!("{url}{separator}options=-csearch_path%3D{schema}");
    let expected_schema = schema.clone();
    // Catch scenario panics via JoinHandle so our schema is cleaned even on failure.
    let result = tokio::spawn(async move {
        let db = Database::connect(&fixture_url, 8).await.unwrap();
        let current: String = sqlx::query_scalar("SELECT current_schema()::text")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(
            current, expected_schema,
            "never migrate outside the isolated fixture"
        );
        db.migrate().await.unwrap();
        db.migrate().await.unwrap();
        concurrent_claims(&db).await;
        postgres_retry_and_rollback(&db).await;
        db.pool().close().await;
    })
    .await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(admin.pool())
        .await
        .unwrap();
    admin.pool().close().await;
    result.unwrap();
}

async fn postgres_retry_and_rollback(db: &Database) {
    use listmngr_db::mail_queue::JobState;
    let a = db
        .mail_queue()
        .enqueue(input(b"pg raw\r\n\x00\xff", "pg-a"), 200)
        .await
        .unwrap();
    let b = db
        .mail_queue()
        .enqueue(input(b"pg raw\n\x00\xfe", "pg-b"), 300)
        .await
        .unwrap();
    assert_eq!(
        db.mail_queue().message(a.message_id).await.unwrap().raw,
        b"pg raw\r\n\x00\xff"
    );
    assert_eq!(
        db.mail_queue().message(b.message_id).await.unwrap().raw,
        b"pg raw\n\x00\xfe"
    );
    let lease = db
        .mail_queue()
        .claim(Queue::In, "pg", 200, 10)
        .await
        .unwrap()
        .unwrap();
    let retried = db
        .mail_queue()
        .retry(&lease, 201, 20, "temporary")
        .await
        .unwrap();
    assert_eq!(retried.run_after, 221);
    assert!(
        db.mail_queue()
            .claim(Queue::In, "pg", 220, 10)
            .await
            .unwrap()
            .is_none()
    );
    let final_lease = db
        .mail_queue()
        .claim(Queue::In, "pg", 221, 10)
        .await
        .unwrap()
        .unwrap();
    assert!(db.mail_queue().shunt(&lease, 222, "stale").await.is_err());
    let shunted = db
        .mail_queue()
        .retry(&final_lease, 222, 20, "permanent")
        .await
        .unwrap();
    assert_eq!(shunted.state, JobState::Shunted);
    assert_eq!(shunted.attempts, 2);
    postgres_audit_rollback(db, &b).await;
}

async fn postgres_audit_rollback(db: &Database, b: &listmngr_db::mail_queue::QueueJob) {
    sqlx::query("CREATE FUNCTION reject_queue_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'audit sabotage'; END $$").execute(db.pool()).await.unwrap();
    sqlx::query("CREATE TRIGGER fail_queue_audit BEFORE INSERT ON audit_log FOR EACH ROW EXECUTE FUNCTION reject_queue_audit()").execute(db.pool()).await.unwrap();
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM messages")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(
        db.mail_queue()
            .enqueue(input(b"pg rollback", "pg"), 400)
            .await
            .is_err()
    );
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM messages")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(before, after);
    assert!(
        db.mail_queue()
            .claim(Queue::In, "pg", 300, 10)
            .await
            .is_err()
    );
    assert_eq!(&db.mail_queue().job(b.id).await.unwrap(), b);
    sqlx::query("DROP TRIGGER fail_queue_audit ON audit_log")
        .execute(db.pool())
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::In, "pg", 300, 10)
        .await
        .unwrap()
        .unwrap();
    db.mail_queue().ack(&lease, 301).await.unwrap();
}

#[tokio::test]
async fn intake_into_shunt_never_runs_automatically() {
    use listmngr_db::mail_queue::JobState;
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let mut msg = input(b"quarantined", "x");
    msg.queue = Queue::Shunt;
    let job = db.mail_queue().enqueue(msg, 100).await.unwrap();
    assert_eq!(job.state, JobState::Shunted);
    assert!(
        db.mail_queue()
            .claim(Queue::Shunt, "worker", 101, 10)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn durable_message_schema_exists() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('message_blobs','messages','queue_jobs')")
        .fetch_one(db.pool()).await.unwrap();
    assert_eq!(
        count, 3,
        "durable blob, message index and queue tables must exist"
    );
}
