//! Mailman's `replybot` in the `in` runner: the owner, request and posting
//! addresses answer on their own, and `respond_and_discard` swallows the
//! original before it is forwarded, run or delivered.
use listmngr_core::{Config, MemberRole, SubscriptionMode};
use listmngr_db::{Database, NewList, NewMember};
use listmngr_mail::lmtp::LmtpHandler;
use listmngr_runners::InboundHandler;
use serde_json::json;
use std::time::Duration;

const LIST: &str = "test.example.com";

async fn fixture(settings: serde_json::Value) -> (Database, InboundHandler) {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains().create("example.com", "", None).await.unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: LIST.parse().unwrap(),
            display_name: "Test".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let mut settings = settings;
    settings["respond_to_post_requests"] = json!(false);
    settings["admin_immed_notify"] = json!(false);
    settings["default_nonmember_action"] = json!("accept");
    db.lists().update(&list.id, &settings).await.unwrap();
    for (email, role) in [
        ("owner@example.com", MemberRole::Owner),
        ("reader@example.com", MemberRole::Member),
    ] {
        db.members()
            .create(NewMember {
                list_id: list.id.clone(),
                email: email.into(),
                display_name: String::new(),
                role,
                subscription_mode: SubscriptionMode::AsAddress,
            })
            .await
            .unwrap();
    }
    let handler = InboundHandler {
        db: db.clone(),
        local_hostname: "localhost".into(),
        max_message_bytes: 65_536,
        max_recipients: 10,
        command_timeout: Duration::from_secs(5),
        in_max_attempts: 3,
        verp_delimiter: "+".into(),
        structure: listmngr_mail::structure::Limits::default(),
    };
    (db, handler)
}

async fn run_in(db: &Database) {
    let config = Config::default();
    let role = listmngr_runners::MailRoleConfig::from_core(&config).unwrap();
    let (shutdown, rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(listmngr_runners::run_in_processor(
        db.clone(),
        config,
        role,
        "autoresponder-test".into(),
        rx,
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let n: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM queue_jobs WHERE queue='in' AND state!='done'",
            )
            .fetch_one(db.pool())
            .await
            .unwrap();
            if n == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the in runner finished every job");
    shutdown.send(true).unwrap();
    task.await.unwrap();
}

async fn deliver(handler: &mut InboundHandler, recipient: &str, subject: &str) {
    let raw = format!(
        "From: Writer <writer@example.net>\r\nTo: {recipient}\r\nMessage-ID: <{}@example.net>\r\nSubject: {subject}\r\n\r\nhello\r\n",
        uuid::Uuid::now_v7()
    );
    let outcome = handler
        .deliver(
            Some("writer@example.net"),
            &[recipient.into()],
            raw.as_bytes(),
        )
        .await;
    assert_eq!(outcome[0].code, 250, "{}", outcome[0].detail);
}

/// `(to, subject, Auto-Submitted)` of every queued outgoing message.
async fn outgoing(db: &Database) -> Vec<(String, String, String)> {
    let raws: Vec<Vec<u8>> = sqlx::query_scalar(
        "SELECT b.raw FROM queue_jobs q JOIN messages m ON m.id=q.message_id JOIN message_blobs b ON b.store_key=m.store_key WHERE q.queue='out' ORDER BY q.id",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    raws.iter()
        .map(|raw| {
            let header = |name: &str| listmngr_mail::header_value(raw, name).unwrap_or_default();
            (header("to"), header("subject"), header("auto-submitted"))
        })
        .collect()
}

#[tokio::test]
async fn the_owner_address_answers_and_still_forwards_unless_told_to_discard() {
    let (db, mut handler) = fixture(json!({
        "autorespond_owner": "respond_and_continue",
        "autoresponse_owner_text": "Owners of $listname reply within a week.",
    }))
    .await;
    deliver(&mut handler, "test-owner@example.com", "A question").await;
    run_in(&db).await;
    let sent = outgoing(&db).await;
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert!(
        sent.iter()
            .any(|(to, subject, auto)| to == "writer@example.net"
                && subject.contains("Auto-response")
                && auto == "auto-replied"),
        "the writer gets the automatic reply: {sent:?}"
    );
    assert!(
        sent.iter()
            .any(|(_, subject, _)| subject.contains("A question")),
        "the question is still forwarded to the owners: {sent:?}"
    );

    // The same writer inside the grace period is not answered again, but
    // is still forwarded.
    deliver(&mut handler, "test-owner@example.com", "Another question").await;
    run_in(&db).await;
    let sent = outgoing(&db).await;
    assert_eq!(sent.len(), 3, "{sent:?}");

    let (db, mut handler) = fixture(json!({"autorespond_owner": "respond_and_discard"})).await;
    deliver(&mut handler, "test-owner@example.com", "Discarded question").await;
    run_in(&db).await;
    let sent = outgoing(&db).await;
    assert_eq!(sent.len(), 1, "only the reply: {sent:?}");
    assert_eq!(sent[0].0, "writer@example.net");
    let audit: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM audit_log WHERE action IN ('list.autoresponse','owner.forward','post.discard') ORDER BY at",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(
        audit,
        ["list.autoresponse", "post.discard"],
        "answered, then discarded; never forwarded"
    );
}

#[tokio::test]
async fn the_request_address_answers_and_discarding_skips_the_command() {
    let (db, mut handler) = fixture(json!({
        "autorespond_requests": "respond_and_continue",
    }))
    .await;
    deliver(&mut handler, "test-request@example.com", "help").await;
    run_in(&db).await;
    let sent = outgoing(&db).await;
    assert_eq!(sent.len(), 2, "the auto-response and the help: {sent:?}");

    let (db, mut handler) = fixture(json!({"autorespond_requests": "respond_and_discard"})).await;
    deliver(&mut handler, "test-join@example.com", "join").await;
    run_in(&db).await;
    let sent = outgoing(&db).await;
    assert_eq!(sent.len(), 1, "the join is swallowed: {sent:?}");
    let workflows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM subscription_workflows")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(workflows, 0, "no confirmation was issued");
}

#[tokio::test]
async fn the_posting_address_answers_and_discarding_never_delivers() {
    let (db, mut handler) = fixture(json!({
        "autorespond_postings": "respond_and_continue",
        "autoresponse_postings_text": "Got it.",
    }))
    .await;
    deliver(&mut handler, "test@example.com", "A post").await;
    run_in(&db).await;
    let sent = outgoing(&db).await;
    assert_eq!(sent.len(), 2, "the reply and the delivery: {sent:?}");
    assert!(sent.iter().any(|(to, ..)| to == "writer@example.net"));

    let (db, mut handler) = fixture(json!({"autorespond_postings": "respond_and_discard"})).await;
    deliver(&mut handler, "test@example.com", "A swallowed post").await;
    run_in(&db).await;
    let sent = outgoing(&db).await;
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0].0, "writer@example.net");
    let audit: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='post.discard'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(audit, 1, "the post is discarded with an audit event");
    let archived: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM archive_messages")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(archived, 0);
}
