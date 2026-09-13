use listmngr_db::{Database, NewList};
use serde_json::json;

#[tokio::test]
async fn message_limit_persists_validates_and_rolls_back_with_audit() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: "size.example.invalid".parse().unwrap(),
            display_name: "Size".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let id = list.id;
    assert_eq!(list.max_message_size, 0);
    for limit in [1, 2048, 2_147_483_647, 0] {
        db.lists()
            .update(&id, &json!({"max_message_size":limit}))
            .await
            .unwrap();
        let saved = serde_json::to_value(db.lists().get(&id).await.unwrap()).unwrap();
        assert_eq!(saved["max_message_size"], limit);
    }
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(db.pool())
        .await
        .unwrap();
    for invalid in [
        json!(-1),
        json!(1.5),
        json!(true),
        json!(null),
        json!("1"),
        json!(u64::MAX),
        json!(2_147_483_648_u64),
    ] {
        assert!(
            db.lists()
                .update(&id, &json!({"max_message_size":invalid}))
                .await
                .is_err()
        );
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM audit_log")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        before
    );
    sqlx::query("CREATE TRIGGER size_audit BEFORE INSERT ON audit_log BEGIN SELECT RAISE(ABORT,'fixture audit failure'); END").execute(db.pool()).await.unwrap();
    assert!(
        db.lists()
            .update(&id, &json!({"max_message_size":2}))
            .await
            .is_err()
    );
    let saved = serde_json::to_value(db.lists().get(&id).await.unwrap()).unwrap();
    assert_eq!(saved["max_message_size"], 0);
    sqlx::query("DROP TRIGGER size_audit")
        .execute(db.pool())
        .await
        .unwrap();
    db.lists()
        .update(&id, &json!({"max_message_size":2}))
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM audit_log")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        before + 1
    );
}
