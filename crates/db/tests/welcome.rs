use listmngr_db::{Database, NewList};
use serde_json::json;

#[tokio::test]
async fn enabled_subscription_publishes_one_private_welcome() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: "dev.example.invalid".parse().unwrap(),
            display_name: "Do not copy this".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let saved = serde_json::to_value(&list).unwrap();
    assert_eq!(
        saved["send_welcome_message"], false,
        "welcome must default off"
    );
    db.lists()
        .update(&list.id, &json!({"send_welcome_message":true}))
        .await
        .unwrap();
    db.members()
        .mass(
            &list.id,
            "subscribe",
            &["Subscribed@example.invalid".into()],
        )
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workflow_notices")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 1, "one durable welcome for actual new membership");
}
