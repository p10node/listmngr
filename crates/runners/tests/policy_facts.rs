use listmngr_core::{
    Config, DeliveryMode, DeliveryStatus, ListId, MemberRole, ModerationAction, Preferences,
    SubscriptionMode,
};
use listmngr_db::{Database, NewList, NewMember};
use listmngr_runners::resolve_recipients;

async fn seeded(db: &Database) -> ListId {
    db.domains()
        .create("dev.example.invalid", "dev", None)
        .await
        .unwrap();
    let list_id: ListId = "dev.dev.example.invalid".parse().unwrap();
    db.lists()
        .create(NewList {
            list_id: list_id.clone(),
            display_name: "Dev".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    list_id
}

async fn add_member(db: &Database, list_id: &ListId, email: &str) -> listmngr_core::Member {
    db.members()
        .create(NewMember {
            list_id: list_id.clone(),
            email: email.into(),
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsAddress,
            display_name: String::new(),
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn resolve_recipients_excludes_disabled_and_digest_members_and_respects_own_postings() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let list_id = seeded(&db).await;

    let regular = add_member(&db, &list_id, "regular@example.invalid").await;
    let disabled = add_member(&db, &list_id, "disabled@example.invalid").await;
    let digest = add_member(&db, &list_id, "digest@example.invalid").await;
    let sender = add_member(&db, &list_id, "sender@example.invalid").await;

    db.preferences()
        .set_member(
            disabled.id,
            Preferences {
                delivery_status: Some(DeliveryStatus::ByBounces),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    db.preferences()
        .set_member(
            digest.id,
            Preferences {
                delivery_mode: Some(DeliveryMode::MimeDigests),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    db.preferences()
        .set_member(
            sender.id,
            Preferences {
                receive_own_postings: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let _ = regular;

    let mut recipients = resolve_recipients(&db, &list_id, "sender@example.invalid")
        .await
        .unwrap();
    recipients.sort();
    assert_eq!(recipients, vec!["regular@example.invalid".to_owned()]);
}

#[tokio::test]
async fn resolve_recipients_includes_sender_who_wants_their_own_copy() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let list_id = seeded(&db).await;
    add_member(&db, &list_id, "sender@example.invalid").await;
    let recipients = resolve_recipients(&db, &list_id, "sender@example.invalid")
        .await
        .unwrap();
    assert_eq!(recipients, vec!["sender@example.invalid".to_owned()]);
}

#[tokio::test]
async fn config_default_policy_is_defer_member_hold_nonmember() {
    let config = Config::default();
    assert_eq!(
        config.mailman.default_member_action,
        ModerationAction::Defer
    );
    assert_eq!(
        config.mailman.default_nonmember_action,
        ModerationAction::Hold
    );
}
