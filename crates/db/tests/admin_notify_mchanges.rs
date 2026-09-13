//! Mailman's `admin_notify_mchanges`: owners and moderators learn of a
//! member's subscription (`list:admin:notice:subscribe`) and removal
//! (`list:admin:notice:unsubscribe`), in the membership's own transaction.
use listmngr_core::{ListId, MemberId, MemberRole, SubscriptionMode};
use listmngr_db::mail_queue::Queue;
use listmngr_db::{Database, NewList, NewMember};
use serde_json::json;

async fn fixture(settings: serde_json::Value) -> (Database, ListId) {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "example", None)
        .await
        .unwrap();
    let list: ListId = "dev.example.invalid".parse().unwrap();
    let created = db
        .lists()
        .create(NewList {
            list_id: list.clone(),
            display_name: "Dev Chat".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    assert!(!created.admin_notify_mchanges, "Mailman's default is off");
    db.lists().update(&list, &settings).await.unwrap();
    for (email, role) in [
        ("owner@example.invalid", MemberRole::Owner),
        ("mod@example.invalid", MemberRole::Moderator),
    ] {
        add(&db, &list, email, role).await;
    }
    (db, list)
}

async fn add(db: &Database, list: &ListId, email: &str, role: MemberRole) -> MemberId {
    db.members()
        .create(NewMember {
            list_id: list.clone(),
            email: email.into(),
            display_name: String::new(),
            role,
            subscription_mode: SubscriptionMode::AsAddress,
        })
        .await
        .unwrap()
        .id
}

/// Drain the outgoing queue as (recipient, subject, body) triples.
async fn notices(db: &Database) -> Vec<(String, String, String)> {
    // Membership notices are stamped with the wall clock.
    let now_ms = chrono::Utc::now().timestamp_millis() + 1_000;
    let mut notices = Vec::new();
    while let Some(lease) = db
        .mail_queue()
        .claim(Queue::Out, "out", now_ms, 100)
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
async fn off_by_default_nothing_reaches_the_owners() {
    let (db, list) = fixture(json!({"send_welcome_message": false})).await;
    assert!(
        notices(&db).await.is_empty(),
        "owner/moderator roles never notify"
    );
    let member = add(&db, &list, "member@example.invalid", MemberRole::Member).await;
    db.members().delete(member).await.unwrap();
    assert!(notices(&db).await.is_empty());
}

#[tokio::test]
async fn a_subscription_and_a_removal_notify_every_owner_and_moderator() {
    let (db, list) =
        fixture(json!({"admin_notify_mchanges": true, "send_welcome_message": true})).await;
    let member = add(&db, &list, "Member@Example.invalid", MemberRole::Member).await;
    let subscribed = notices(&db).await;
    // The welcome to the member plus one notice per administrator.
    assert_eq!(subscribed.len(), 3, "{subscribed:?}");
    for admin in ["mod@example.invalid", "owner@example.invalid"] {
        let notice = subscribed
            .iter()
            .find(|(to, _, _)| to == admin)
            .expect("notice to the administrator");
        assert_eq!(notice.1, "Dev Chat subscription notification");
        assert_eq!(
            notice.2,
            "Member@Example.invalid has been successfully subscribed to Dev Chat.\r\n"
        );
    }
    assert!(
        subscribed.iter().any(
            |(to, subject, _)| to == "Member@Example.invalid" && subject.starts_with("Welcome")
        )
    );

    db.members().delete(member).await.unwrap();
    let removed = notices(&db).await;
    assert_eq!(removed.len(), 2, "{removed:?}");
    for (to, subject, body) in &removed {
        assert!(to == "owner@example.invalid" || to == "mod@example.invalid");
        assert_eq!(subject, "Dev Chat unsubscription notification");
        assert_eq!(
            body,
            "Member@Example.invalid has been removed from Dev Chat.\r\n"
        );
    }

    // Adding or removing an owner or moderator is not a membership change.
    let extra = add(
        &db,
        &list,
        "second-mod@example.invalid",
        MemberRole::Moderator,
    )
    .await;
    db.members().delete(extra).await.unwrap();
    assert!(notices(&db).await.is_empty());
}

#[tokio::test]
async fn the_notice_is_independent_of_welcome_and_goodbye_and_speaks_the_owners_language() {
    let (db, list) = fixture(json!({
        "admin_notify_mchanges": true,
        "send_welcome_message": false,
        "send_goodbye_message": false
    }))
    .await;
    let moderator = db
        .members()
        .roster(&list, MemberRole::Moderator)
        .await
        .unwrap()
        .remove(0);
    db.members().delete(moderator.id).await.unwrap();
    let owner = db
        .members()
        .roster(&list, MemberRole::Owner)
        .await
        .unwrap()
        .remove(0);
    db.preferences()
        .set_member(
            owner.id,
            listmngr_core::Preferences {
                preferred_language: Some("vi".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let member = add(&db, &list, "member@example.invalid", MemberRole::Member).await;
    assert_eq!(
        notices(&db).await,
        [(
            "owner@example.invalid".to_owned(),
            "Thông báo đăng ký Dev Chat".to_owned(),
            "member@example.invalid đã đăng ký thành công vào Dev Chat.\r\n".to_owned()
        )]
    );
    db.members().delete(member).await.unwrap();
    assert_eq!(
        notices(&db).await,
        [(
            "owner@example.invalid".to_owned(),
            "Thông báo huỷ đăng ký Dev Chat".to_owned(),
            "member@example.invalid đã bị gỡ khỏi Dev Chat.\r\n".to_owned()
        )]
    );
}
