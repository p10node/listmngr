use listmngr_db::{
    AuditContext, Database,
    mail_queue::{JobId, JobState, NewMessage, Queue},
};

async fn seed(db: &Database, queue: Queue) -> JobId {
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: b"private-report\x00\xff".to_vec(),
                external_id: "fixture".into(),
                context: "private-context".into(),
                queue,
                max_attempts: 3,
            },
            100,
        )
        .await
        .unwrap()
        .id
}

#[tokio::test]
async fn acknowledgement_audit_sabotage_rolls_back_then_retry_commits_once() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let id = seed(&db, Queue::Bounces).await;
    let before = db.mail_queue().job(id).await.unwrap();
    let message = db.mail_queue().message(before.message_id).await.unwrap();
    sqlx::query("CREATE TRIGGER reject_ack BEFORE INSERT ON audit_log WHEN NEW.action='queue.acknowledge_bounce' BEGIN SELECT RAISE(ABORT, 'fixture audit failure'); END").execute(db.pool()).await.unwrap();
    assert!(
        db.acknowledge_bounce(id, "reviewed", &AuditContext::system())
            .await
            .is_err()
    );
    assert_eq!(db.mail_queue().job(id).await.unwrap(), before);
    assert_eq!(
        db.mail_queue().message(before.message_id).await.unwrap(),
        message
    );
    sqlx::query("DROP TRIGGER reject_ack")
        .execute(db.pool())
        .await
        .unwrap();
    db.acknowledge_bounce(id, "reviewed", &AuditContext::system())
        .await
        .unwrap();
    let mut expected = before;
    expected.state = JobState::Done;
    assert_eq!(db.mail_queue().job(id).await.unwrap(), expected);
    let audit: (String, String, String) =
        sqlx::query_as("SELECT id,at,diff FROM audit_log WHERE action='queue.acknowledge_bounce'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(
        db.acknowledge_bounce(id, "repeat", &AuditContext::system())
            .await
            .is_err()
    );
    let after: Vec<(String, String, String)> =
        sqlx::query_as("SELECT id,at,diff FROM audit_log WHERE action='queue.acknowledge_bounce'")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(after, [audit]);
    assert_eq!(db.mail_queue().job(id).await.unwrap(), expected);
    assert_eq!(
        db.mail_queue().message(expected.message_id).await.unwrap(),
        message
    );
}

#[tokio::test]
async fn acknowledgement_rejects_wrong_queue_leases_shunts_missing_and_invalid_reasons() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let actor = AuditContext::system();
    let ordinary = seed(&db, Queue::In).await;
    let bounce = seed(&db, Queue::Bounces).await;
    for reason in [
        String::new(),
        " \t ".into(),
        "x\ny".into(),
        "é".repeat(1025),
    ] {
        assert!(
            db.acknowledge_bounce(bounce, &reason, &actor)
                .await
                .is_err()
        );
    }
    for id in [ordinary, JobId(uuid::Uuid::now_v7())] {
        assert!(db.acknowledge_bounce(id, "reviewed", &actor).await.is_err());
    }
    let lease = db
        .mail_queue()
        .claim(Queue::Bounces, "worker", 100, 1000)
        .await
        .unwrap()
        .unwrap();
    assert!(
        db.acknowledge_bounce(bounce, "reviewed", &actor)
            .await
            .is_err()
    );
    assert_eq!(db.mail_queue().job(bounce).await.unwrap(), lease.job);
    db.mail_queue().heartbeat(&lease, 101, 1000).await.unwrap();
    db.mail_queue()
        .shunt(&lease, 102, "quarantine")
        .await
        .unwrap();
    assert!(
        db.acknowledge_bounce(bounce, "reviewed", &actor)
            .await
            .is_err()
    );
    // Even a stored bounces job with a non-ready state must fail closed.
    sqlx::query("UPDATE queue_jobs SET queue='bounces' WHERE id=$1")
        .bind(bounce.0.to_string())
        .execute(db.pool())
        .await
        .unwrap();
    assert!(
        db.acknowledge_bounce(bounce, "reviewed", &actor)
            .await
            .is_err()
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log WHERE action='queue.acknowledge_bounce'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count, 0);
    let valid = seed(&db, Queue::Bounces).await;
    db.acknowledge_bounce(valid, &"é".repeat(1024), &actor)
        .await
        .unwrap();
}

#[tokio::test]
async fn acknowledgement_and_claim_have_only_one_winner() {
    struct TempDb(std::path::PathBuf);
    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let root = TempDb(std::env::temp_dir().join(uuid::Uuid::now_v7().to_string()));
    std::fs::create_dir(&root.0).unwrap();
    let url = format!("sqlite://{}?mode=rwc", root.0.join("race.db").display());
    let db = Database::connect(&url, 2).await.unwrap();
    db.migrate().await.unwrap();
    let first = db.pool().acquire().await.unwrap();
    let second = db.pool().acquire().await.unwrap();
    drop((first, second));
    let actor = AuditContext::system();
    let id = seed(&db, Queue::Bounces).await;
    let queue = db.mail_queue();
    let barrier = tokio::sync::Barrier::new(2);
    let (ack, claim) = tokio::join!(
        async {
            barrier.wait().await;
            db.acknowledge_bounce(id, "reviewed", &actor).await
        },
        async {
            barrier.wait().await;
            queue.claim(Queue::Bounces, "worker", 100, 1000).await
        },
    );
    let claim = claim.unwrap();
    assert_ne!(ack.is_ok(), claim.is_some());
    let audit_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log WHERE action='queue.acknowledge_bounce'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(audit_count, i64::from(ack.is_ok()));
    if let Some(lease) = claim {
        queue.ack(&lease, 101).await.unwrap();
    }
    assert_eq!(queue.job(id).await.unwrap().state, JobState::Done);
    assert!(
        queue
            .claim(Queue::Bounces, "later", 200, 1000)
            .await
            .unwrap()
            .is_none()
    );
    db.pool().close().await;
}
