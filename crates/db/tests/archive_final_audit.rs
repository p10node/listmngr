use listmngr_core::{Error, ListId};
use listmngr_db::{
    Database, NewList,
    archive::ArchiveMessage,
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

async fn seed(db: &Database) -> ListId {
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    let list: ListId = "audit.example.invalid".parse().unwrap();
    db.lists()
        .create(NewList {
            list_id: list.clone(),
            display_name: "Audit".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    list
}
async fn fixture(db: &Database, list: &ListId) -> Lease {
    db.mail_queue().enqueue(NewMessage {
        raw: b"From: author@example.invalid\r\nMessage-ID: <parent@example.invalid>\r\nSubject: parent\r\n\r\nbody".to_vec(),
        external_id: "<parent@example.invalid>".into(),
        context: serde_json::json!({"list_id":list.as_str()}).to_string(), queue: Queue::Archive, max_attempts: 3,
    }, 100).await.unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::Archive, "owner", 100, 100)
        .await
        .unwrap()
        .unwrap();
    // Completion must roll back both insertion and rethreading of an existing reply.
    sqlx::query("INSERT INTO archive_messages(list_id,hash,thread,subject,body,raw_b64,created_at) VALUES($1,$2,$3,'reply','existing body','',100)")
        .bind(list.as_str()).bind(format!("reply-{}", lease.job.id.0)).bind(lease.job.id.0.to_string()).execute(db.pool()).await.unwrap();
    lease
}
async fn complete(db: &Database, lease: &Lease, clock: &Clock) -> listmngr_core::Result<()> {
    db.archive()
        .with_clock(clock)
        .complete(
            lease,
            &ArchiveMessage {
                hash: lease.job.id.0.to_string(),
                thread: "root".into(),
                subject: String::new(),
                body: String::new(),
                raw: vec![],
            },
            101,
        )
        .await
}
#[derive(Debug, PartialEq)]
struct Snapshot {
    rows: Vec<(String, String, String, String, String, i64)>,
    counts: (i64, i64, i64, i64),
}
async fn snapshot(db: &Database) -> Snapshot {
    Snapshot {
        rows: sqlx::query_as("SELECT hash,thread,subject,body,raw_b64,created_at FROM archive_messages ORDER BY hash").fetch_all(db.pool()).await.unwrap(),
        counts: sqlx::query_as("SELECT (SELECT COUNT(*) FROM audit_log), (SELECT COUNT(*) FROM queue_jobs), (SELECT COUNT(*) FROM delivery_recipients), (SELECT COUNT(*) FROM messages)").fetch_one(db.pool()).await.unwrap(),
    }
}
async fn success(db: &Database, lease: &Lease, before: &Snapshot, never: bool) {
    let after = snapshot(db).await;
    assert_eq!(after.counts.0, before.counts.0 + if never { 1 } else { 2 });
    assert_eq!(
        (after.counts.1, after.counts.2, after.counts.3),
        (before.counts.1, before.counts.2, before.counts.3)
    );
    if never {
        assert_eq!(after.rows, before.rows);
    } else {
        assert_eq!(after.rows.len(), before.rows.len() + 1);
        let parent = after
            .rows
            .iter()
            .find(|r| r.0 == lease.job.id.0.to_string())
            .unwrap();
        assert_eq!(
            (&parent.1, &parent.2, &parent.3),
            (
                &"root".to_string(),
                &"[audit] parent".to_string(),
                &"body".to_string()
            )
        );
        assert_eq!(
            after
                .rows
                .iter()
                .find(|r| r.0 == format!("reply-{}", lease.job.id.0))
                .unwrap()
                .1,
            "root"
        );
    }
    let job = db.mail_queue().job(lease.job.id).await.unwrap();
    assert_eq!(job.state, listmngr_db::mail_queue::JobState::Done);
    assert!(job.lease_until.is_none());
}

#[tokio::test]
async fn sqlite_archive_final_audit_rollback_valid_and_renewed() {
    for never in [false, true] {
        let db = Database::connect("sqlite::memory:", 1).await.unwrap();
        let list = seed(&db).await;
        if never {
            db.lists()
                .update(&list, &serde_json::json!({"archive_policy":"never"}))
                .await
                .unwrap();
        }
        let lease = fixture(&db, &list).await;
        let before = snapshot(&db).await;
        let result = complete(&db, &lease, &Clock::new(2, 200)).await;
        assert!(
            matches!(result, Err(Error::Conflict(_))),
            "archive final ACK audit expiry committed: {result:?}"
        );
        assert_eq!(db.mail_queue().job(lease.job.id).await.unwrap(), lease.job);
        assert_eq!(snapshot(&db).await, before);
        complete(&db, &lease, &Clock::new(0, 199)).await.unwrap();
        success(&db, &lease, &before, never).await;
        let renewed = fixture(&db, &list).await;
        db.mail_queue().heartbeat(&renewed, 101, 300).await.unwrap();
        assert_eq!(renewed.job.lease_until, Some(200));
        let before = snapshot(&db).await;
        complete(&db, &renewed, &Clock::new(2, 250)).await.unwrap();
        success(&db, &renewed, &before, never).await;
        db.pool().close().await;
    }
}

#[path = "archive_final_audit/postgres.rs"]
mod postgres;
