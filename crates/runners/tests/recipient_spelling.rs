use listmngr_core::{DeliveryMode, DeliveryStatus, MemberRole, SubscriptionMode};
use listmngr_db::{Database, NewList, NewMember};
use listmngr_pipeline::policy::{CandidateRecipient, select_recipients};

#[tokio::test]
async fn transport_recipient_preserves_stored_spelling() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains().create("example.com", "", None).await.unwrap();
    let id: listmngr_core::ListId = "test.example.com".parse().unwrap();
    db.lists()
        .create(NewList {
            list_id: id.clone(),
            display_name: "Test".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    db.members()
        .create(NewMember {
            list_id: id.clone(),
            email: "ExactCase@example.com".into(),
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsAddress,
            display_name: String::new(),
        })
        .await
        .unwrap();
    assert_eq!(
        listmngr_runners::resolve_recipients(&db, &id, "other@example.com")
            .await
            .unwrap(),
        ["ExactCase@example.com"]
    );
}

#[test]
fn own_postings_use_normalized_identity_without_rewriting_transport() {
    let candidates = [CandidateRecipient {
        email: "ExactCase@example.com".into(),
        delivery_status: DeliveryStatus::Enabled,
        delivery_mode: DeliveryMode::Regular,
        receive_own_postings: false,
    }];
    assert!(select_recipients(&candidates, "exactcase@example.com").is_empty());
    assert_eq!(
        select_recipients(&candidates, "other@example.com"),
        ["ExactCase@example.com"]
    );
}
