use listmngr_core::{Error, ListId};
use listmngr_db::{
    Database, NewList,
    digests::DigestRecipient,
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
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: b"Subject: input\r\n\r\noriginal".to_vec(),
                external_id: uuid::Uuid::now_v7().to_string(),
                context: serde_json::json!({"list_id":list}).to_string(),
                queue: Queue::Digest,
                max_attempts: 3,
            },
            100,
        )
        .await
        .unwrap();
    db.mail_queue()
        .claim(Queue::Digest, "owner", 100, 100)
        .await
        .unwrap()
        .unwrap()
}
const RAW: &[u8] = b"Subject: safe digest post\r\n\r\nprivate cooked content";
async fn complete(
    db: &Database,
    lease: &Lease,
    clock: &Clock,
    empty: bool,
) -> listmngr_core::Result<()> {
    let recipients = if empty {
        vec![]
    } else {
        vec![DigestRecipient {
            email: "MiXeD@example.invalid".into(),
            mode: "mime_digests".into(),
        }]
    };
    db.digests()
        .with_clock(clock)
        .collect(
            lease,
            &"audit.example.invalid".parse().unwrap(),
            RAW,
            &recipients,
            101,
        )
        .await
}
type DigestPostRow = (String, String, Vec<u8>, String, i64, Option<String>);

#[derive(Debug, PartialEq)]
struct Snapshot {
    posts: Vec<DigestPostRow>,
    counts: (i64, i64, i64, i64, i64, i64, i64),
    settings: (i64, i64, Option<String>),
}
async fn snapshot(db: &Database) -> Snapshot {
    Snapshot {
        posts: sqlx::query_as("SELECT id,list_id,raw,recipients,accepted_at,issue_id FROM digest_posts ORDER BY id").fetch_all(db.pool()).await.unwrap(),
        counts: sqlx::query_as("SELECT (SELECT COUNT(*) FROM audit_log),(SELECT COUNT(*) FROM queue_jobs),(SELECT COUNT(*) FROM delivery_recipients),(SELECT COUNT(*) FROM messages),(SELECT COUNT(*) FROM message_blobs),(SELECT COUNT(*) FROM digest_issues),(SELECT COUNT(*) FROM digest_deliveries)").fetch_one(db.pool()).await.unwrap(),
        settings: sqlx::query_as("SELECT volume,next_digest_number,digest_last_sent_at FROM mailing_lists WHERE list_id='audit.example.invalid'").fetch_one(db.pool()).await.unwrap(),
    }
}
async fn success(db: &Database, lease: &Lease, before: &Snapshot, empty: bool) {
    let after = snapshot(db).await;
    let mut expected_counts = before.counts;
    expected_counts.0 += 1;
    assert_eq!(after.counts, expected_counts);
    assert_eq!(after.settings, before.settings);
    assert_eq!(after.posts.len(), before.posts.len() + 1);
    for post in &before.posts {
        assert!(after.posts.contains(post));
    }
    let post = after
        .posts
        .iter()
        .find(|p| p.0 == lease.job.message_id.0.to_string())
        .unwrap();
    assert_eq!(post.1, "audit.example.invalid");
    assert_eq!(post.2, RAW);
    let recipients: serde_json::Value = serde_json::from_str(&post.3).unwrap();
    assert_eq!(
        recipients,
        if empty {
            serde_json::json!([])
        } else {
            serde_json::json!([{"email":"MiXeD@example.invalid","mode":"mime_digests"}])
        }
    );
    assert_eq!(post.4, 101);
    assert!(post.5.is_none());
    let job = db.mail_queue().job(lease.job.id).await.unwrap();
    assert_eq!(job.state, listmngr_db::mail_queue::JobState::Done);
    assert!(job.lease_until.is_none());
}
#[tokio::test]
async fn sqlite_digest_final_audit_rollback_valid_and_renewed() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    let list = seed(&db).await;
    for empty in [false, true] {
        for (final_ms, renewed, expires) in [
            (200, false, true),
            (199, false, false),
            (250, true, false),
            (401, true, true),
        ] {
            let lease = fixture(&db, &list).await;
            if renewed {
                db.mail_queue().heartbeat(&lease, 101, 300).await.unwrap();
            }
            let job = db.mail_queue().job(lease.job.id).await.unwrap();
            let before = snapshot(&db).await;
            let result = complete(&db, &lease, &Clock::new(2, final_ms), empty).await;
            if expires {
                assert!(
                    matches!(result, Err(Error::Conflict(_))),
                    "digest final ACK audit expiry committed: {result:?}"
                );
                assert_eq!(db.mail_queue().job(lease.job.id).await.unwrap(), job);
                assert_eq!(snapshot(&db).await, before);
                complete(&db, &lease, &Clock::new(0, 101), empty)
                    .await
                    .unwrap();
            } else {
                result.unwrap();
            }
            success(&db, &lease, &before, empty).await;
        }
    }
    db.pool().close().await;
}
#[path = "digest_final_audit/postgres.rs"]
mod postgres;
