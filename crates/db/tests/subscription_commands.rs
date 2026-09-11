use listmngr_db::{
    Database, NewList,
    mail_queue::{Lease, NewMessage, Queue},
};

async fn fixture() -> Database {
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
    db
}
async fn command(db: &Database, external: &str) -> Lease {
    db.mail_queue().enqueue(NewMessage { raw: b"From: Case@example.com\r\n\r\njoin\r\n".to_vec(), external_id: external.into(), context: serde_json::json!({"list_id":"test.example.com","envelope_sender":"Case@example.com","subscription_command":"join"}).to_string(), queue: Queue::In, max_attempts: 5 }, 100_000).await.unwrap();
    db.mail_queue()
        .claim(Queue::In, "fixture", 100_001, 30_000)
        .await
        .unwrap()
        .unwrap()
}
async fn count(db: &Database, query: &str) -> i64 {
    sqlx::query_scalar(query)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn failed_notice_audit_rolls_back_command_ack_and_all_business_writes() {
    let db = fixture().await;
    let lease = command(&db, "atomic@example.com").await;
    sqlx::query("CREATE TRIGGER fail_notice_audit BEFORE INSERT ON audit_log WHEN NEW.action='subscription.request' BEGIN SELECT RAISE(ABORT, 'fixture audit failure'); END").execute(db.pool()).await.unwrap();
    assert!(
        db.workflows()
            .request_from_lease(&lease, 100_002)
            .await
            .is_err()
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM queue_jobs WHERE state='leased'").await,
        1
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM subscription_workflows").await,
        0
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM queue_jobs WHERE queue='out'").await,
        0
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM audit_log WHERE action='queue.ack'"
        )
        .await,
        0
    );
    sqlx::query("DROP TRIGGER fail_notice_audit")
        .execute(db.pool())
        .await
        .unwrap();
    db.workflows()
        .request_from_lease(&lease, 100_003)
        .await
        .unwrap();
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM queue_jobs WHERE state='done'").await,
        1
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM subscription_workflows").await,
        1
    );
    assert!(
        db.workflows()
            .request_from_lease(&lease, 100_004)
            .await
            .is_err()
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM subscription_workflows").await,
        1
    );
}

#[tokio::test]
async fn rate_limited_command_is_acknowledged_without_a_second_notice() {
    let db = fixture().await;
    let first = command(&db, "one@example.com").await;
    db.workflows()
        .request_from_lease(&first, 100_002)
        .await
        .unwrap();
    let second = command(&db, "two@example.com").await;
    db.workflows()
        .request_from_lease(&second, 100_003)
        .await
        .unwrap();
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM queue_jobs WHERE state='done'").await,
        2
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM queue_jobs WHERE queue='out'").await,
        1
    );
    assert_eq!(count(&db, "SELECT COUNT(*) FROM members").await, 0);
}

async fn confirmation(db: &Database, token: &str, now: i64) -> Lease {
    db.mail_queue().enqueue(NewMessage { raw: b"From: Case@example.com\r\n\r\n".to_vec(), external_id: format!("confirm-{now}@example.com"), context: serde_json::json!({"list_id":"test.example.com","envelope_sender":"Case@example.com","subscription_command":{"confirm":token}}).to_string(), queue: Queue::In, max_attempts: 5 }, now).await.unwrap();
    db.mail_queue()
        .claim(Queue::In, "confirmation", now, 30_000)
        .await
        .unwrap()
        .unwrap()
}
async fn token(db: &Database) -> String {
    let raw: Vec<u8> = sqlx::query_scalar("SELECT b.raw FROM message_blobs b JOIN messages m ON m.store_key=b.store_key JOIN queue_jobs q ON q.message_id=m.id WHERE q.queue='out'").fetch_one(db.pool()).await.unwrap();
    String::from_utf8(raw)
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("Token: "))
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn confirmed_email_command_publishes_one_receipt_to_stored_mailbox() {
    let db = fixture().await;
    let list = "test.example.com".parse().unwrap();
    db.workflows()
        .request(
            &list,
            "Exact@example.com",
            listmngr_db::workflows::SubscriptionAction::Join,
            100_000,
        )
        .await
        .unwrap();
    let secret = token(&db).await;
    // The confirming envelope differs from the verified request mailbox.
    let lease = confirmation(&db, &secret, 100_001).await;
    db.workflows()
        .request_from_lease(&lease, 100_002)
        .await
        .unwrap();
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM queue_jobs WHERE queue='out'").await,
        2,
        "challenge plus completion receipt"
    );
    assert_eq!(count(&db, "SELECT COUNT(*) FROM members").await, 1);
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM delivery_recipients WHERE email='Exact@example.com'"
        )
        .await,
        2
    );
    assert_eq!(count(&db, "SELECT COUNT(*) FROM workflow_notices").await, 2);
    assert!(
        db.workflows()
            .request_from_lease(&lease, 100_003)
            .await
            .is_err()
    );
    assert!(
        db.workflows()
            .confirm(&list, &secret, 100_003)
            .await
            .is_err()
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM queue_jobs WHERE queue='out'").await,
        2
    );
}

#[tokio::test]
async fn completion_receipt_failures_roll_back_membership_token_spool_audit_and_ack() {
    for trigger in [
        "CREATE TRIGGER fail_completion BEFORE INSERT ON workflow_notices BEGIN SELECT RAISE(ABORT, 'receipt fault'); END",
        "CREATE TRIGGER fail_completion BEFORE INSERT ON audit_log WHEN NEW.action='subscription.confirm' BEGIN SELECT RAISE(ABORT, 'audit fault'); END",
        "CREATE TRIGGER fail_completion BEFORE INSERT ON audit_log WHEN NEW.action='queue.ack' BEGIN SELECT RAISE(ABORT, 'ack fault'); END",
    ] {
        let db = fixture().await;
        let request = command(&db, "completion-rollback@example.com").await;
        db.workflows()
            .request_from_lease(&request, 100_002)
            .await
            .unwrap();
        let lease = confirmation(&db, &token(&db).await, 100_003).await;
        let tables = [
            "members",
            "preferences",
            "addresses",
            "message_blobs",
            "messages",
            "queue_jobs",
            "workflow_notices",
            "delivery_recipients",
            "audit_log",
        ];
        let mut before = Vec::new();
        for table in tables {
            before.push(count(&db, &format!("SELECT COUNT(*) FROM {table}")).await);
        }
        sqlx::query(trigger).execute(db.pool()).await.unwrap();
        assert!(
            db.workflows()
                .request_from_lease(&lease, 100_004)
                .await
                .is_err()
        );
        for (table, expected) in tables.into_iter().zip(before) {
            assert_eq!(
                count(&db, &format!("SELECT COUNT(*) FROM {table}")).await,
                expected,
                "partial write in {table}"
            );
        }
        assert_eq!(
            count(
                &db,
                "SELECT COUNT(*) FROM subscription_workflows WHERE consumed=0"
            )
            .await,
            1
        );
        assert_eq!(
            db.mail_queue().job(lease.job.id).await.unwrap().state,
            listmngr_db::mail_queue::JobState::Leased
        );
        sqlx::query("DROP TRIGGER fail_completion")
            .execute(db.pool())
            .await
            .unwrap();
        db.workflows()
            .request_from_lease(&lease, 100_005)
            .await
            .unwrap();
        assert_eq!(count(&db, "SELECT COUNT(*) FROM members").await, 1);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM workflow_notices").await, 2);
        assert_eq!(
            count(
                &db,
                "SELECT COUNT(*) FROM subscription_workflows WHERE consumed=1"
            )
            .await,
            1
        );
    }
}

#[tokio::test]
async fn banned_join_command_acknowledges_without_notice_and_unban_allows_retry() {
    let db = fixture().await;
    let list = "test.example.com".parse().unwrap();
    db.bans()
        .create(
            &list,
            "case@example.com",
            &listmngr_db::AuditContext::system(),
        )
        .await
        .unwrap();
    let request = command(&db, "banned@example.com").await;
    db.workflows()
        .request_from_lease(&request, 100_002)
        .await
        .unwrap();
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM queue_jobs WHERE state='done'").await,
        1
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM subscription_workflows").await,
        0
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM queue_jobs WHERE queue='out'").await,
        0
    );
    db.bans()
        .delete(
            &list,
            "case@example.com",
            &listmngr_db::AuditContext::system(),
        )
        .await
        .unwrap();
    let retry = command(&db, "allowed@example.com").await;
    db.workflows()
        .request_from_lease(&retry, 100_003)
        .await
        .unwrap();
    let secret = token(&db).await;
    let reply = confirmation(&db, &secret, 101_000).await;
    db.bans()
        .create(&list, "^Case@", &listmngr_db::AuditContext::system())
        .await
        .unwrap();
    assert!(
        db.workflows()
            .request_from_lease(&reply, 101_001)
            .await
            .is_err()
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM subscription_workflows WHERE consumed=1"
        )
        .await,
        0
    );
    assert_eq!(count(&db, "SELECT COUNT(*) FROM members").await, 0);
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM queue_jobs WHERE state='leased'").await,
        1
    );
    db.bans()
        .delete(&list, "^Case@", &listmngr_db::AuditContext::system())
        .await
        .unwrap();
    db.workflows()
        .request_from_lease(&reply, 101_002)
        .await
        .unwrap();
    assert_eq!(count(&db, "SELECT COUNT(*) FROM members").await, 1);
}

#[tokio::test]
async fn confirmation_audit_failure_rolls_back_ack_token_and_membership_then_replays_fail() {
    let db = fixture().await;
    let request = command(&db, "request@example.com").await;
    db.workflows()
        .request_from_lease(&request, 100_002)
        .await
        .unwrap();
    let token = token(&db).await;
    let reply = confirmation(&db, &token, 101_000).await;
    sqlx::query("CREATE TRIGGER fail_confirm BEFORE INSERT ON audit_log WHEN NEW.action='subscription.confirm' BEGIN SELECT RAISE(ABORT, 'fixture confirmation audit failure'); END").execute(db.pool()).await.unwrap();
    assert!(
        db.workflows()
            .request_from_lease(&reply, 101_001)
            .await
            .is_err()
    );
    assert_eq!(count(&db, "SELECT COUNT(*) FROM members").await, 0);
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM subscription_workflows WHERE consumed=1"
        )
        .await,
        0
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM queue_jobs WHERE state='leased'").await,
        1
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM audit_log WHERE action='queue.ack'"
        )
        .await,
        1
    );
    sqlx::query("DROP TRIGGER fail_confirm")
        .execute(db.pool())
        .await
        .unwrap();
    db.workflows()
        .request_from_lease(&reply, 101_002)
        .await
        .unwrap();
    assert_eq!(count(&db, "SELECT COUNT(*) FROM members").await, 1);
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM subscription_workflows WHERE consumed=1"
        )
        .await,
        1
    );
    assert!(
        db.workflows()
            .request_from_lease(&reply, 101_003)
            .await
            .is_err()
    );
    let duplicate = confirmation(&db, &token, 102_000).await;
    assert!(
        db.workflows()
            .request_from_lease(&duplicate, 102_001)
            .await
            .is_err()
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM audit_log WHERE action='subscription.confirm'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn invalid_expired_wrong_list_and_stale_confirmation_never_consume() {
    for case in ["invalid", "expired", "wrong-list", "stale"] {
        let db = fixture().await;
        let request = command(&db, "request@example.com").await;
        db.workflows()
            .request_from_lease(&request, 100_002)
            .await
            .unwrap();
        let token = if case == "invalid" {
            "A".repeat(43)
        } else {
            token(&db).await
        };
        let reply = confirmation(&db, &token, 101_000).await;
        if case == "expired" {
            sqlx::query("UPDATE subscription_workflows SET expires_at=101001")
                .execute(db.pool())
                .await
                .unwrap();
        }
        if case == "wrong-list" {
            sqlx::query("UPDATE messages SET context=replace(context,'test.example.com','other.example.com') WHERE id=$1").bind(reply.job.message_id.0.to_string()).execute(db.pool()).await.unwrap();
        }
        let now = if case == "stale" { 131_000 } else { 101_001 };
        assert!(
            db.workflows()
                .request_from_lease(&reply, now)
                .await
                .is_err(),
            "{case}"
        );
        assert_eq!(
            count(&db, "SELECT COUNT(*) FROM members").await,
            0,
            "{case}"
        );
        assert_eq!(
            count(
                &db,
                "SELECT COUNT(*) FROM subscription_workflows WHERE consumed=1"
            )
            .await,
            0,
            "{case}"
        );
        assert_eq!(
            count(&db, "SELECT COUNT(*) FROM queue_jobs WHERE state='leased'").await,
            1,
            "{case}"
        );
    }
}
