use listmngr_db::{Database, NewList};
use serde_json::json;

#[tokio::test]
async fn bounce_configuration_defaults_and_persistence() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: "score.example.invalid".parse().unwrap(),
            display_name: "Score".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let value = serde_json::to_value(&list).unwrap();
    assert_eq!(value["bounce_notify_owner_on_bounce_increment"], false);
    assert_eq!(value["bounce_notify_owner_on_disable"], true);
    let mut legacy = value.clone();
    legacy
        .as_object_mut()
        .unwrap()
        .remove("bounce_notify_owner_on_bounce_increment");
    legacy
        .as_object_mut()
        .unwrap()
        .remove("bounce_notify_owner_on_disable");
    legacy
        .as_object_mut()
        .unwrap()
        .remove("bounce_score_threshold");
    let restored: listmngr_core::MailingList = serde_json::from_value(legacy).unwrap();
    assert!(restored.bounce_notify_owner_on_disable);
    assert_eq!(
        serde_json::to_value(&restored).unwrap()["bounce_notify_owner_on_bounce_increment"],
        false
    );
    assert!((restored.bounce_score_threshold - 5.0).abs() < f64::EPSILON);
    assert_eq!(value["bounce_score_threshold"], 5.0);
    db.lists()
        .update(&list.id, &json!({"bounce_score_threshold":2.5}))
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(db.lists().get(&list.id).await.unwrap()).unwrap()["bounce_score_threshold"],
        2.5
    );
    for bad in [
        json!(0),
        json!(-1),
        json!(1_000_000.1),
        json!("NaN"),
        json!(null),
        json!(true),
    ] {
        assert!(
            db.lists()
                .update(&list.id, &json!({"bounce_score_threshold":bad}))
                .await
                .is_err()
        );
    }
    assert_eq!(value["process_bounces"], false);
    assert_eq!(value["bounce_info_stale_after"], 7);
    db.lists()
        .update(
            &list.id,
            &json!({"process_bounces":true,"bounce_info_stale_after":30}),
        )
        .await
        .unwrap();
    let value = serde_json::to_value(db.lists().get(&list.id).await.unwrap()).unwrap();
    assert_eq!(value["process_bounces"], true);
    assert_eq!(value["bounce_info_stale_after"], 30);
}
