//! An e-mail `-join`/`-leave` is authorised by an unauthenticated envelope
//! sender. Under the `open` policies it acts at once only for an address the
//! system already verified; any other mailbox gets the confirmation token
//! (security finding F-19), and the hourly per-address cooldown applies to
//! `open` lists too.
use listmngr_core::{MemberRole, SubscriptionMode};
use listmngr_db::{
    Database, NewList, NewMember,
    mail_queue::{NewMessage, Queue},
};

const LIST: &str = "dev.example.invalid";

async fn fixture() -> Database {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: LIST.parse().unwrap(),
            display_name: "Dev".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    db.lists()
        .update(
            &list.id,
            &serde_json::json!({"subscription_policy": "open", "unsubscription_policy": "open"}),
        )
        .await
        .unwrap();
    db
}

async fn add_member(db: &Database, email: &str) {
    db.members()
        .create(NewMember {
            list_id: LIST.parse().unwrap(),
            email: email.into(),
            display_name: String::new(),
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsAddress,
        })
        .await
        .unwrap();
}

/// Run one e-mail command from `sender` through the `in` queue.
async fn command(db: &Database, sender: &str, kind: &str, now: i64) {
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: format!("From: {sender}\r\n\r\n{kind}\r\n").into_bytes(),
                external_id: format!("<{}@example.invalid>", uuid::Uuid::now_v7()),
                context: serde_json::json!({"list_id": LIST, "envelope_sender": sender, "subscription_command": kind}).to_string(),
                queue: Queue::In,
                max_attempts: 5,
            },
            now,
        )
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::In, "test", now + 1, 30_000)
        .await
        .unwrap()
        .unwrap();
    db.workflows()
        .request_from_lease(&lease, now + 2)
        .await
        .unwrap();
}

async fn count(db: &Database, query: &str) -> i64 {
    sqlx::query_scalar(query)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn a_forged_leave_or_join_under_open_policy_waits_for_the_mailbox() {
    let db = fixture().await;
    add_member(&db, "victim@example.invalid").await;
    let now = chrono::Utc::now().timestamp_millis();
    // The address was never verified: the forged envelope sender proves nothing.
    command(&db, "victim@example.invalid", "leave", now).await;
    assert_eq!(count(&db, "SELECT COUNT(*) FROM members").await, 1);
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM subscription_workflows WHERE state='pending_confirmation'"
        )
        .await,
        1,
        "the mailbox gets a confirmation token instead"
    );
    // The cooldown holds for an open list: a second command makes no new request.
    command(&db, "victim@example.invalid", "leave", now + 10).await;
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM subscription_workflows").await,
        1
    );
    command(&db, "stranger@example.invalid", "join", now + 20).await;
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM members").await,
        1,
        "an unverified mailbox is not subscribed by a bare envelope sender"
    );
}

#[tokio::test]
async fn a_verified_address_still_joins_and_leaves_at_once() {
    let db = fixture().await;
    add_member(&db, "reader@example.invalid").await;
    sqlx::query("UPDATE addresses SET verified_on='2026-01-01T00:00:00+00:00'")
        .execute(db.pool())
        .await
        .unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    command(&db, "reader@example.invalid", "leave", now).await;
    assert_eq!(count(&db, "SELECT COUNT(*) FROM members").await, 0);
    // The hourly cooldown applies to open lists too: the join is sent after it.
    command(&db, "reader@example.invalid", "join", now + 3_700_000).await;
    assert_eq!(count(&db, "SELECT COUNT(*) FROM members").await, 1);
}
