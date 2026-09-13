use listmngr_core::Error;
use listmngr_db::{
    Database, NewList,
    mail_queue::{Lease, LeaseClock, NewMessage, Queue},
};
use std::sync::atomic::{AtomicI64, Ordering};
#[derive(Debug)]
struct Clock {
    remaining: AtomicI64,
    final_ms: AtomicI64,
}
impl Clock {
    const fn new(samples: i64, final_ms: i64) -> Self {
        Self {
            remaining: AtomicI64::new(samples),
            final_ms: AtomicI64::new(final_ms),
        }
    }
}
impl LeaseClock for Clock {
    fn now_ms(&self) -> i64 {
        if self.remaining.fetch_sub(1, Ordering::SeqCst) > 0 {
            101
        } else {
            self.final_ms.load(Ordering::SeqCst)
        }
    }
}
// Serialize every column, including deleted membership/preferences, consumed tokens,
// rate counters, byte blobs and audits, so equal counts cannot hide partial rollback.
async fn snapshot(db: &Database) -> Vec<String> {
    use sqlx::{Column, Row, TypeInfo, ValueRef};
    let mut result = Vec::new();
    for table in [
        "subscription_workflows",
        "subscription_rate",
        "email_help_requests",
        "workflow_notices",
        "members",
        "addresses",
        "preferences",
        "queue_jobs",
        "delivery_recipients",
        "messages",
        "message_blobs",
        "audit_log",
        "held_messages",
        "moderation_log",
    ] {
        let rows = sqlx::query(&format!("SELECT * FROM {table}"))
            .fetch_all(db.pool())
            .await
            .unwrap();
        let mut records = Vec::new();
        for row in rows {
            let mut values = Vec::new();
            for col in row.columns() {
                let i = col.ordinal();
                let raw = row.try_get_raw(i).unwrap();
                let value = if raw.is_null() {
                    "NULL".into()
                } else {
                    match raw.type_info().name() {
                        "BIGINT" | "INTEGER" | "SMALLINT" => {
                            format!("{:?}", row.try_get::<i64, _>(i).unwrap())
                        }
                        "DOUBLE" | "REAL" => format!("{:?}", row.try_get::<f64, _>(i).unwrap()),
                        "BLOB" | "BYTEA" => format!("{:?}", row.try_get::<Vec<u8>, _>(i).unwrap()),
                        _ => format!("{:?}", row.try_get::<String, _>(i).unwrap()),
                    }
                };
                values.push(value);
            }
            records.push(format!("{values:?}"));
        }
        records.sort();
        result.push(format!("{table}:{records:?}"));
    }
    result
}
async fn fixture(db: &Database) -> Lease {
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    db.lists()
        .create(NewList {
            list_id: "clock.example.invalid".parse().unwrap(),
            display_name: "Clock".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: b"Subject: held\r\n\r\noriginal".to_vec(),
                external_id: "held@example.invalid".into(),
                context: "{}".into(),
                queue: Queue::In,
                max_attempts: 3,
            },
            100,
        )
        .await
        .unwrap();
    db.mail_queue()
        .claim(Queue::In, "owner", 100, 100)
        .await
        .unwrap()
        .unwrap()
}
async fn complete(db: &Database, lease: &Lease, clock: &Clock) -> listmngr_core::Result<()> {
    db.moderation()
        .with_clock(clock)
        .hold(
            lease,
            &"clock.example.invalid".parse().unwrap(),
            "Case@example.invalid",
            "Held subject",
            "policy",
            101,
        )
        .await
        .map(|_| ())
}
async fn success(db: &Database, lease: &Lease) {
    assert_eq!(
        db.mail_queue().job(lease.job.id).await.unwrap().state,
        listmngr_db::mail_queue::JobState::Done
    );
    let held = db
        .moderation()
        .list_pending(&"clock.example.invalid".parse().unwrap())
        .await
        .unwrap();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].message_id, lease.job.message_id);
    assert_eq!(held[0].sender, "Case@example.invalid");
    assert_eq!(held[0].subject, "Held subject");
    assert_eq!(held[0].reason, "policy");
    let audits: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log WHERE action IN ('queue.ack','moderation.hold')",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(audits, 2);
}
#[tokio::test]
async fn sqlite_moderation_final_audit() {
    for (final_ms, renewed, expires) in [
        (200, false, true),
        (199, false, false),
        (250, true, false),
        (401, true, true),
    ] {
        let db = Database::connect("sqlite::memory:", 1).await.unwrap();
        let lease = fixture(&db).await;
        if renewed {
            db.mail_queue().heartbeat(&lease, 101, 300).await.unwrap();
        }
        let before = snapshot(&db).await;
        let result = complete(&db, &lease, &Clock::new(1, final_ms)).await;
        if expires {
            assert!(
                matches!(result, Err(Error::Conflict(_))),
                "hold final audit committed: {result:?}"
            );
            assert_eq!(snapshot(&db).await, before);
            complete(&db, &lease, &Clock::new(0, 101)).await.unwrap();
        } else {
            result.unwrap();
        }
        success(&db, &lease).await;
        db.pool().close().await;
    }
}

#[path = "moderation_final_audit/postgres.rs"]
mod postgres;
