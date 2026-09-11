use super::{AtomicI64, Clock, Database, Ordering, mutate, outgoing_fixture, recipient_state};
use listmngr_db::mail_queue::{Lease, Queue};
use std::time::Duration;

async fn counts(db: &Database) -> (i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT COUNT(*) FROM audit_log), (SELECT COUNT(*) FROM queue_jobs), (SELECT COUNT(*) FROM delivery_recipients)")
        .fetch_one(db.pool()).await.unwrap()
}

async fn operation(
    db: &Database,
    clock: &Clock,
    lease: &Lease,
    op: &str,
) -> listmngr_core::Result<()> {
    if op == "claim" {
        db.mail_queue()
            .with_clock(clock)
            .claim(Queue::Out, "new-owner", 101, 100)
            .await
            .map(|_| ())
    } else {
        mutate(db, clock, lease, op).await
    }
}

async fn install_barrier(db: &Database, op: &str, key: i64) {
    let action = match op {
        "begin" | "notice_begin" => "queue.delivery_begin",
        "finish" | "notice_finish" | "ack" => "queue.ack",
        "children" => "queue.enqueue",
        "retry" => "queue.retry",
        "shunt" => "queue.shunt",
        "heartbeat" => "queue.heartbeat",
        "claim" => "queue.claim",
        _ => unreachable!(),
    };
    sqlx::query(&format!("CREATE OR REPLACE FUNCTION final_audit_barrier() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action = '{action}' THEN PERFORM pg_advisory_xact_lock({key}::bigint); END IF; RETURN NEW; END $$"))
        .execute(db.pool()).await.unwrap();
    sqlx::query("CREATE TRIGGER final_audit_barrier BEFORE INSERT ON audit_log FOR EACH ROW EXECUTE FUNCTION final_audit_barrier()")
        .execute(db.pool()).await.unwrap();
}

async fn wait_through_audit(
    db: &Database,
    lease: &Lease,
    op: &str,
    expires: bool,
) -> listmngr_core::Result<()> {
    let mut blocker = db.pool().begin().await.unwrap();
    // A UUIDv7 timestamp prefix is shared by concurrent fixtures. This live
    // connection's PID is unique on the fixture server while we hold it.
    let key: i64 = sqlx::query_scalar("SELECT pg_backend_pid()::bigint")
        .fetch_one(&mut *blocker)
        .await
        .unwrap();
    install_barrier(db, op, key).await;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(key)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let clock = Clock(AtomicI64::new(101));
    let mutation = operation(db, &clock, lease, op);
    tokio::pin!(mutation);
    // Observe the actual trigger's advisory-lock wait, not a sleep-based guess.
    let observed = async {
        loop {
            let waiting: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pg_locks w WHERE w.locktype='advisory' AND w.classid=0 AND w.objid::bigint=$1 AND NOT w.granted AND w.database=(SELECT oid FROM pg_database WHERE datname=current_database()) AND EXISTS(SELECT 1 FROM pg_locks q WHERE q.pid=w.pid AND q.database=w.database AND q.relation='queue_jobs'::regclass AND q.granted)")
                .bind(key).fetch_one(db.pool()).await.unwrap();
            if waiting > 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
    };
    tokio::select! {
        result = &mut mutation => panic!("{op} never reached audit barrier: {result:?}"),
        result = tokio::time::timeout(Duration::from_secs(5), observed) => result.expect("audit lock wait not observed"),
    }
    let deadline = if matches!(op, "heartbeat" | "claim") {
        201
    } else {
        200
    };
    clock.0.store(
        if expires { deadline } else { deadline - 1 },
        Ordering::SeqCst,
    );
    blocker.commit().await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), mutation)
        .await
        .unwrap();
    sqlx::query("DROP TRIGGER final_audit_barrier ON audit_log")
        .execute(db.pool())
        .await
        .unwrap();
    result
}

async fn assert_success(db: &Database, lease: &Lease, op: &str, before: (i64, i64, i64)) {
    let after = counts(db).await;
    assert!(after.0 > before.0, "{op} missing successful audit");
    if matches!(op, "begin" | "notice_begin") {
        assert_eq!(recipient_state(db, lease).await.0, "ambiguous");
    }
    if op == "children" {
        assert_eq!(after.1, before.1 + 1);
        assert_eq!(after.2, before.2 + 1);
    }
}

async fn barrier_case(db: &Database, op: &str, expires: bool) {
    let lease = outgoing_fixture(db, op).await;
    if op == "claim" {
        db.mail_queue()
            .retry(&lease, 100, 0, "claim fixture")
            .await
            .unwrap();
    }
    let job_before = db.mail_queue().job(lease.job.id).await.unwrap();
    let recipient_before = recipient_state(db, &lease).await;
    let counts_before = counts(db).await;
    let result = wait_through_audit(db, &lease, op, expires).await;
    if expires {
        assert!(
            matches!(result, Err(listmngr_core::Error::Conflict(_))),
            "{op}: expired audit committed: {result:?}"
        );
        assert_eq!(
            db.mail_queue().job(lease.job.id).await.unwrap(),
            job_before,
            "{op} queue rollback"
        );
        assert_eq!(
            recipient_state(db, &lease).await,
            recipient_before,
            "{op} recipient rollback"
        );
        assert_eq!(
            counts(db).await,
            counts_before,
            "{op} child/recipient/audit rollback"
        );
        // Original opaque lease still works after rollback (unless prepared ready for claim).
        if op != "claim" {
            db.mail_queue().ack(&lease, 101).await.unwrap();
        }
    } else {
        result.unwrap();
        assert_success(db, &lease, op, counts_before).await;
    }
    // Own fixture only: remove ready/leased leftovers from subsequent claim selection.
    sqlx::query("UPDATE queue_jobs SET state='done',locked_by=NULL,lease_token=NULL,lease_until=NULL WHERE message_id=$1")
        .bind(lease.job.message_id.0.to_string()).execute(db.pool()).await.unwrap();
    eprintln!("observed PostgreSQL final audit barrier: {op}, expires={expires}: PASS");
}

#[tokio::test]
#[ignore = "requires explicit disposable TEST_POSTGRES_URL; owns isolated schema"]
async fn postgres_final_audit_wait_fences_queue_authority() {
    let url = std::env::var("TEST_POSTGRES_URL").expect("explicit fixture database required");
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("final_audit_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let isolated = format!(
        "{url}{}options=-csearch_path%3D{schema}",
        if url.contains('?') { '&' } else { '?' }
    );
    let result = tokio::spawn(async move {
        let db = Database::connect(&isolated, 4).await.unwrap();
        db.migrate().await.unwrap();
        for op in [
            "begin",
            "notice_begin",
            "finish",
            "notice_finish",
            "ack",
            "retry",
            "shunt",
            "children",
            "heartbeat",
            "claim",
        ] {
            for expires in [true, false] {
                barrier_case(&db, op, expires).await;
            }
        }
        db.pool().close().await;
    })
    .await;
    // Cleanup even when an assertion in the owned test task panics.
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    result.unwrap();
}
