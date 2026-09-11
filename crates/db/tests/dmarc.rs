use listmngr_db::{Database, NewList};
use serde_json::json;
#[tokio::test]
async fn dmarc_settings_audit_failure_rolls_back_and_pairs_are_strict() {
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
            display_name: "Dev".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let id = list.id;
    let enabled =
        json!({"dmarc_mitigate_action":"munge_from","dmarc_mitigate_unconditionally":true});
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(db.pool())
        .await
        .unwrap();
    sqlx::query("CREATE TRIGGER dmarc_audit BEFORE INSERT ON audit_log BEGIN SELECT RAISE(ABORT,'fixture audit failure'); END").execute(db.pool()).await.unwrap();
    assert!(db.lists().update(&id, &enabled).await.is_err());
    let saved = serde_json::to_value(db.lists().get(&id).await.unwrap()).unwrap();
    assert_eq!(saved["dmarc_mitigate_action"], "no_mitigation");
    assert_eq!(saved["dmarc_mitigate_unconditionally"], false);
    sqlx::query("DROP TRIGGER dmarc_audit")
        .execute(db.pool())
        .await
        .unwrap();
    for bad in [
        json!({"dmarc_mitigate_action":"munge_from"}),
        json!({"dmarc_mitigate_action":"wrap_message"}),
        json!({"dmarc_mitigate_action":null}),
        json!({"dmarc_mitigate_unconditionally":"true"}),
        json!({"dmarc_mitigate_unconditionally":1}),
        json!({"dmarc_mitigate_unconditionally":null}),
    ] {
        assert!(db.lists().update(&id, &bad).await.is_err());
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM audit_log")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        before
    );
    // Pre-staging unconditional=true with no mitigation is explicitly supported.
    db.lists()
        .update(&id, &json!({"dmarc_mitigate_unconditionally":true}))
        .await
        .unwrap();
    db.lists()
        .update(&id, &json!({"dmarc_mitigate_action":"munge_from"}))
        .await
        .unwrap();
    assert!(
        db.lists()
            .update(&id, &json!({"dmarc_mitigate_unconditionally":false}))
            .await
            .is_err()
    );
    let saved = serde_json::to_value(db.lists().get(&id).await.unwrap()).unwrap();
    assert_eq!(saved["dmarc_mitigate_action"], "munge_from");
    assert_eq!(saved["dmarc_mitigate_unconditionally"], true);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM audit_log")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        before + 2
    );
    // SQL writes cannot bypass the same supported pair/action constraint.
    assert!(
        sqlx::query("UPDATE mailing_lists SET dmarc_mitigate_unconditionally=0")
            .execute(db.pool())
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE mailing_lists SET dmarc_mitigate_action='reject'")
            .execute(db.pool())
            .await
            .is_err()
    );
    db.lists().update(&id,&json!({"dmarc_mitigate_action":"no_mitigation","dmarc_mitigate_unconditionally":false})).await.unwrap();
}
