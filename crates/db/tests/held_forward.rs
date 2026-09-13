//! Mailman's `forward` on a moderator decision: a copy of the held post,
//! wrapped as `message/rfc822`, goes to the address the moderator names,
//! from the list's bounces address, whatever the decision.
use listmngr_core::{Error, ListId};
use listmngr_db::mail_queue::{NewMessage, Queue};
use listmngr_db::moderation::{HeldMessage, ReviewAction};
use listmngr_db::{AuditContext, Database, NewList};

const RAW: &[u8] = b"From: author@example.invalid\r\nSubject: private subject\r\nMessage-ID: <original@example.invalid>\r\n\r\nPRIVATE BODY";

async fn fixture() -> (Database, HeldMessage) {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "example", None)
        .await
        .unwrap();
    let list: ListId = "dev.example.invalid".parse().unwrap();
    db.lists()
        .create(NewList {
            list_id: list.clone(),
            display_name: "Dev".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    db.lists()
        .update(
            &list,
            &serde_json::json!({"respond_to_post_requests": false, "admin_immed_notify": false}),
        )
        .await
        .unwrap();
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: RAW.to_vec(),
                external_id: "<original@example.invalid>".into(),
                context: serde_json::json!({"list_id": list, "envelope_sender": "author@example.invalid"}).to_string(),
                queue: Queue::In,
                max_attempts: 3,
            },
            100,
        )
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::In, "worker", 100, 100)
        .await
        .unwrap()
        .unwrap();
    let held = db
        .moderation()
        .hold(
            &lease,
            &list,
            "author@example.invalid",
            "private subject",
            "initial hold",
            101,
        )
        .await
        .unwrap();
    (db, held)
}

/// Every outgoing message as (recipients, raw).
async fn outgoing(db: &Database) -> Vec<(Vec<String>, Vec<u8>)> {
    let mut out = Vec::new();
    while let Some(lease) = db
        .mail_queue()
        .claim(Queue::Out, "out", 300, 100)
        .await
        .unwrap()
    {
        let message = db.mail_queue().message(lease.job.message_id).await.unwrap();
        let recipients = db
            .mail_queue()
            .pending_recipients(lease.job.id)
            .await
            .unwrap();
        out.push((recipients, message.raw));
    }
    out
}

#[tokio::test]
async fn a_forwarded_decision_sends_the_original_wrapped_to_the_named_address() {
    for action in [ReviewAction::Defer, ReviewAction::Discard] {
        let (db, held) = fixture().await;
        db.moderation()
            .review_forwarding(
                held.id,
                &AuditContext::system(),
                &action,
                "please look",
                Some("Colleague@Example.NET"),
                200,
            )
            .await
            .unwrap();
        let out = outgoing(&db).await;
        assert_eq!(out.len(), 1, "one forward, no delivery: {action:?}");
        let (recipients, raw) = &out[0];
        assert_eq!(recipients, &["colleague@example.net"]);
        let text = String::from_utf8_lossy(raw);
        assert!(
            text.contains("Subject: Forward of moderated message"),
            "{text}"
        );
        assert!(text.contains("From: dev-bounces@example.invalid"), "{text}");
        assert!(text.contains("Content-Type: message/rfc822"), "{text}");
        assert!(text.contains("Subject: private subject"), "{text}");
        assert!(text.contains("PRIVATE BODY"), "{text}");
        let (logged_action, forward_to): (String, Option<String>) =
            sqlx::query_as("SELECT action, forward_to FROM moderation_log WHERE held_id=$1")
                .bind(held.id.0.to_string())
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(forward_to.as_deref(), Some("colleague@example.net"));
        let disposition: Option<String> =
            sqlx::query_scalar("SELECT disposition FROM held_messages WHERE id=$1")
                .bind(held.id.0.to_string())
                .fetch_one(db.pool())
                .await
                .unwrap();
        if matches!(action, ReviewAction::Defer) {
            assert_eq!(logged_action, "defer");
            assert_eq!(disposition, None, "a deferred post stays held");
        } else {
            assert_eq!(logged_action, "discarded");
            assert_eq!(disposition.as_deref(), Some("discarded"));
        }
        let audit: String = sqlx::query_scalar(
            "SELECT diff FROM audit_log WHERE action LIKE 'moderation.%' ORDER BY id DESC LIMIT 1",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert!(audit.contains("colleague@example.net"), "{audit}");
    }
}

#[tokio::test]
async fn a_bad_forward_address_refuses_the_whole_decision() {
    for bad in [
        "not an address",
        "dev@example.invalid",
        "dev-owner@example.invalid",
        "",
    ] {
        let (db, held) = fixture().await;
        let result = db
            .moderation()
            .review_forwarding(
                held.id,
                &AuditContext::system(),
                &ReviewAction::Discard,
                "",
                Some(bad),
                200,
            )
            .await;
        assert!(matches!(result, Err(Error::Validation(_))), "{bad:?}");
        let disposition: Option<String> =
            sqlx::query_scalar("SELECT disposition FROM held_messages WHERE id=$1")
                .bind(held.id.0.to_string())
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(disposition, None, "{bad:?}: nothing was decided");
        assert!(outgoing(&db).await.is_empty(), "{bad:?}: nothing was sent");
        let logged: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM moderation_log")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(logged, 0);
    }
}

#[tokio::test]
async fn without_forward_the_review_is_unchanged_and_logs_no_address() {
    let (db, held) = fixture().await;
    db.moderation()
        .review(
            held.id,
            &AuditContext::system(),
            &ReviewAction::Defer,
            "",
            200,
        )
        .await
        .unwrap();
    assert!(outgoing(&db).await.is_empty());
    let forward_to: Option<String> =
        sqlx::query_scalar("SELECT forward_to FROM moderation_log WHERE held_id=$1")
            .bind(held.id.0.to_string())
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(forward_to, None);
}
