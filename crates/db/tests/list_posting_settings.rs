use listmngr_core::ModerationAction;
use listmngr_db::{Database, NewList};
use serde_json::json;

async fn seed(db: &Database) -> listmngr_core::ListId {
    db.migrate().await.unwrap();
    db.domains()
        .create("policy.invalid", "", None)
        .await
        .unwrap();
    db.lists()
        .create(NewList {
            list_id: "news.policy.invalid".parse().unwrap(),
            display_name: "News".into(),
            style: "legacy-announce".into(),
        })
        .await
        .unwrap()
        .id
}

#[tokio::test]
async fn policy_overrides_persist_clear_and_rollback_with_audit() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    let id = seed(&db).await;
    assert_eq!(
        db.lists().get(&id).await.unwrap().default_member_action,
        Some(ModerationAction::Hold)
    );
    db.lists()
        .update(
            &id,
            &json!({"default_member_action":"accept", "default_nonmember_action":"discard"}),
        )
        .await
        .unwrap();
    let saved = db.lists().get(&id).await.unwrap();
    assert_eq!(saved.default_member_action, Some(ModerationAction::Accept));
    assert_eq!(
        saved.default_nonmember_action,
        Some(ModerationAction::Discard)
    );
    for value in [json!("bad"), json!(42), json!(false)] {
        assert!(
            db.lists()
                .update(&id, &json!({"default_member_action":value}))
                .await
                .is_err()
        );
    }
    db.lists()
        .update(&id, &json!({"default_member_action":null}))
        .await
        .unwrap();
    assert_eq!(
        db.lists().get(&id).await.unwrap().default_member_action,
        None
    );
    sqlx::query("CREATE TRIGGER reject_policy BEFORE INSERT ON audit_log BEGIN SELECT RAISE(ABORT, 'fixture audit failure'); END").execute(db.pool()).await.unwrap();
    assert!(
        db.lists()
            .update(&id, &json!({"default_nonmember_action":"accept"}))
            .await
            .is_err()
    );
    assert_eq!(
        db.lists().get(&id).await.unwrap().default_nonmember_action,
        Some(ModerationAction::Discard)
    );
}

#[tokio::test]
#[ignore = "requires explicit disposable TEST_POSTGRES_URL; uses own schema"]
async fn postgres_waiting_patch_does_not_overwrite_other_committed_fields() {
    sqlx::any::install_default_drivers();
    let url = std::env::var("TEST_POSTGRES_URL").expect("explicit fixture database required");
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("list_patch_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let isolated = format!(
        "{url}{}options=-csearch_path%3D{schema}",
        if url.contains('?') { '&' } else { '?' }
    );
    let db = Database::connect(&isolated, 1).await.unwrap();
    let id = seed(&db).await;
    let writer_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let mut blocker = admin.begin().await.unwrap();
    sqlx::query(&format!(
        "UPDATE {schema}.mailing_lists SET description='committed concurrently' WHERE list_id=$1"
    ))
    .bind(id.as_str())
    .execute(&mut *blocker)
    .await
    .unwrap();
    let worker = {
        let db = db.clone();
        let id = id.clone();
        tokio::spawn(async move {
            db.lists()
                .update(&id, &json!({"default_member_action":"reject"}))
                .await
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT cardinality(pg_blocking_pids($1)) > 0")
                .bind(writer_pid)
                .fetch_one(&admin)
                .await
                .unwrap();
            if waiting {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("patch must really reach the row-lock barrier");
    blocker.commit().await.unwrap();
    worker.await.unwrap().unwrap();
    let saved = db.lists().get(&id).await.unwrap();
    // Tear down before assertions, including on the RED baseline.
    db.pool().close().await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    assert_eq!(saved.description, "committed concurrently");
    assert_eq!(saved.default_member_action, Some(ModerationAction::Reject));
}
