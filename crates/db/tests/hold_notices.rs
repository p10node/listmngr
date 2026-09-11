//! Holding a post notifies the poster (`list:user:notice:hold`) and the
//! owners/moderators (`list:admin:action:post`) from templates, gated by
//! `respond_to_post_requests` and `admin_immed_notify`, in the same
//! transaction as the hold itself.
use listmngr_core::{ListId, MemberRole, SubscriptionMode};
use listmngr_db::mail_queue::{NewMessage, Queue};
use listmngr_db::templates::Scope;
use listmngr_db::{Database, NewList, NewMember};
use serde_json::json;

const RAW: &[u8] = b"From: author@elsewhere.invalid\r\nTo: dev@example.invalid\r\nSubject: private subject\r\nMessage-ID: <original@example.invalid>\r\n\r\nPRIVATE BODY";

async fn fixture(settings: serde_json::Value) -> (Database, ListId) {
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
            display_name: "Dev Chat".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    db.lists().update(&list, &settings).await.unwrap();
    for (email, role) in [
        ("owner@example.invalid", MemberRole::Owner),
        ("mod@example.invalid", MemberRole::Moderator),
        ("member@example.invalid", MemberRole::Member),
    ] {
        db.members()
            .create(NewMember {
                list_id: list.clone(),
                email: email.into(),
                display_name: String::new(),
                role,
                subscription_mode: SubscriptionMode::AsAddress,
            })
            .await
            .unwrap();
    }
    (db, list)
}

/// Hold one message and return every outgoing notice as (recipient, subject, body).
async fn hold(db: &Database, list: &ListId, sender: &str) -> Vec<(String, String, String)> {
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: RAW.to_vec(),
                external_id: format!("<{}@example.invalid>", uuid::Uuid::now_v7()),
                context: json!({"list_id": list, "envelope_sender": sender}).to_string(),
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
    db.moderation()
        .hold(
            &lease,
            list,
            sender,
            "private subject",
            "Message has no subject; message exceeds list max_message_size",
            101,
        )
        .await
        .unwrap();
    let mut notices = Vec::new();
    while let Some(lease) = db
        .mail_queue()
        .claim(Queue::Out, "out", 200, 100)
        .await
        .unwrap()
    {
        let message = db.mail_queue().message(lease.job.message_id).await.unwrap();
        let recipients = db
            .mail_queue()
            .pending_recipients(lease.job.id)
            .await
            .unwrap();
        let parsed = mail_parser::MessageParser::default()
            .parse(&message.raw)
            .unwrap();
        notices.push((
            recipients.join(","),
            parsed.subject().unwrap_or_default().to_owned(),
            parsed.body_text(0).unwrap_or_default().into_owned(),
        ));
    }
    notices.sort();
    notices
}

#[tokio::test]
async fn a_hold_notifies_the_poster_and_every_owner_and_moderator_from_templates() {
    let (db, list) = fixture(json!({})).await;
    let notices = hold(&db, &list, "author@elsewhere.invalid").await;
    assert_eq!(notices.len(), 3, "{notices:?}");

    let poster = notices
        .iter()
        .find(|(to, _, _)| to == "author@elsewhere.invalid")
        .expect("poster notice");
    assert_eq!(
        poster.1,
        "Your message to dev@example.invalid awaits moderator approval"
    );
    assert!(
        poster
            .2
            .contains("Your mail to 'dev@example.invalid' with the subject")
    );
    assert!(poster.2.contains("    private subject"));
    assert!(
        poster
            .2
            .contains("Message has no subject; message exceeds list max_message_size")
    );

    for admin in ["owner@example.invalid", "mod@example.invalid"] {
        let notice = notices
            .iter()
            .find(|(to, _, _)| to == admin)
            .unwrap_or_else(|| panic!("{admin} notice"));
        assert_eq!(
            notice.1,
            "dev@example.invalid post from author@elsewhere.invalid requires approval"
        );
        assert!(notice.2.contains("    List:    dev@example.invalid"));
        assert!(notice.2.contains("    From:    author@elsewhere.invalid"));
        assert!(notice.2.contains("    Subject: private subject"));
        assert!(notice.2.contains("Message has no subject"));
    }
    let audit: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='moderation.hold'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(audit, 1);
}

#[tokio::test]
async fn the_settings_switch_each_notice_off_independently() {
    let (db, list) = fixture(json!({"respond_to_post_requests": false})).await;
    let notices = hold(&db, &list, "author@elsewhere.invalid").await;
    assert_eq!(notices.len(), 2);
    assert!(
        notices
            .iter()
            .all(|(to, _, _)| to != "author@elsewhere.invalid")
    );

    let (db, list) = fixture(json!({"admin_immed_notify": false})).await;
    let notices = hold(&db, &list, "author@elsewhere.invalid").await;
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].0, "author@elsewhere.invalid");

    let (db, list) =
        fixture(json!({"admin_immed_notify": false, "respond_to_post_requests": false})).await;
    assert!(
        hold(&db, &list, "author@elsewhere.invalid")
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn a_poster_notice_is_never_sent_to_an_unsafe_or_list_owned_address() {
    for sender in [
        "",
        "<>",
        "dev@example.invalid",
        "dev-owner@example.invalid",
        "dev-bounces@example.invalid",
        "a..b@example.invalid",
        "a@example.invalid\r\nBcc: x@example.invalid",
    ] {
        let (db, list) = fixture(json!({"admin_immed_notify": false})).await;
        let notices = hold(&db, &list, sender).await;
        assert!(notices.is_empty(), "{sender:?} got {notices:?}");
        // The hold itself still happened.
        let held: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM held_messages")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(held, 1, "{sender:?}");
    }
}

#[tokio::test]
async fn hold_notices_use_list_scoped_template_overrides() {
    let (db, list) = fixture(json!({"admin_immed_notify": false})).await;
    db.templates()
        .set_body(
            &Scope::List(list.clone()),
            "list:user:notice:hold",
            "en",
            "Bài của bạn tới $display_name ($listname) đang chờ duyệt:\n$reasons\n",
        )
        .await
        .unwrap();
    let notices = hold(&db, &list, "author@elsewhere.invalid").await;
    assert_eq!(notices.len(), 1);
    assert_eq!(
        notices[0].2,
        "Bài của bạn tới Dev Chat (dev@example.invalid) đang chờ duyệt:\r\nMessage has no subject; message exceeds list max_message_size\r\n"
    );
}
