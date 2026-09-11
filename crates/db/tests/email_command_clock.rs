use listmngr_core::Error;
use listmngr_db::{
    Database, NewList,
    mail_queue::{Lease, LeaseClock, NewMessage, Queue},
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
struct DeadlineOnSecondSample(AtomicI64);
impl LeaseClock for DeadlineOnSecondSample {
    fn now_ms(&self) -> i64 {
        self.0.fetch_add(1, Ordering::SeqCst)
    }
}
async fn fixture(command: serde_json::Value) -> (Database, Lease) {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
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
    db.mail_queue().enqueue(NewMessage {
        raw: b"Subject: command\r\n\r\n".to_vec(),
        external_id: "command@example.invalid".into(),
        context: serde_json::json!({"list_id":"clock.example.invalid", "envelope_sender":"Case@example.invalid", "subscription_command":command}).to_string(),
        queue: Queue::In, max_attempts:3,
    },100).await.unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::In, "worker", 100, 100)
        .await
        .unwrap()
        .unwrap();
    (db, lease)
}
async fn counts(db: &Database) -> (i64, i64, i64, i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT COUNT(*) FROM audit_log),(SELECT COUNT(*) FROM subscription_workflows),(SELECT COUNT(*) FROM email_help_requests),(SELECT COUNT(*) FROM workflow_notices),(SELECT COUNT(*) FROM messages),(SELECT COUNT(*) FROM members)").fetch_one(db.pool()).await.unwrap()
}

#[tokio::test]
async fn command_pool_wait_uses_live_authority_and_valid_control_commits() {
    for command in ["join", "leave", "help"] {
        let (db, lease) = fixture(serde_json::json!(command)).await;
        let before = counts(&db).await;
        let clock = Clock(AtomicI64::new(101));
        let held = db.pool().acquire().await.unwrap();
        let repo = db.workflows().with_clock(&clock);
        let operation = repo.request_from_lease(&lease, 101);
        tokio::pin!(operation);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut operation)
                .await
                .is_err()
        );
        clock.0.store(200, Ordering::SeqCst);
        drop(held);
        assert!(
            matches!(operation.await, Err(Error::Conflict(_))),
            "{command}"
        );
        assert_eq!(counts(&db).await, before);
        assert_eq!(db.mail_queue().job(lease.job.id).await.unwrap(), lease.job);
        clock.0.store(199, Ordering::SeqCst);
        db.workflows()
            .with_clock(&clock)
            .request_from_lease(&lease, 101)
            .await
            .unwrap();
        assert_eq!(
            db.mail_queue().job(lease.job.id).await.unwrap().state,
            listmngr_db::mail_queue::JobState::Done
        );
        assert_eq!(counts(&db).await.3, 1);
    }
}

#[tokio::test]
async fn command_rechecks_deadline_after_business_writes_before_ack() {
    for command in ["join", "leave", "help"] {
        let (db, lease) = fixture(serde_json::json!(command)).await;
        let before = counts(&db).await;
        let clock = DeadlineOnSecondSample(AtomicI64::new(199));
        let result = db
            .workflows()
            .with_clock(&clock)
            .request_from_lease(&lease, 101)
            .await;
        assert!(
            matches!(result, Err(Error::Conflict(_))),
            "{command}: {result:?}"
        );
        assert_eq!(
            counts(&db).await,
            before,
            "{command}: business/audit writes must roll back"
        );
        assert_eq!(db.mail_queue().job(lease.job.id).await.unwrap(), lease.job);
    }
}

#[tokio::test]
async fn confirmation_deadline_rolls_back_token_membership_and_ack() {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    for action in ["join", "leave"] {
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([0_u8; 32]);
        let (db, lease) = fixture(serde_json::json!({"confirm":token})).await;
        if action == "leave" {
            db.members()
                .create(listmngr_db::NewMember {
                    list_id: "clock.example.invalid".parse().unwrap(),
                    email: "Case@example.invalid".into(),
                    display_name: String::new(),
                    role: listmngr_core::MemberRole::Member,
                    subscription_mode: listmngr_core::SubscriptionMode::AsAddress,
                })
                .await
                .unwrap();
        }
        sqlx::query("INSERT INTO subscription_workflows(id,list_id,email,original_email,action,token_hash,created_at,expires_at) VALUES('fixture','clock.example.invalid','case@example.invalid','Case@example.invalid',$1,$2,100,100000)")
            .bind(action).bind(format!("{:x}",Sha256::digest([0_u8;32]))).execute(db.pool()).await.unwrap();
        let before = counts(&db).await;
        let clock = DeadlineOnSecondSample(AtomicI64::new(199));
        let result = db
            .workflows()
            .with_clock(&clock)
            .request_from_lease(&lease, 101)
            .await;
        assert!(
            matches!(result, Err(Error::Conflict(_))),
            "{action}: {result:?}"
        );
        assert_eq!(counts(&db).await, before);
        let consumed: i64 = sqlx::query_scalar("SELECT consumed FROM subscription_workflows")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(consumed, 0);
        assert_eq!(db.mail_queue().job(lease.job.id).await.unwrap(), lease.job);
        db.workflows()
            .with_clock(&Clock(AtomicI64::new(199)))
            .request_from_lease(&lease, 101)
            .await
            .unwrap();
        let consumed: i64 = sqlx::query_scalar("SELECT consumed FROM subscription_workflows")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(consumed, 1);
        assert_eq!(counts(&db).await.5, i64::from(action == "join"));
    }
}

#[tokio::test]
async fn throttled_and_missing_list_commands_still_fence_the_ack() {
    for command in ["join", "leave", "help"] {
        for missing in [true, false] {
            let (db, lease) = fixture(serde_json::json!(command)).await;
            if missing {
                db.lists()
                    .delete(&"clock.example.invalid".parse().unwrap())
                    .await
                    .unwrap();
            } else {
                sqlx::query(
                    "UPDATE subscription_rate SET requests=100,window_start=100 WHERE id=1",
                )
                .execute(db.pool())
                .await
                .unwrap();
            }
            let before = counts(&db).await;
            let clock = DeadlineOnSecondSample(AtomicI64::new(199));
            assert!(matches!(
                db.workflows()
                    .with_clock(&clock)
                    .request_from_lease(&lease, 101)
                    .await,
                Err(Error::Conflict(_))
            ));
            assert_eq!(counts(&db).await, before);
            db.workflows()
                .with_clock(&Clock(AtomicI64::new(199)))
                .request_from_lease(&lease, 101)
                .await
                .unwrap();
            assert_eq!(counts(&db).await.3, 0);
            assert_eq!(
                db.mail_queue().job(lease.job.id).await.unwrap().state,
                listmngr_db::mail_queue::JobState::Done
            );
        }
    }
}
