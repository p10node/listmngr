//! Mailman's Usenet gateway settings on a list: the style defaults, the
//! patches that persist with their audit event, the rejections that leave
//! the row alone, and the watermark that only the gateway writes.
use listmngr_core::ListId;
use listmngr_db::{Database, NewList};
use serde_json::{Value, json};

async fn fixture(db: &Database) -> ListId {
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
    list
}

async fn config(db: &Database, list: &ListId) -> Value {
    serde_json::to_value(db.lists().get(list).await.unwrap()).unwrap()
}

async fn audit_count(db: &Database) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='list.config'")
        .fetch_one(db.pool())
        .await
        .unwrap()
}

/// Mailman's `BasicOperation` style: no gateway either way, no newsgroup,
/// the prefix kept on gated posts, an unmoderated group, never polled.
fn mailman_defaults() -> Value {
    json!({
        "gateway_to_mail": false,
        "gateway_to_news": false,
        "linked_newsgroup": "",
        "nntp_prefix_subject_too": true,
        "newsgroup_moderation": "none",
        "usenet_watermark": null,
    })
}

async fn scenario(db: &Database) {
    let list = fixture(db).await;
    let initial = config(db, &list).await;
    for (key, expected) in mailman_defaults().as_object().unwrap() {
        assert_eq!(&initial[key], expected, "default {key}");
    }
    a_full_patch_persists_with_its_audit(db, &list).await;
    // Every moderation value; clearing the newsgroup.
    for value in ["moderated", "none"] {
        db.lists()
            .update(&list, &json!({"newsgroup_moderation": value}))
            .await
            .unwrap();
        assert_eq!(config(db, &list).await["newsgroup_moderation"], value);
    }
    db.lists()
        .update(&list, &json!({"linked_newsgroup": ""}))
        .await
        .unwrap();
    assert_eq!(config(db, &list).await["linked_newsgroup"], "");
    // The gateway records where it has read to; the setting is read-only.
    db.usenet().set_watermark(&list, 4_242).await.unwrap();
    assert_eq!(config(db, &list).await["usenet_watermark"], 4_242);
    refused_writes_change_nothing(db, &list).await;
    // Newsgroup names as Usenet spells them.
    for name in ["comp.lang.rust", "alt.test", "local.lists.dev-2", "a.b_c+d"] {
        db.lists()
            .update(&list, &json!({"linked_newsgroup": name}))
            .await
            .unwrap();
        assert_eq!(config(db, &list).await["linked_newsgroup"], name);
    }
}

async fn a_full_patch_persists_with_its_audit(db: &Database, list: &ListId) {
    let patch = json!({
        "gateway_to_mail": true,
        "gateway_to_news": true,
        "linked_newsgroup": "comp.lang.rust.lists",
        "nntp_prefix_subject_too": false,
        "newsgroup_moderation": "open_moderated",
    });
    let before = audit_count(db).await;
    db.lists().update(list, &patch).await.unwrap();
    let saved = config(db, list).await;
    for (key, expected) in patch.as_object().unwrap() {
        assert_eq!(&saved[key], expected, "saved {key}");
    }
    assert_eq!(saved["usenet_watermark"], Value::Null);
    assert_eq!(audit_count(db).await, before + 1, "one audited write");
    let audit: String = sqlx::query_scalar(
        "SELECT diff FROM audit_log WHERE action='list.config' ORDER BY at DESC LIMIT 1",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    let audit: Value = serde_json::from_str(&audit).unwrap();
    assert_eq!(audit["linked_newsgroup"], "comp.lang.rust.lists");
    assert_eq!(audit["newsgroup_moderation"], "open_moderated");
}

async fn refused_writes_change_nothing(db: &Database, list: &ListId) {
    let snapshot = config(db, list).await;
    let before = audit_count(db).await;
    for bad in [
        json!({"usenet_watermark": 5}),
        json!({"usenet_watermark": null}),
        json!({"newsgroup_moderation": "closed"}),
        json!({"newsgroup_moderation": 1}),
        json!({"gateway_to_news": "yes"}),
        json!({"linked_newsgroup": "comp lang rust"}),
        json!({"linked_newsgroup": "comp.lang.rust\r\ncontrol"}),
        json!({"linked_newsgroup": ".leading"}),
        json!({"linked_newsgroup": "a".repeat(256)}),
        json!({"linked_newsgroup": null}),
    ] {
        assert!(db.lists().update(list, &bad).await.is_err(), "{bad}");
    }
    assert_eq!(
        config(db, list).await,
        snapshot,
        "refused writes change nothing"
    );
    assert_eq!(audit_count(db).await, before, "and leave no audit event");
}

#[tokio::test]
async fn usenet_settings_round_trip_on_sqlite() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    scenario(&db).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_usenet_settings_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("usenet_settings")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 2).await.unwrap();
    scenario(&db).await;
    db.pool().close().await;
    schema.drop().await.unwrap();
}
