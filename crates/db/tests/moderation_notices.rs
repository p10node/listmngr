use listmngr_core::ListId;
use listmngr_db::mail_queue::{NewMessage, Queue};
use listmngr_db::moderation::{HeldMessage, ReviewAction};
use listmngr_db::{AuditContext, Database, NewList};

const RAW: &[u8] = b"From: header@example.invalid\r\nReply-To: private@example.invalid\r\nSubject: private subject\r\nMessage-ID: <original@example.invalid>\r\nX-Private: secret\r\n\r\nPRIVATE BODY";

async fn fixture(sender: &str, context: serde_json::Value, raw: &[u8]) -> (Database, HeldMessage) {
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
    // These fixtures exercise moderation, not hold notices: keep the queue
    // limited to the posts and the notices under test.
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
                raw: raw.to_vec(),
                external_id: "original".into(),
                context: context.to_string(),
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
            sender,
            "private held subject",
            "initial hold",
            101,
        )
        .await
        .unwrap();
    (db, held)
}

#[tokio::test]
async fn rejection_comment_is_utf8_bounded_but_full_reason_is_durable() {
    let (db, held) = fixture(
        "author@example.invalid",
        context("author@example.invalid"),
        RAW,
    )
    .await;
    let reason = "ế".repeat(2000);
    reject(&db, &held, false, &reason).await.unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::Out, "out", 201, 100)
        .await
        .unwrap()
        .unwrap();
    let message = db.mail_queue().message(lease.job.message_id).await.unwrap();
    let parsed = mail_parser::MessageParser::default()
        .parse(&message.raw)
        .unwrap();
    let body = parsed.body_text(0).unwrap();
    let comment = body.split("following reasons:\r\n\r\n").nth(1).unwrap();
    assert!(comment.len() <= 4096 + "\r\n[Comment truncated]\r\n".len());
    assert!(comment.ends_with("\r\n[Comment truncated]\r\n"));
    assert!(!comment.contains('\u{fffd}'));
    assert!(message.raw.len() < 6500);
    let saved: String = sqlx::query_scalar("SELECT reason FROM moderation_log WHERE held_id=$1")
        .bind(held.id.0.to_string())
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(saved, reason);
}

fn unsafe_cases() -> Vec<(&'static str, serde_json::Value, Vec<u8>)> {
    let mut cases = vec![
        (
            "author@example.invalid",
            serde_json::json!({}),
            RAW.to_vec(),
        ),
        (
            "author@example.invalid",
            serde_json::json!({"list_id":"dev.example.invalid", "envelope_sender":null}),
            RAW.to_vec(),
        ),
        ("author@example.invalid", context(""), RAW.to_vec()),
        ("author@example.invalid", context("<>"), RAW.to_vec()),
        (
            "author@example.invalid",
            context("other@example.invalid"),
            RAW.to_vec(),
        ),
        (
            "author@example.invalid",
            context("a@example.invalid\r\nBcc: bad@example.invalid"),
            RAW.to_vec(),
        ),
        (
            "author@example.invalid",
            context("a..b@example.invalid"),
            RAW.to_vec(),
        ),
        (
            "author@example.invalid",
            context("ü@example.invalid"),
            RAW.to_vec(),
        ),
        (
            "dev@example.invalid",
            context("dev@example.invalid"),
            RAW.to_vec(),
        ),
        (
            "dev-owner@example.invalid",
            context("dev-owner@example.invalid"),
            RAW.to_vec(),
        ),
        (
            "dev+unknown@example.invalid",
            context("dev+unknown@example.invalid"),
            RAW.to_vec(),
        ),
        (
            "author@example.invalid",
            serde_json::json!({"list_id":"other.example.invalid", "envelope_sender":"author@example.invalid"}),
            RAW.to_vec(),
        ),
        (
            "author@example.invalid",
            context("author@example.invalid"),
            b"not MIME".to_vec(),
        ),
    ];
    for header in [
        "Auto-Submitted: auto-generated",
        "Auto-Submitted: no\r\nAuto-Submitted: auto-replied",
        "List-Id: other.example.invalid",
        "Precedence: bulk",
        "X-Auto-Response-Suppress: All",
        "Resent-From: x@example.invalid",
        "X-Loop: list",
        "Broken header",
        "X-Bad: value\rbroken",
    ] {
        let mut raw = format!("{header}\r\n").into_bytes();
        raw.extend_from_slice(RAW);
        cases.push((
            "author@example.invalid",
            context("author@example.invalid"),
            raw,
        ));
    }
    for (key, value) in [
        ("owner_route", serde_json::json!(true)),
        ("subscription_command", serde_json::json!("help")),
    ] {
        let mut route = context("author@example.invalid");
        route[key] = value;
        cases.push(("author@example.invalid", route, RAW.to_vec()));
    }
    cases
}

#[tokio::test]
async fn unsafe_rejection_targets_and_loop_messages_are_suppressed_on_both_paths() {
    let cases = unsafe_cases();
    for legacy in [false, true] {
        for (sender, context, raw) in &cases {
            let (db, held) = fixture(sender, context.clone(), raw).await;
            reject(&db, &held, legacy, "persist despite suppression")
                .await
                .unwrap();
            assert_eq!(
                db.moderation().get(held.id).await.unwrap().disposition,
                Some(listmngr_db::moderation::Disposition::Rejected)
            );
            let notices: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workflow_notices")
                .fetch_one(db.pool())
                .await
                .unwrap();
            assert_eq!(notices, 0, "{context}");
            let messages: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM messages")
                .fetch_one(db.pool())
                .await
                .unwrap();
            assert_eq!(messages, 1);
            let saved: String =
                sqlx::query_scalar("SELECT reason FROM moderation_log WHERE held_id=$1")
                    .bind(held.id.0.to_string())
                    .fetch_one(db.pool())
                    .await
                    .unwrap();
            assert_eq!(saved, "persist despite suppression");
            assert_eq!(
                db.mail_queue().message(held.message_id).await.unwrap().raw,
                *raw
            );
        }
    }
}

#[tokio::test]
async fn rejection_audit_failure_rolls_back_everything_and_retry_publishes_once() {
    for legacy in [false, true] {
        let (db, held) = fixture(
            "author@example.invalid",
            context("author@example.invalid"),
            RAW,
        )
        .await;
        sqlx::query("CREATE TRIGGER fail_rejection_audit BEFORE INSERT ON audit_log WHEN NEW.action='moderation.rejected' BEGIN SELECT RAISE(ABORT, 'sabotage'); END").execute(db.pool()).await.unwrap();
        assert!(reject(&db, &held, legacy, "retry me").await.is_err());
        assert_eq!(
            db.moderation().get(held.id).await.unwrap().disposition,
            None
        );
        for (table, expected) in [
            ("messages", 1),
            ("message_blobs", 1),
            ("queue_jobs", 1),
            ("delivery_recipients", 0),
            ("workflow_notices", 0),
            ("moderation_log", 0),
        ] {
            let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
                .fetch_one(db.pool())
                .await
                .unwrap();
            assert_eq!(count, expected, "rollback {table}");
        }
        sqlx::query("DROP TRIGGER fail_rejection_audit")
            .execute(db.pool())
            .await
            .unwrap();
        reject(&db, &held, legacy, "retry me").await.unwrap();
        assert!(reject(&db, &held, legacy, "retry me").await.is_err());
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workflow_notices")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(count, 1);
        let audits: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='moderation.rejected'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(audits, 1);
    }
}

#[tokio::test]
async fn non_rejection_decisions_never_publish_notices() {
    for action in [
        ReviewAction::Discard,
        ReviewAction::Defer,
        ReviewAction::Accept { max_attempts: 3 },
    ] {
        let (db, held) = fixture(
            "author@example.invalid",
            context("author@example.invalid"),
            RAW,
        )
        .await;
        db.moderation()
            .review(held.id, &AuditContext::system(), &action, "comment", 200)
            .await
            .unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workflow_notices")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(count, 0);
    }
    let (db, held) = fixture(
        "author@example.invalid",
        context("author@example.invalid"),
        RAW,
    )
    .await;
    db.moderation()
        .discard(held.id, None, "comment", 200)
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workflow_notices")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
}

fn context(sender: &str) -> serde_json::Value {
    serde_json::json!({"list_id":"dev.example.invalid", "envelope_sender":sender})
}

async fn reject(
    db: &Database,
    held: &HeldMessage,
    legacy: bool,
    reason: &str,
) -> listmngr_core::Result<()> {
    if legacy {
        db.moderation()
            .reject(held.id, None, reason, 200)
            .await
            .map(|_| ())
    } else {
        db.moderation()
            .review(
                held.id,
                &AuditContext::system(),
                &ReviewAction::Reject,
                reason,
                200,
            )
            .await
    }
}

#[tokio::test]
async fn rejection_publishes_private_single_recipient_notice_on_both_paths() {
    for legacy in [false, true] {
        let (db, held) = fixture(
            "author@example.invalid",
            context("Author@Example.Invalid"),
            RAW,
        )
        .await;
        let reason = "Please revise.\r\nBcc: injected@example.invalid\r\nLý do";
        reject(&db, &held, legacy, reason).await.unwrap();
        let lease = db
            .mail_queue()
            .claim(Queue::Out, "out", 201, 100)
            .await
            .unwrap()
            .expect("rejection notice");
        assert_ne!(lease.job.message_id, held.message_id);
        assert!(db.workflows().is_notice(lease.job.id).await.unwrap());
        assert_eq!(
            db.mail_queue()
                .pending_recipients(lease.job.id)
                .await
                .unwrap(),
            ["Author@Example.Invalid"]
        );
        let message = db.mail_queue().message(lease.job.message_id).await.unwrap();
        let parsed = mail_parser::MessageParser::default()
            .parse(&message.raw)
            .unwrap();
        assert_eq!(
            parsed.subject(),
            Some("Request to mailing list \"Dev\" rejected")
        );
        assert!(parsed.body_text(0).unwrap().contains(reason));
        assert!(parsed.header("Bcc").is_none());
        let text = String::from_utf8(message.raw).unwrap();
        for private in [
            "PRIVATE BODY",
            "private subject",
            "private held subject",
            "X-Private",
            "Reply-To:",
            "original@example.invalid",
        ] {
            assert!(!text.contains(private), "leaked {private}");
        }
        assert_eq!(
            db.mail_queue().message(held.message_id).await.unwrap().raw,
            RAW
        );
        assert!(reject(&db, &held, !legacy, "duplicate").await.is_err());
        let jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs WHERE queue!='in'")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(jobs, 1);
        let saved: String =
            sqlx::query_scalar("SELECT reason FROM moderation_log WHERE held_id=$1")
                .bind(held.id.0.to_string())
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(saved, reason);
    }
}
