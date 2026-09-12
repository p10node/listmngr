//! Mailman's topic settings (`topics_enabled`, `topics_bodylines_limit`,
//! `topics`): defaults, a validated round trip with audit, and rejections
//! that leave the row alone.
use listmngr_core::ListId;
use listmngr_db::{Database, NewList};
use serde_json::{Value, json};

async fn fixture() -> (Database, ListId) {
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
            display_name: "Dev".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    (db, list)
}

async fn config(db: &Database, list: &ListId) -> Value {
    serde_json::to_value(db.lists().get(list).await.unwrap()).unwrap()
}

#[tokio::test]
async fn topics_default_off_round_trip_and_reject_bad_patterns_atomically() {
    let (db, list) = fixture().await;
    let initial = config(&db, &list).await;
    assert_eq!(initial["topics_enabled"], false);
    assert_eq!(initial["topics_bodylines_limit"], 5);
    assert_eq!(initial["topics"], json!([]));

    let patch = json!({
        "topics_enabled": true,
        "topics_bodylines_limit": -1,
        "topics": [
            {"name": "Rust", "pattern": "cargo\nborrow checker", "description": "Rust talk"},
            {"name": "Python", "pattern": "\\bpip\\b"}
        ]
    });
    db.lists().update(&list, &patch).await.unwrap();
    let saved = config(&db, &list).await;
    assert_eq!(saved["topics_enabled"], true);
    assert_eq!(saved["topics_bodylines_limit"], -1);
    assert_eq!(saved["topics"][0]["name"], "Rust");
    assert_eq!(saved["topics"][0]["pattern"], "cargo\nborrow checker");
    assert_eq!(saved["topics"][1]["description"], "");
    let audit: String = sqlx::query_scalar(
        "SELECT diff FROM audit_log WHERE action='list.config' ORDER BY at DESC LIMIT 1",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(audit.contains("borrow checker"), "{audit}");

    let before = config(&db, &list).await;
    for (label, bad) in [
        (
            "unbalanced regex",
            json!({"topics": [{"name": "x", "pattern": "("}]}),
        ),
        (
            "empty pattern",
            json!({"topics": [{"name": "x", "pattern": "  \n"}]}),
        ),
        (
            "empty name",
            json!({"topics": [{"name": "", "pattern": "a"}]}),
        ),
        (
            "duplicate names",
            json!({"topics": [{"name": "A", "pattern": "a"}, {"name": "a", "pattern": "b"}]}),
        ),
        (
            "multi-line description",
            json!({"topics": [{"name": "x", "pattern": "a", "description": "a\nb"}]}),
        ),
        ("not a list", json!({"topics": "Rust"})),
        ("missing pattern", json!({"topics": [{"name": "x"}]})),
        ("limit out of range", json!({"topics_bodylines_limit": -2})),
        ("limit too large", json!({"topics_bodylines_limit": 10_001})),
        ("enabled as string", json!({"topics_enabled": "yes"})),
    ] {
        let mut patch = bad.clone();
        patch["display_name"] = json!("must not persist");
        let error = db.lists().update(&list, &patch).await.unwrap_err();
        assert!(
            matches!(error, listmngr_core::Error::Validation(_)),
            "{label}: {error:?}"
        );
        assert_eq!(config(&db, &list).await, before, "{label} leaked a write");
    }
}
