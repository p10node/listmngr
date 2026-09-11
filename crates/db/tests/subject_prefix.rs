use listmngr_core::{Error, ListId};
use listmngr_db::{Database, NewList};
use serde_json::json;

async fn audit_count(db: &Database) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(db.pool())
        .await
        .unwrap()
}

async fn contract(db: &Database) {
    let id = seed(db).await;
    for prefix in ["[news] ", "[Tiếng Việt] ", "[📬] ", "  [space]\t", ""] {
        let saved = db
            .lists()
            .update(&id, &json!({"subject_prefix":prefix}))
            .await
            .unwrap();
        assert_eq!(saved.subject_prefix, prefix);
        let cooked = listmngr_mail::cook_headers(
            b"Subject: hello\r\n\r\nbody",
            Some(&saved.subject_prefix),
            &[],
        )
        .unwrap();
        if prefix.is_ascii() {
            assert_eq!(
                cooked,
                format!("Subject: {prefix}hello\r\n\r\nbody").as_bytes()
            );
        } else {
            // Storage remains verbatim; the consumer now emits RFC2047 rather
            // than raw UTF-8. Check decoded semantics and the wire separately.
            assert!(cooked.is_ascii());
            assert!(cooked.ends_with(b"\r\n\r\nbody"));
            let parsed = mail_parser::MessageParser::default()
                .parse(&cooked)
                .unwrap();
            let expected = format!("{prefix}hello");
            assert_eq!(parsed.subject(), Some(expected.as_str()));
        }
        let count = audit_count(db).await;
        let snapshot = serde_json::to_value(db.lists().get(&id).await.unwrap()).unwrap();
        for bad in [
            json!("bad\rvalue"),
            json!("bad\nvalue"),
            json!("bad\r\n value"),
            json!("\r\n\r\nbody"),
            json!(null),
            json!(42),
        ] {
            let error = db.lists().update(&id, &json!({"display_name":"must roll back", "subject_prefix":bad, "emergency":true})).await.unwrap_err();
            assert!(matches!(error, Error::Validation(_)));
            assert_eq!(
                serde_json::to_value(db.lists().get(&id).await.unwrap()).unwrap(),
                snapshot
            );
            assert_eq!(audit_count(db).await, count);
        }
    }
    // Header restrictions must not leak into legitimate multiline prose fields.
    let updated = db
        .lists()
        .update(
            &id,
            &json!({"description":"line1\r\nline2", "info":"paragraph1\nparagraph2"}),
        )
        .await
        .unwrap();
    assert_eq!(updated.description, "line1\r\nline2");
    assert_eq!(updated.info, "paragraph1\nparagraph2");
    assert_eq!(updated.subject_prefix, "");
}

#[tokio::test]
async fn prefix_validation_preserves_valid_values_and_rejects_whole_invalid_patch() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    contract(&db).await;
}

#[tokio::test]
async fn prefix_change_rolls_back_on_audit_failure() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    let id = seed(&db).await;
    let before = db.lists().get(&id).await.unwrap().subject_prefix;
    let count = audit_count(&db).await;
    sqlx::query("CREATE TRIGGER reject_prefix_audit BEFORE INSERT ON audit_log BEGIN SELECT RAISE(ABORT, 'fixture audit failure'); END").execute(db.pool()).await.unwrap();
    assert!(
        db.lists()
            .update(&id, &json!({"subject_prefix":"[changed] "}))
            .await
            .is_err()
    );
    assert_eq!(db.lists().get(&id).await.unwrap().subject_prefix, before);
    assert_eq!(audit_count(&db).await, count);
    sqlx::query("DROP TRIGGER reject_prefix_audit")
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(
        db.lists()
            .update(&id, &json!({"subject_prefix":"[changed] "}))
            .await
            .unwrap()
            .subject_prefix,
        "[changed] "
    );
    assert_eq!(audit_count(&db).await, count + 1);
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns isolated schema"]
async fn postgres_subject_prefix_contract() {
    let url = std::env::var("TEST_POSTGRES_URL").unwrap();
    let admin = Database::connect(&url, 1).await.unwrap();
    let schema = format!("prefix_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(admin.pool())
        .await
        .unwrap();
    let sep = if url.contains('?') { '&' } else { '?' };
    let fixture = format!("{url}{sep}options=-csearch_path%3D{schema}");
    let result = tokio::spawn(async move {
        let db = Database::connect(&fixture, 3).await.unwrap();
        contract(&db).await;
        db.pool().close().await;
    })
    .await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(admin.pool())
        .await
        .unwrap();
    admin.pool().close().await;
    result.unwrap();
}

async fn seed(db: &Database) -> ListId {
    db.migrate().await.unwrap();
    db.domains()
        .create("prefix.invalid", "", None)
        .await
        .unwrap();
    db.lists()
        .create(NewList {
            list_id: "news.prefix.invalid".parse().unwrap(),
            display_name: "News".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap()
        .id
}

#[tokio::test]
async fn unsafe_subject_prefix_is_rejected_before_persistence() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    let id = seed(&db).await;
    let unsafe_prefix = "[news]\r\nBcc: victim@fixture.invalid";
    assert!(
        listmngr_mail::cook_headers(b"Subject: hello\r\n\r\nbody", Some(unsafe_prefix), &[])
            .is_err()
    );
    let result = db
        .lists()
        .update(&id, &json!({"subject_prefix":unsafe_prefix}))
        .await;
    assert!(
        matches!(result, Err(Error::Validation(_))),
        "configuration must reject a prefix its mail consumer rejects"
    );
}
