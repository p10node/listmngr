//! `backup` and `restore`: every table of a site written as JSON lines
//! with a manifest, and read back into an empty database migrated to the
//! same schema — the same rows, the same message bytes whichever store
//! held them, on `SQLite` and on `PostgreSQL` in either direction; a target
//! that is not empty or not at the backup's schema is refused.
use listmngr_core::{MemberRole, SubscriptionMode};
use listmngr_db::backup::{Manifest, backup, restore};
use listmngr_db::blobs::BlobStore;
use listmngr_db::mail_queue::{NewMessage, Queue};
use listmngr_db::{Database, NewList, NewMember};
use serde_json::json;
use std::collections::BTreeMap;

const LIST: &str = "dev.example.invalid";
const RAW: &[u8] = b"From: poster@example.net\r\nSubject: hello\r\n\r\nthe body\r\n";

async fn fresh(store: BlobStore) -> Database {
    let db = Database::connect("sqlite::memory:", 1)
        .await
        .unwrap()
        .with_message_store(store);
    db.migrate().await.unwrap();
    db
}

/// A site with something in the tables that matter: a domain, a list
/// with settings of its own, two members, a queued post, a held post and
/// the audit rows all of that leaves.
async fn populate(db: &Database) -> listmngr_db::mail_queue::MessageId {
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: LIST.parse().unwrap(),
            display_name: "Dev".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    db.lists()
        .update(
            &list.id,
            &json!({"subject_prefix": "[Dev] ", "respond_to_post_requests": false, "description": "A list with \"quotes\", ünïcödé and\nnewlines"}),
        )
        .await
        .unwrap();
    for (email, role) in [
        ("owner@example.invalid", MemberRole::Owner),
        ("member@example.net", MemberRole::Member),
    ] {
        db.members()
            .create(NewMember {
                list_id: list.id.clone(),
                email: email.into(),
                display_name: "Someone".into(),
                role,
                subscription_mode: SubscriptionMode::AsAddress,
            })
            .await
            .unwrap();
    }
    // The post that is held: queued first, claimed, held.
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: b"From: other@example.net\r\nSubject: held\r\n\r\nheld body\r\n".to_vec(),
                external_id: "<held@example.net>".into(),
                context: json!({"list_id": LIST}).to_string(),
                queue: Queue::In,
                max_attempts: 3,
            },
            1_000,
        )
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::In, "backup-test", 1_000, 30_000)
        .await
        .unwrap()
        .unwrap();
    db.moderation()
        .hold(
            &lease,
            &list.id,
            "other@example.net",
            "held",
            "moderation",
            1_000,
        )
        .await
        .unwrap();
    // The post still waiting in the queue.
    let queued = db
        .mail_queue()
        .enqueue(
            NewMessage {
                raw: RAW.to_vec(),
                external_id: "<hello@example.net>".into(),
                context: json!({"list_id": LIST}).to_string(),
                queue: Queue::In,
                max_attempts: 3,
            },
            2_000,
        )
        .await
        .unwrap();
    queued.message_id
}

async fn counts(db: &Database, manifest: &Manifest) -> BTreeMap<String, i64> {
    let mut counts = BTreeMap::new();
    for table in &manifest.tables {
        let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {}", table.name))
            .fetch_one(db.pool())
            .await
            .unwrap();
        counts.insert(table.name.clone(), count);
    }
    counts
}

async fn list_json(db: &Database) -> serde_json::Value {
    serde_json::to_value(db.lists().get(&LIST.parse().unwrap()).await.unwrap()).unwrap()
}

async fn member_emails(db: &Database) -> Vec<String> {
    sqlx::query_scalar("SELECT a.email FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id=$1 ORDER BY a.email")
        .bind(LIST)
        .fetch_all(db.pool())
        .await
        .unwrap()
}

/// What the restored database must agree with the source on.
async fn same_site(
    source: &Database,
    target: &Database,
    manifest: &Manifest,
    message: listmngr_db::mail_queue::MessageId,
) {
    let before = counts(source, manifest).await;
    let after = counts(target, manifest).await;
    assert_eq!(before, after);
    assert!(before.values().sum::<i64>() > 0);
    assert_eq!(list_json(source).await, list_json(target).await);
    assert_eq!(member_emails(source).await, member_emails(target).await);
    let stored = target.mail_queue().message(message).await.unwrap();
    assert_eq!(stored.raw, RAW);
    let held: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM held_messages WHERE disposition IS NULL")
            .fetch_one(target.pool())
            .await
            .unwrap();
    assert_eq!(held, 1);
    assert_eq!(
        before["audit_log"], after["audit_log"],
        "the audit log is carried as it was, nothing added"
    );
}

#[tokio::test]
async fn a_backup_restores_into_an_empty_database_of_the_same_schema() {
    let source = fresh(BlobStore::db()).await;
    let message = populate(&source).await;
    let dir = tempfile::tempdir().unwrap();
    let manifest = backup(&source, dir.path()).await.unwrap();
    assert_eq!(manifest.format, 1);
    assert_eq!(manifest.database, "sqlite");
    assert_eq!(manifest.message_store, "db");
    assert!(manifest.tables.len() > 40, "{}", manifest.tables.len());
    assert!(manifest.rows() > 0);
    assert!(dir.path().join("manifest.json").is_file());
    assert!(dir.path().join("tables/mailing_lists.jsonl").is_file());
    let written: Manifest =
        serde_json::from_slice(&std::fs::read(dir.path().join("manifest.json")).unwrap()).unwrap();
    assert_eq!(written, manifest);
    // Parents before children: domains before lists before members.
    let position = |name: &str| manifest.tables.iter().position(|t| t.name == name).unwrap();
    assert!(position("domains") < position("mailing_lists"));
    assert!(position("mailing_lists") < position("members"));
    assert!(position("addresses") < position("members"));
    assert!(!manifest.tables.iter().any(|t| t.name.starts_with("_sqlx")));

    let target = fresh(BlobStore::db()).await;
    let restored = restore(&target, dir.path()).await.unwrap();
    assert_eq!(restored, manifest);
    same_site(&source, &target, &manifest, message).await;
    // The seeded styles are the backup's, once.
    let styles: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM list_styles")
        .fetch_one(target.pool())
        .await
        .unwrap();
    assert_eq!(styles, counts(&source, &manifest).await["list_styles"]);
    // A second restore finds the tables full.
    let again = restore(&target, dir.path()).await.unwrap_err();
    assert!(again.to_string().contains("is not empty"), "{again}");
    // A backup into the same directory is refused too.
    let twice = backup(&source, dir.path()).await.unwrap_err();
    assert!(twice.to_string().contains("already holds"), "{twice}");
}

#[tokio::test]
async fn the_bytes_of_an_external_store_travel_with_the_backup() {
    let root = tempfile::tempdir().unwrap();
    let source = fresh(BlobStore::fs(root.path())).await;
    let message = populate(&source).await;
    let key = BlobStore::key(RAW);
    let row: Vec<u8> = sqlx::query_scalar("SELECT raw FROM message_blobs WHERE store_key=$1")
        .bind(&key)
        .fetch_one(source.pool())
        .await
        .unwrap();
    assert!(row.is_empty(), "the source keeps the bytes outside");
    let dir = tempfile::tempdir().unwrap();
    let manifest = backup(&source, dir.path()).await.unwrap();
    assert_eq!(manifest.message_store, "fs");
    let lines = std::fs::read_to_string(dir.path().join("tables/message_blobs.jsonl")).unwrap();
    assert!(
        lines.contains(&base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            RAW
        )),
        "the backup carries the bytes: {lines}"
    );
    // Into a database that keeps the bytes in the row.
    let in_rows = fresh(BlobStore::db()).await;
    restore(&in_rows, dir.path()).await.unwrap();
    let row: Vec<u8> = sqlx::query_scalar("SELECT raw FROM message_blobs WHERE store_key=$1")
        .bind(&key)
        .fetch_one(in_rows.pool())
        .await
        .unwrap();
    assert_eq!(row, RAW);
    same_site(&source, &in_rows, &manifest, message).await;
    // And into one with a store of its own.
    let other_root = tempfile::tempdir().unwrap();
    let in_store = fresh(BlobStore::fs(other_root.path())).await;
    restore(&in_store, dir.path()).await.unwrap();
    assert_eq!(
        std::fs::read(
            other_root
                .path()
                .join(&key[..2])
                .join(&key[2..4])
                .join(&key)
        )
        .unwrap(),
        RAW
    );
    same_site(&source, &in_store, &manifest, message).await;
    // An object the source store lost makes the backup fail rather than
    // write an incomplete one.
    std::fs::remove_file(root.path().join(&key[..2]).join(&key[2..4]).join(&key)).unwrap();
    let lost = tempfile::tempdir().unwrap();
    let error = backup(&source, lost.path()).await.unwrap_err();
    assert!(error.to_string().contains("no object"), "{error}");
    assert!(!lost.path().join("manifest.json").exists());
}

#[tokio::test]
async fn a_foreign_or_damaged_backup_is_refused() {
    let source = fresh(BlobStore::db()).await;
    populate(&source).await;
    let dir = tempfile::tempdir().unwrap();
    backup(&source, dir.path()).await.unwrap();
    let manifest_path = dir.path().join("manifest.json");
    let original = std::fs::read_to_string(&manifest_path).unwrap();
    // A checksum that is not this binary's.
    let mut manifest: Manifest = serde_json::from_str(&original).unwrap();
    manifest.migrations[0].checksum = "00".repeat(48);
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let error = restore(&fresh(BlobStore::db()).await, dir.path())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("does not know"), "{error}");
    // A format from the future.
    let mut manifest: Manifest = serde_json::from_str(&original).unwrap();
    manifest.format = 2;
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let error = restore(&fresh(BlobStore::db()).await, dir.path())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("format"), "{error}");
    // A row count that does not match its file.
    let mut manifest: Manifest = serde_json::from_str(&original).unwrap();
    let lists = manifest
        .tables
        .iter_mut()
        .find(|t| t.name == "mailing_lists")
        .unwrap();
    lists.rows += 1;
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let target = fresh(BlobStore::db()).await;
    let error = restore(&target, dir.path()).await.unwrap_err();
    assert!(error.to_string().contains("manifest says"), "{error}");
    let lists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mailing_lists")
        .fetch_one(target.pool())
        .await
        .unwrap();
    assert_eq!(lists, 0, "nothing of a refused restore stays");
    // No manifest at all.
    std::fs::remove_file(&manifest_path).unwrap();
    assert!(
        restore(&fresh(BlobStore::db()).await, dir.path())
            .await
            .is_err()
    );
    // An unmigrated target.
    std::fs::write(&manifest_path, &original).unwrap();
    let unmigrated = Database::connect("sqlite::memory:", 1).await.unwrap();
    assert!(restore(&unmigrated, dir.path()).await.is_err());
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_backup_round_trip_contract() {
    // SQLite → PostgreSQL → SQLite, the site unchanged along the way.
    let source = fresh(BlobStore::db()).await;
    let message = populate(&source).await;
    let from_sqlite = tempfile::tempdir().unwrap();
    let manifest = backup(&source, from_sqlite.path()).await.unwrap();
    let schema = listmngr_db::test_support::IsolatedSchema::create("backup")
        .await
        .unwrap();
    let postgres = Database::connect(&schema.url, 2).await.unwrap();
    postgres.migrate().await.unwrap();
    restore(&postgres, from_sqlite.path()).await.unwrap();
    same_site(&source, &postgres, &manifest, message).await;
    let from_postgres = tempfile::tempdir().unwrap();
    let back = backup(&postgres, from_postgres.path()).await.unwrap();
    assert_eq!(back.database, "postgres");
    assert_eq!(
        back.tables
            .iter()
            .map(|t| (&t.name, t.rows))
            .collect::<Vec<_>>(),
        manifest
            .tables
            .iter()
            .map(|t| (&t.name, t.rows))
            .collect::<Vec<_>>(),
        "the same tables with the same counts, in the same order"
    );
    let again = fresh(BlobStore::db()).await;
    restore(&again, from_postgres.path()).await.unwrap();
    same_site(&source, &again, &manifest, message).await;
    assert!(
        restore(&postgres, from_sqlite.path()).await.is_err(),
        "not empty"
    );
    postgres.pool().close().await;
    schema.drop().await.unwrap();
}
