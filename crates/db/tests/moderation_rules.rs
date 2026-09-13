//! Message Acceptance settings behind the chain rules: `administrivia`,
//! `require_explicit_destination`, `acceptable_aliases`, the four legacy
//! `*_these_nonmembers` lists, the write-only `moderator_password`, and the
//! per-list `header_matches` rows.
use listmngr_db::{Database, HeaderMatchRow, NewList};
use serde_json::json;

async fn fixture() -> (Database, listmngr_core::ListId) {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: "rules.example.invalid".parse().unwrap(),
            display_name: "Rules".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    (db, list.id)
}

async fn audit_count(db: &Database) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn new_lists_carry_mailman_defaults_for_the_acceptance_settings() {
    let (db, id) = fixture().await;
    let value = serde_json::to_value(db.lists().get(&id).await.unwrap()).unwrap();
    assert_eq!(value["administrivia"], true);
    assert_eq!(value["require_explicit_destination"], true);
    assert_eq!(value["acceptable_aliases"], json!([]));
    for key in [
        "accept_these_nonmembers",
        "hold_these_nonmembers",
        "reject_these_nonmembers",
        "discard_these_nonmembers",
    ] {
        assert_eq!(value[key], json!([]), "{key}");
    }
    // The password is write-only: never projected, even when unset.
    assert!(value.get("moderator_password").is_none());
}

#[tokio::test]
async fn acceptance_booleans_and_address_lists_persist_through_config_updates() {
    let (db, id) = fixture().await;
    db.lists()
        .update(
            &id,
            &json!({
                "administrivia": false,
                "require_explicit_destination": false,
                "acceptable_aliases": ["Announce@example.invalid", "^.*@alias\\.invalid$"],
                "accept_these_nonmembers": ["friend@example.invalid"],
                "hold_these_nonmembers": ["^suspicious@"],
                "reject_these_nonmembers": ["spam@example.invalid"],
                "discard_these_nonmembers": ["^.*@junk\\.invalid$"],
            }),
        )
        .await
        .unwrap();
    let saved = serde_json::to_value(db.lists().get(&id).await.unwrap()).unwrap();
    assert_eq!(saved["administrivia"], false);
    assert_eq!(saved["require_explicit_destination"], false);
    assert_eq!(
        saved["acceptable_aliases"],
        json!(["Announce@example.invalid", "^.*@alias\\.invalid$"])
    );
    assert_eq!(
        saved["accept_these_nonmembers"],
        json!(["friend@example.invalid"])
    );
    assert_eq!(saved["hold_these_nonmembers"], json!(["^suspicious@"]));
    assert_eq!(
        saved["reject_these_nonmembers"],
        json!(["spam@example.invalid"])
    );
    assert_eq!(
        saved["discard_these_nonmembers"],
        json!(["^.*@junk\\.invalid$"])
    );

    // Lists can be cleared again.
    db.lists()
        .update(&id, &json!({"acceptable_aliases": []}))
        .await
        .unwrap();
    let saved = serde_json::to_value(db.lists().get(&id).await.unwrap()).unwrap();
    assert_eq!(saved["acceptable_aliases"], json!([]));
}

#[tokio::test]
async fn invalid_address_list_entries_are_rejected_without_an_audit_row() {
    let (db, id) = fixture().await;
    let before = audit_count(&db).await;
    for (key, invalid) in [
        ("acceptable_aliases", json!("not-a-list")),
        ("acceptable_aliases", json!([1])),
        ("acceptable_aliases", json!([""])),
        ("acceptable_aliases", json!(["  "])),
        ("acceptable_aliases", json!(["^("])),
        (
            "acceptable_aliases",
            json!(["has\nnewline@example.invalid"]),
        ),
        ("accept_these_nonmembers", json!(null)),
        ("hold_these_nonmembers", json!({"a": 1})),
        ("administrivia", json!("yes")),
        ("require_explicit_destination", json!(1)),
    ] {
        assert!(
            db.lists()
                .update(&id, &json!({ key: invalid }))
                .await
                .is_err(),
            "{key} accepted {invalid}"
        );
    }
    assert_eq!(audit_count(&db).await, before);
}

#[tokio::test]
async fn moderator_password_is_hashed_verifiable_and_clearable() {
    let (db, id) = fixture().await;
    assert!(
        !db.lists()
            .verify_moderator_password(&id, "anything")
            .await
            .unwrap(),
        "no password set means nothing verifies"
    );
    db.lists()
        .update(
            &id,
            &json!({"moderator_password": "correct horse battery staple"}),
        )
        .await
        .unwrap();
    let stored: String =
        sqlx::query_scalar("SELECT moderator_password FROM mailing_lists WHERE list_id=$1")
            .bind(id.as_str())
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(
        stored.starts_with("$argon2id$"),
        "stored as Argon2id, got {stored}"
    );
    assert!(
        db.lists()
            .verify_moderator_password(&id, "correct horse battery staple")
            .await
            .unwrap()
    );
    assert!(
        !db.lists()
            .verify_moderator_password(&id, "Correct horse battery staple")
            .await
            .unwrap()
    );
    // The audit row must not carry the plaintext or the hash.
    let audit: String = sqlx::query_scalar(
        "SELECT diff FROM audit_log WHERE action='list.config' ORDER BY rowid DESC LIMIT 1",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(
        !audit.contains("correct horse"),
        "plaintext leaked into audit"
    );
    assert!(!audit.contains("$argon2"), "hash leaked into audit");
    assert!(
        audit.contains("moderator_password"),
        "change not attributed"
    );

    // Clearing with an empty string removes the password.
    db.lists()
        .update(&id, &json!({"moderator_password": ""}))
        .await
        .unwrap();
    assert!(
        !db.lists()
            .verify_moderator_password(&id, "correct horse battery staple")
            .await
            .unwrap()
    );
    let stored: Option<String> =
        sqlx::query_scalar("SELECT moderator_password FROM mailing_lists WHERE list_id=$1")
            .bind(id.as_str())
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(stored, None);
}

#[tokio::test]
async fn moderator_password_rejects_non_strings_and_oversized_values() {
    let (db, id) = fixture().await;
    let before = audit_count(&db).await;
    for invalid in [json!(1), json!(true), json!(null), json!("x".repeat(1025))] {
        assert!(
            db.lists()
                .update(&id, &json!({"moderator_password": invalid}))
                .await
                .is_err()
        );
    }
    assert_eq!(audit_count(&db).await, before);
}

#[tokio::test]
async fn header_matches_replace_atomically_in_position_order_with_audit() {
    let (db, id) = fixture().await;
    assert!(db.header_matches().list(&id).await.unwrap().is_empty());
    let before = audit_count(&db).await;
    db.header_matches()
        .replace(
            &id,
            &[
                HeaderMatchRow {
                    header: "X-Spam-Flag".into(),
                    pattern: "^yes$".into(),
                    chain: None,
                    tag: None,
                },
                HeaderMatchRow {
                    header: "Subject".into(),
                    pattern: "viagra".into(),
                    chain: Some("discard".into()),
                    tag: Some("spam".into()),
                },
            ],
        )
        .await
        .unwrap();
    let rows = db.header_matches().list(&id).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].header, "X-Spam-Flag");
    assert_eq!(rows[0].chain, None);
    assert_eq!(rows[1].chain.as_deref(), Some("discard"));
    assert_eq!(rows[1].tag.as_deref(), Some("spam"));
    assert_eq!(audit_count(&db).await, before + 1);

    // A replacement with a bad row leaves the previous rows untouched.
    let result = db
        .header_matches()
        .replace(
            &id,
            &[HeaderMatchRow {
                header: "Subject".into(),
                pattern: "^(".into(),
                chain: None,
                tag: None,
            }],
        )
        .await;
    assert!(result.is_err(), "invalid regex must be rejected");
    assert_eq!(db.header_matches().list(&id).await.unwrap().len(), 2);
    assert_eq!(audit_count(&db).await, before + 1);

    // Empty header names and unknown chains are rejected too.
    for row in [
        HeaderMatchRow {
            header: " ".into(),
            pattern: "x".into(),
            chain: None,
            tag: None,
        },
        HeaderMatchRow {
            header: "X".into(),
            pattern: "x".into(),
            chain: Some("no-such-chain".into()),
            tag: None,
        },
    ] {
        assert!(db.header_matches().replace(&id, &[row]).await.is_err());
    }

    db.header_matches().replace(&id, &[]).await.unwrap();
    assert!(db.header_matches().list(&id).await.unwrap().is_empty());
}

/// The same acceptance-settings contract on a live PostgreSQL schema: JSON
/// array columns, the Argon2 password column and header-rule replacement must
/// behave identically on both engines.
#[tokio::test]
#[ignore = "requires explicit disposable TEST_POSTGRES_URL; uses own schema"]
async fn postgres_moderation_rules_contract() {
    sqlx::any::install_default_drivers();
    let url = std::env::var("TEST_POSTGRES_URL").expect("explicit fixture database required");
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("moderation_rules_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let isolated = format!(
        "{url}{}options=-csearch_path%3D{schema}",
        if url.contains('?') { '&' } else { '?' }
    );
    let result = tokio::spawn(async move {
        let db = Database::connect(&isolated, 2).await.unwrap();
        moderation_rules_scenario(&db).await;
    })
    .await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
    result.unwrap();
}

/// Every acceptance-settings behavior the `SQLite` tests above cover, run once
/// against an isolated `PostgreSQL` schema.
async fn moderation_rules_scenario(db: &Database) {
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    let id = db
        .lists()
        .create(NewList {
            list_id: "rules.example.invalid".parse().unwrap(),
            display_name: "Rules".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap()
        .id;

    let defaults = serde_json::to_value(db.lists().get(&id).await.unwrap()).unwrap();
    assert_eq!(defaults["administrivia"], true);
    assert_eq!(defaults["require_explicit_destination"], true);
    assert_eq!(defaults["acceptable_aliases"], json!([]));

    db.lists()
        .update(
            &id,
            &json!({
                "administrivia": false,
                "acceptable_aliases": ["^.*@alias\\.invalid$"],
                "hold_these_nonmembers": ["held@example.invalid"],
                "moderator_password": "pg posting key"
            }),
        )
        .await
        .unwrap();
    let saved = serde_json::to_value(db.lists().get(&id).await.unwrap()).unwrap();
    assert_eq!(saved["administrivia"], false);
    assert_eq!(saved["acceptable_aliases"], json!(["^.*@alias\\.invalid$"]));
    assert_eq!(
        saved["hold_these_nonmembers"],
        json!(["held@example.invalid"])
    );
    assert!(saved.get("moderator_password").is_none());
    assert!(
        db.lists()
            .verify_moderator_password(&id, "pg posting key")
            .await
            .unwrap()
    );
    assert!(
        !db.lists()
            .verify_moderator_password(&id, "wrong")
            .await
            .unwrap()
    );
    assert!(
        db.lists()
            .update(&id, &json!({"acceptable_aliases": ["^("]}))
            .await
            .is_err()
    );

    postgres_header_rules_scenario(db, &id).await;
}

/// Header-rule replacement on `PostgreSQL`: order, chain, atomic rejection.
async fn postgres_header_rules_scenario(db: &Database, id: &listmngr_core::ListId) {
    db.header_matches()
        .replace(
            id,
            &[
                HeaderMatchRow {
                    header: "X-Spam-Flag".into(),
                    pattern: "^yes$".into(),
                    chain: None,
                    tag: None,
                },
                HeaderMatchRow {
                    header: "Subject".into(),
                    pattern: "lottery".into(),
                    chain: Some("discard".into()),
                    tag: Some("spam".into()),
                },
            ],
        )
        .await
        .unwrap();
    let rows = db.header_matches().list(id).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].chain.as_deref(), Some("discard"));
    assert!(
        db.header_matches()
            .replace(
                id,
                &[HeaderMatchRow {
                    header: "Subject".into(),
                    pattern: "^(".into(),
                    chain: None,
                    tag: None,
                }],
            )
            .await
            .is_err()
    );
    assert_eq!(db.header_matches().list(id).await.unwrap().len(), 2);
}
