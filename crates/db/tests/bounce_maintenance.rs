use listmngr_db::{Database, NewList};
use serde_json::json;
#[path = "bounce_maintenance/cases.rs"]
mod cases;
#[path = "bounce_maintenance/concurrent.rs"]
mod concurrent;
#[path = "bounce_maintenance/postgres.rs"]
mod postgres;

async fn fixture() -> (Database, listmngr_core::ListId) {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    let id = "maintenance.example.invalid".parse().unwrap();
    db.lists()
        .create(NewList {
            list_id: id,
            display_name: "Maintenance".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    (db, "maintenance.example.invalid".parse().unwrap())
}

async fn disabled(db: &Database, id: &listmngr_core::ListId, email: &str) -> listmngr_core::Member {
    let member = db
        .members()
        .create(listmngr_db::NewMember {
            list_id: id.clone(),
            email: email.into(),
            role: listmngr_core::MemberRole::Member,
            subscription_mode: listmngr_core::SubscriptionMode::AsAddress,
            display_name: String::new(),
        })
        .await
        .unwrap();
    sqlx::query("UPDATE preferences SET delivery_status='by_bounces' WHERE id=$1")
        .bind(member.preferences_id.0.to_string())
        .execute(db.pool())
        .await
        .unwrap();
    db.lists()
        .update(id, &json!({"process_bounces":true}))
        .await
        .unwrap();
    member
}

#[tokio::test]
async fn warning_publication_is_private_atomic_and_immediate() {
    let (db, id) = fixture().await;
    let member = disabled(&db, &id, "Subscriber@Example.invalid").await;
    let now = chrono::DateTime::parse_from_rfc3339("2026-09-09T12:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let summary = db
        .bounce_maintenance()
        .sweep_at(100, None, now)
        .await
        .unwrap();
    assert_eq!(summary.warned, 1);
    let (count, last): (i64, String) =
        sqlx::query_as("SELECT total_warnings_sent,last_warning_sent FROM members WHERE id=$1")
            .bind(member.id.0.to_string())
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(count, 1);
    assert_eq!(chrono::DateTime::parse_from_rfc3339(&last).unwrap(), now);
    let recipients: Vec<String> = sqlx::query_scalar("SELECT email FROM delivery_recipients")
        .fetch_all(db.pool())
        .await
        .unwrap();
    assert_eq!(recipients, ["Subscriber@Example.invalid"]);
    let raw: Vec<u8> = sqlx::query_scalar("SELECT raw FROM message_blobs")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(raw.len() <= 4096);
    let raw = String::from_utf8(raw).unwrap();
    assert!(raw.contains("Subject: Your subscription for maintenance@example.invalid mailing list has been disabled\r\n"));
    assert!(raw.contains("Reply-To: maintenance-owner@example.invalid\r\n"));
    assert!(raw.contains("has been disabled"));
    assert!(!raw.contains("http"));
    let notices: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workflow_notices")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(notices, 1);
    let audits: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='bounce.warning'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(audits, 1);
    assert_eq!(
        db.bounce_maintenance()
            .sweep_at(100, None, now)
            .await
            .unwrap()
            .warned,
        0
    );
}

#[tokio::test]
async fn due_removal_waits_final_interval_and_preserves_identity() {
    let (db, id) = fixture().await;
    let member = disabled(&db, &id, "Subscriber@Example.invalid").await;
    let owner = db
        .members()
        .create(listmngr_db::NewMember {
            list_id: id.clone(),
            email: "Admin@Example.invalid".into(),
            role: listmngr_core::MemberRole::Owner,
            subscription_mode: listmngr_core::SubscriptionMode::AsAddress,
            display_name: String::new(),
        })
        .await
        .unwrap();
    db.lists()
        .update(
            &id,
            &json!({"bounce_you_are_disabled_warnings":1,"send_goodbye_message":true}),
        )
        .await
        .unwrap();
    let now = chrono::DateTime::parse_from_rfc3339("2026-09-09T12:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert_eq!(
        db.bounce_maintenance()
            .sweep_at(100, None, now)
            .await
            .unwrap()
            .warned,
        1
    );
    let due = now + chrono::Duration::days(7);
    assert_eq!(
        db.bounce_maintenance()
            .sweep_at(100, None, due - chrono::Duration::nanoseconds(1))
            .await
            .unwrap()
            .removed,
        0
    );
    assert!(db.members().get(member.id).await.is_ok());
    let summary = db
        .bounce_maintenance()
        .sweep_at(100, None, due)
        .await
        .unwrap();
    assert_eq!(summary.removed, 1);
    assert_eq!(summary.warned, 0);
    assert!(db.members().get(member.id).await.is_err());
    assert!(db.members().get(owner.id).await.is_ok());
    assert!(db.addresses().get_by_id(member.address_id).await.is_ok());
    let recipients: Vec<String> =
        sqlx::query_scalar("SELECT email FROM delivery_recipients ORDER BY email")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(
        recipients,
        [
            "Admin@Example.invalid",
            "Subscriber@Example.invalid",
            "Subscriber@Example.invalid"
        ]
    );
    let raws: Vec<Vec<u8>> = sqlx::query_scalar("SELECT raw FROM message_blobs")
        .fetch_all(db.pool())
        .await
        .unwrap();
    assert!(raws.iter().any(|raw| {
        String::from_utf8_lossy(raw).contains(
            "unsubscribed from maintenance@example.invalid mailing list due to bounces\r\n",
        )
    }));
    assert_eq!(
        db.bounce_maintenance()
            .sweep_at(100, None, due)
            .await
            .unwrap()
            .removed,
        0
    );
}

#[tokio::test]
async fn maintenance_schema_enforces_raw_integer_bounds() {
    let (db, _) = fixture().await;
    for (key, max) in [
        ("bounce_you_are_disabled_warnings", 100),
        ("bounce_you_are_disabled_warnings_interval", 36500),
        ("bounce_notify_owner_on_removal", 1),
    ] {
        for bad in [-1, max + 1] {
            assert!(
                sqlx::query(&format!("UPDATE mailing_lists SET {key}=$1"))
                    .bind(bad)
                    .execute(db.pool())
                    .await
                    .is_err()
            );
        }
        sqlx::query(&format!("UPDATE mailing_lists SET {key}=0"))
            .execute(db.pool())
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn maintenance_config_defaults_validation_and_persistence() {
    let (db, id) = fixture().await;
    let value = serde_json::to_value(db.lists().get(&id).await.unwrap()).unwrap();
    for (key, default, valid, invalid) in [
        (
            "bounce_you_are_disabled_warnings",
            json!(3),
            json!(100),
            json!(101),
        ),
        (
            "bounce_you_are_disabled_warnings_interval",
            json!(7),
            json!(36500),
            json!(36501),
        ),
        (
            "bounce_notify_owner_on_removal",
            json!(true),
            json!(false),
            json!(1),
        ),
    ] {
        assert_eq!(value[key], default, "{key} default");
        let mut legacy = value.clone();
        legacy.as_object_mut().unwrap().remove(key);
        let restored: listmngr_core::MailingList = serde_json::from_value(legacy).unwrap();
        assert_eq!(serde_json::to_value(restored).unwrap()[key], default);
        db.lists().update(&id, &json!({key:valid})).await.unwrap();
        assert_eq!(
            serde_json::to_value(db.lists().get(&id).await.unwrap()).unwrap()[key],
            valid
        );
        for bad in [invalid, json!(-1), json!(null), json!("3"), json!(1.5)] {
            assert!(db.lists().update(&id, &json!({key:bad})).await.is_err());
        }
    }
    assert_eq!(value["process_bounces"], false);
}
