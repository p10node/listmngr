use listmngr_core::{Error, MemberRole, SubscriptionMode};
use listmngr_db::{
    Database, NewList, NewMember,
    mail_queue::{JobState, Lease, LeaseClock, NewMessage, Queue},
};
use std::sync::atomic::{AtomicUsize, Ordering};

async fn fixture(owner: &str, raw: &[u8]) -> (Database, Lease) {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains().create("example.com", "", None).await.unwrap();
    db.lists()
        .create(NewList {
            list_id: "test.example.com".parse().unwrap(),
            display_name: "Test".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    if !owner.is_empty() {
        db.members()
            .create(NewMember {
                list_id: "test.example.com".parse().unwrap(),
                email: owner.into(),
                display_name: String::new(),
                role: MemberRole::Owner,
                subscription_mode: SubscriptionMode::AsUser,
            })
            .await
            .unwrap();
    }
    db.mail_queue().enqueue(NewMessage { raw: raw.to_vec(), external_id: "owner@example.net".into(), context: serde_json::json!({"list_id":"test.example.com", "owner_route":true, "envelope_sender":"Author@example.net"}).to_string(), queue: Queue::In, max_attempts: 3 }, 100).await.unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::In, "owner", 100, 100)
        .await
        .unwrap()
        .unwrap();
    (db, lease)
}
const RAW: &[u8] = b"From: Author@example.net\r\nMessage-ID: <owner@example.net>\r\n\r\nbody";

async fn assert_unchanged(db: &Database, lease: &Lease, audits: i64) {
    assert_eq!(
        db.mail_queue().job(lease.job.id).await.unwrap().state,
        JobState::Leased
    );
    for table in ["owner_deliveries", "delivery_recipients"] {
        assert_eq!(
            sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*) FROM {table}"))
                .fetch_one(db.pool())
                .await
                .unwrap(),
            0
        );
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM queue_jobs")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        1
    );
    assert_eq!(audit_count(db).await, audits);
}
async fn audit_count(db: &Database) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn owner_producer_refuses_route_recipients_and_automatic_raw() {
    for (owner, raw) in [
        ("test@example.com", RAW),
        ("TEST-request@EXAMPLE.COM", RAW),
        ("test-confirm+TOKEN@example.com", RAW),
        ("", RAW),
        (
            "Owner@example.net",
            b"Message-ID: <owner@example.net>\r\nAuto-Submitted: auto-generated\r\n\r\nbody"
                .as_slice(),
        ),
    ] {
        let (db, lease) = fixture(owner, raw).await;
        let audits = audit_count(&db).await;
        assert!(
            matches!(
                db.owner_mail().forward(&lease, 3, 101).await,
                Err(Error::Validation(_))
            ),
            "{owner}"
        );
        assert_unchanged(&db, &lease, audits).await;
    }
}

#[derive(Debug)]
struct ExpiringClock {
    calls: AtomicUsize,
    expire_on: usize,
}
impl LeaseClock for ExpiringClock {
    fn now_ms(&self) -> i64 {
        if self.calls.fetch_add(1, Ordering::SeqCst) >= self.expire_on {
            200
        } else {
            101
        }
    }
}
#[tokio::test]
async fn owner_completion_resamples_after_all_writes_and_rolls_back_audit() {
    for expire_on in [1, 2] {
        let (db, lease) = fixture("Owner@example.net", RAW).await;
        let audits = audit_count(&db).await;
        let clock = ExpiringClock {
            calls: AtomicUsize::new(0),
            expire_on,
        };
        assert!(matches!(
            db.owner_mail()
                .with_clock(&clock)
                .forward(&lease, 3, 101)
                .await,
            Err(Error::Conflict(_))
        ));
        assert_unchanged(&db, &lease, audits).await;
        db.owner_mail().forward(&lease, 3, 101).await.unwrap();
        assert!(db.owner_mail().forward(&lease, 3, 101).await.is_err());
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM owner_deliveries")
                .fetch_one(db.pool())
                .await
                .unwrap(),
            1
        );
    }
}

#[tokio::test]
async fn owner_audit_failure_rolls_back_and_valid_retry_succeeds() {
    let (db, lease) = fixture("Owner@example.net", RAW).await;
    let audits = audit_count(&db).await;
    sqlx::query("CREATE TRIGGER fail_owner BEFORE INSERT ON audit_log WHEN NEW.action='queue.ack' BEGIN SELECT RAISE(ABORT,'fixture audit failure'); END").execute(db.pool()).await.unwrap();
    assert!(db.owner_mail().forward(&lease, 3, 101).await.is_err());
    assert_unchanged(&db, &lease, audits).await;
    sqlx::query("DROP TRIGGER fail_owner")
        .execute(db.pool())
        .await
        .unwrap();
    db.owner_mail().forward(&lease, 3, 101).await.unwrap();
}
