use listmngr_db::{
    Database,
    mail_queue::{ChildJob, LeaseClock, NewMessage, Queue, RecipientOutcome},
};
use std::sync::atomic::{AtomicI64, Ordering};
#[derive(Debug)]
struct Clock(AtomicI64);
impl LeaseClock for Clock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}
#[derive(Debug)]
struct FinalClock {
    remaining: AtomicI64,
    final_ms: i64,
}
impl LeaseClock for FinalClock {
    fn now_ms(&self) -> i64 {
        if self.remaining.fetch_sub(1, Ordering::SeqCst) > 0 {
            101
        } else {
            self.final_ms
        }
    }
}

async fn final_audit_case(operation: &str, samples: i64) {
    let (db, _dir) = sqlite().await;
    let lease = outgoing_fixture(&db, operation).await;
    let before = recipient_state(&db, &lease).await;
    let counts: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM audit_log), (SELECT COUNT(*) FROM queue_jobs)",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    let clock = FinalClock {
        remaining: AtomicI64::new(samples),
        final_ms: if operation == "heartbeat" { 201 } else { 200 },
    };
    let result = mutate(&db, &clock, &lease, operation).await;
    assert!(
        matches!(result, Err(listmngr_core::Error::Conflict(_))),
        "{operation}: final audit expiry committed: {result:?}"
    );
    assert_eq!(db.mail_queue().job(lease.job.id).await.unwrap(), lease.job);
    assert_eq!(recipient_state(&db, &lease).await, before);
    let after: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM audit_log), (SELECT COUNT(*) FROM queue_jobs)",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(counts, after);
    mutate(&db, &Clock(AtomicI64::new(101)), &lease, operation)
        .await
        .unwrap();
    db.pool().close().await;
}

#[tokio::test]
async fn final_audit_begin() {
    final_audit_case("begin", 2).await;
    final_audit_case("notice_begin", 2).await;
}

#[tokio::test]
async fn final_audit_transition() {
    for operation in ["ack", "retry", "shunt"] {
        final_audit_case(operation, 1).await;
    }
}

#[tokio::test]
async fn final_audit_children() {
    final_audit_case("children", 1).await;
}

#[tokio::test]
async fn final_audit_heartbeat() {
    final_audit_case("heartbeat", 1).await;
}

#[tokio::test]
async fn final_audit_claim() {
    let (db, _dir) = sqlite().await;
    let job = db.mail_queue().enqueue(input(), 100).await.unwrap();
    let clock = FinalClock {
        remaining: AtomicI64::new(2),
        final_ms: 201,
    };
    let result = db
        .mail_queue()
        .with_clock(&clock)
        .claim(Queue::In, "owner", 101, 100)
        .await;
    assert!(
        matches!(result, Err(listmngr_core::Error::Conflict(_))),
        "expired claim committed: {result:?}"
    );
    assert_eq!(db.mail_queue().job(job.id).await.unwrap(), job);
    let audits: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='queue.claim'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(audits, 0);
    let valid = FinalClock {
        remaining: AtomicI64::new(2),
        final_ms: 200,
    };
    let lease = db
        .mail_queue()
        .with_clock(&valid)
        .claim(Queue::In, "owner", 101, 100)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lease.job.lease_until, Some(201));
    db.pool().close().await;
}

#[tokio::test]
async fn final_audit_uses_renewed_authority_not_lease_snapshot() {
    for operation in [
        "begin",
        "notice_begin",
        "finish",
        "notice_finish",
        "ack",
        "retry",
        "shunt",
        "children",
        "heartbeat",
    ] {
        let (db, _dir) = sqlite().await;
        let lease = outgoing_fixture(&db, operation).await;
        db.mail_queue().heartbeat(&lease, 101, 300).await.unwrap();
        assert_eq!(lease.job.lease_until, Some(200));
        let clock = Clock(AtomicI64::new(250));
        mutate(&db, &clock, &lease, operation).await.unwrap();
        db.pool().close().await;
    }
    // Renewal may complete after the old deadline, while its new grant is valid.
    let (db, _dir) = sqlite().await;
    let lease = outgoing_fixture(&db, "heartbeat").await;
    let clock = FinalClock {
        remaining: AtomicI64::new(1),
        final_ms: 200,
    };
    let renewed = db
        .mail_queue()
        .with_clock(&clock)
        .heartbeat(&lease, 101, 100)
        .await
        .unwrap();
    assert_eq!(renewed.job.lease_until, Some(201));
    db.pool().close().await;
}

#[tokio::test]
async fn final_audit_finish_regression() {
    final_audit_case("finish", 2).await;
    final_audit_case("notice_finish", 2).await;
}

#[path = "mail_queue_lock_clock/final_audit_postgres.rs"]
mod final_audit_postgres;

struct TempDir(std::path::PathBuf);
impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(uuid::Uuid::now_v7().to_string());
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn sqlite() -> (Database, TempDir) {
    let dir = TempDir::new();
    let db = Database::connect(
        &format!("sqlite://{}/fixture.sqlite?mode=rwc", dir.0.display()),
        3,
    )
    .await
    .unwrap();
    db.migrate().await.unwrap();
    (db, dir)
}
fn input() -> NewMessage {
    NewMessage {
        raw: b"lock clock".to_vec(),
        external_id: "fixture".into(),
        context: "fixture".into(),
        queue: Queue::In,
        max_attempts: 3,
    }
}
async fn mutate(
    db: &Database,
    clock: &dyn LeaseClock,
    lease: &listmngr_db::mail_queue::Lease,
    operation: &str,
) -> listmngr_core::Result<()> {
    let repo = db.mail_queue().with_clock(clock);
    match operation.trim_start_matches("notice_") {
        "ack" => repo.ack(lease, 101).await.map(|_| ()),
        "heartbeat" => repo.heartbeat(lease, 101, 100).await.map(|_| ()),
        "retry" => repo.retry(lease, 101, 10, "fixture").await.map(|_| ()),
        "shunt" => repo.shunt(lease, 101, "fixture").await.map(|_| ()),
        "children" => repo
            .complete_with_children(
                lease,
                101,
                &[ChildJob {
                    queue: Queue::Out,
                    max_attempts: 3,
                    recipients: vec!["child@example.invalid".into()],
                }],
            )
            .await
            .map(|_| ()),
        "begin" => {
            repo.begin_delivery(lease, 101, &["recipient@example.invalid".into()])
                .await
        }
        "finish" => repo
            .finish_delivery(
                lease,
                101,
                &[(
                    "recipient@example.invalid".into(),
                    RecipientOutcome::Sent,
                    "sent".into(),
                )],
                10,
            )
            .await
            .map(|_| ()),
        "hold" => db
            .moderation()
            .with_clock(clock)
            .hold(
                lease,
                &"fixture.example.invalid".parse().unwrap(),
                "sender",
                "subject",
                "reason",
                101,
            )
            .await
            .map(|_| ()),
        _ => unreachable!(),
    }
}

async fn expiry_matrix(db: &Database, sqlite: bool) {
    for operation in [
        "ack",
        "heartbeat",
        "retry",
        "shunt",
        "children",
        "begin",
        "finish",
        "hold",
        "notice_begin",
        "notice_finish",
    ] {
        expiry_case(db, sqlite, operation).await;
    }
}

async fn recipient_state(
    db: &Database,
    lease: &listmngr_db::mail_queue::Lease,
) -> (String, String, Option<String>) {
    sqlx::query_as("SELECT status,detail,attempt_token FROM delivery_recipients WHERE job_id=$1")
        .bind(lease.job.id.0.to_string())
        .fetch_one(db.pool())
        .await
        .unwrap()
}

async fn outgoing_fixture(db: &Database, operation: &str) -> listmngr_db::mail_queue::Lease {
    if operation.starts_with("notice_") {
        let domain = format!("{}.invalid", uuid::Uuid::now_v7().simple());
        db.domains().create(&domain, "", None).await.unwrap();
        let list = format!("notice.{domain}").parse().unwrap();
        db.lists()
            .create(listmngr_db::NewList {
                list_id: list,
                display_name: "Notice".into(),
                style: "legacy-default".into(),
            })
            .await
            .unwrap();
        let list = format!("notice.{domain}").parse().unwrap();
        db.workflows()
            .request(
                &list,
                "recipient@example.invalid",
                listmngr_db::workflows::SubscriptionAction::Join,
                100,
            )
            .await
            .unwrap();
        let lease = db
            .mail_queue()
            .claim(Queue::Out, "owner", 100, 100)
            .await
            .unwrap()
            .unwrap();
        if operation == "notice_finish" {
            db.mail_queue()
                .begin_delivery(&lease, 101, &["recipient@example.invalid".into()])
                .await
                .unwrap();
        }
        return lease;
    }
    db.mail_queue().enqueue(input(), 100).await.unwrap();
    let source = db
        .mail_queue()
        .claim(Queue::In, "owner", 100, 100)
        .await
        .unwrap()
        .unwrap();
    let (_, children) = db
        .mail_queue()
        .complete_with_children(
            &source,
            100,
            &[ChildJob {
                queue: Queue::Out,
                max_attempts: 3,
                recipients: vec!["recipient@example.invalid".into()],
            }],
        )
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::Out, "owner", 100, 100)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lease.job.id, children[0].id);
    if operation == "finish" {
        db.mail_queue()
            .begin_delivery(&lease, 101, &["recipient@example.invalid".into()])
            .await
            .unwrap();
    }
    lease
}

async fn expiry_case(db: &Database, sqlite: bool, operation: &str) {
    let lease = outgoing_fixture(db, operation).await;
    let before = recipient_state(db, &lease).await;
    let audit_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let clock = Clock(AtomicI64::new(101));
    // A separate connection owns a real writer/row lock. Poll the actual
    // operation while it is held. Time is advanced explicitly, never by sleep.
    let mut blocker = db
        .pool()
        .begin_with(if sqlite { "BEGIN IMMEDIATE" } else { "BEGIN" })
        .await
        .unwrap();
    sqlx::query("UPDATE queue_jobs SET last_error=last_error WHERE id=$1")
        .bind(lease.job.id.0.to_string())
        .execute(&mut *blocker)
        .await
        .unwrap();
    let mutation = mutate(db, &clock, &lease, operation);
    tokio::pin!(mutation);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut mutation)
            .await
            .is_err(),
        "{operation} did not wait"
    );
    clock.0.store(200, Ordering::SeqCst);
    blocker.commit().await.unwrap();
    assert!(
        matches!(mutation.await, Err(listmngr_core::Error::Conflict(_))),
        "{operation}: expired owner committed after writer wait"
    );
    assert_eq!(db.mail_queue().job(lease.job.id).await.unwrap(), lease.job);
    let after = recipient_state(db, &lease).await;
    assert_eq!(before, after, "{operation} leaked recipient mutation");
    let audit_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(audit_before, audit_after, "{operation} leaked audit");
    // Consume the fixture deterministically so the next iteration is isolated.
    db.mail_queue().ack(&lease, 101).await.unwrap();
}
#[tokio::test]
async fn expiry_during_sqlite_writer_wait_fences_all_mutations() {
    let (db, _dir) = sqlite().await;
    expiry_matrix(&db, true).await;
    db.pool().close().await;
}

async fn recipient_wait_fences_reservation(db: &Database, operation: &str) {
    let lease = outgoing_fixture(db, "begin").await;
    let mut blocker = db.pool().begin().await.unwrap();
    sqlx::query("UPDATE delivery_recipients SET detail=detail WHERE job_id=$1")
        .bind(lease.job.id.0.to_string())
        .execute(&mut *blocker)
        .await
        .unwrap();
    let clock = Clock(AtomicI64::new(101));
    let mutation = mutate(db, &clock, &lease, operation);
    tokio::pin!(mutation);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut mutation)
            .await
            .is_err()
    );
    clock.0.store(200, Ordering::SeqCst);
    blocker.commit().await.unwrap();
    assert!(
        matches!(mutation.await, Err(listmngr_core::Error::Conflict(_))),
        "recipient reservation committed after recipient lock wait"
    );
    assert_eq!(
        db.mail_queue()
            .pending_recipients(lease.job.id)
            .await
            .unwrap(),
        vec!["recipient@example.invalid"]
    );
    db.mail_queue().ack(&lease, 101).await.unwrap();
}

// Explicit disposable backend gate; every invocation owns its schema.
#[tokio::test]
#[ignore = "requires explicit disposable TEST_POSTGRES_URL; owns isolated schema"]
async fn expiry_during_postgres_row_lock_wait_fences_all_mutations() {
    let url = std::env::var("TEST_POSTGRES_URL").expect("explicit fixture database required");
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("queue_clock_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let isolated = format!(
        "{url}{}options=-csearch_path%3D{schema}",
        if url.contains('?') { '&' } else { '?' }
    );
    let db = Database::connect(&isolated, 3).await.unwrap();
    db.migrate().await.unwrap();
    expiry_matrix(&db, false).await;
    recipient_wait_fences_reservation(&db, "begin").await;
    recipient_wait_fences_reservation(&db, "finish").await;
    db.pool().close().await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}

#[tokio::test]
async fn expiry_during_pool_wait_fences_ack() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.mail_queue().enqueue(input(), 100).await.unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::In, "owner", 100, 100)
        .await
        .unwrap()
        .unwrap();
    let clock = Clock(AtomicI64::new(101));
    let connection = db.pool().acquire().await.unwrap();
    let mutation = mutate(&db, &clock, &lease, "ack");
    tokio::pin!(mutation);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut mutation)
            .await
            .is_err()
    );
    clock.0.store(200, Ordering::SeqCst);
    drop(connection);
    assert!(matches!(
        mutation.await,
        Err(listmngr_core::Error::Conflict(_))
    ));
    assert_eq!(db.mail_queue().job(lease.job.id).await.unwrap(), lease.job);
    db.pool().close().await;
}

#[tokio::test]
async fn claim_deadline_is_based_on_post_reservation_time() {
    let (db, _dir) = sqlite().await;
    db.mail_queue().enqueue(input(), 100).await.unwrap();
    let clock = Clock(AtomicI64::new(101));
    let repo = db.mail_queue().with_clock(&clock);
    let blocker = db.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
    let claim = repo.claim(Queue::In, "owner", 101, 100);
    tokio::pin!(claim);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut claim)
            .await
            .is_err()
    );
    clock.0.store(500, Ordering::SeqCst);
    blocker.commit().await.unwrap();
    let lease = claim.await.unwrap().unwrap();
    assert_eq!(lease.job.lease_until, Some(600));
    assert!(
        matches!(
            db.mail_queue().live().ack(&lease, 101).await,
            Err(listmngr_core::Error::Conflict(_))
        ),
        "system clock must ignore fixture timestamp"
    );
    db.pool().close().await;
}
