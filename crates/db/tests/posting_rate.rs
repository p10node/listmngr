//! The posting-rate ledger: one row per accepted post, written by the
//! accept transaction when the runner names the sender; counted per list
//! and sender, lower-cased, at or after a moment; swept after a day; gone
//! with its list — on `SQLite` and, in an isolated schema, on `PostgreSQL`.
use listmngr_core::ListId;
use listmngr_db::{
    Database, NewList,
    mail_queue::{AcceptEffects, NewMessage, Queue},
};

const HOUR: i64 = 3_600_000;
const DAY: i64 = 86_400_000;
const ALICE: &str = "alice@example.invalid";

async fn accept(db: &Database, list: &ListId, sender: Option<&str>, n: u32) {
    let now = chrono::Utc::now().timestamp_millis();
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: b"From: a@example.invalid\r\n\r\nbody\r\n".to_vec(),
                external_id: format!("<{n}@example.invalid>"),
                context: serde_json::json!({"list_id": list, "envelope_sender": sender})
                    .to_string(),
                queue: Queue::In,
                max_attempts: 3,
            },
            now,
        )
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .live()
        .claim(Queue::In, "ledger-fixture", now, 30_000)
        .await
        .unwrap()
        .expect("the job just queued");
    let effects = AcceptEffects {
        list_id: list,
        record_post: false,
        acknowledge: None,
        dmarc_mitigate: false,
        authentication_results: None,
        arc_chain: None,
        posting_rate_sender: sender,
    };
    db.mail_queue()
        .live()
        .complete_accepted(&lease, now, &[], None, Some(&effects))
        .await
        .unwrap();
}

async fn rows(db: &Database) -> Vec<(String, String)> {
    sqlx::query_as("SELECT list_id, email FROM posting_rate ORDER BY list_id, email, posted_at")
        .fetch_all(db.pool())
        .await
        .unwrap()
}

async fn count(db: &Database, list: &ListId, email: &str, since: i64) -> u32 {
    db.posting_rate()
        .count_since(list, email, since)
        .await
        .unwrap()
}

async fn fixture(db: &Database) -> (ListId, ListId) {
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    let dev: ListId = "dev.example.invalid".parse().unwrap();
    let ops: ListId = "ops.example.invalid".parse().unwrap();
    for list in [&dev, &ops] {
        db.lists()
            .create(NewList {
                list_id: list.clone(),
                display_name: "List".into(),
                style: "legacy-default".into(),
            })
            .await
            .unwrap();
    }
    (dev, ops)
}

/// Step 1: rows are written lower-cased, only when a sender is named, and
/// counted per list and sender without regard to case.
async fn written_and_counted(db: &Database, dev: &ListId, ops: &ListId) {
    accept(db, dev, Some(ALICE), 1).await;
    accept(db, dev, Some("Alice@Example.INVALID"), 2).await;
    accept(db, dev, Some("Bob@example.invalid"), 3).await;
    accept(db, ops, Some(ALICE), 4).await;
    accept(db, dev, None, 5).await;
    let row = |list: &ListId, email: &str| (list.to_string(), email.to_owned());
    assert_eq!(
        rows(db).await,
        vec![
            row(dev, ALICE),
            row(dev, ALICE),
            row(dev, "bob@example.invalid"),
            row(ops, ALICE),
        ]
    );
    let now = chrono::Utc::now().timestamp_millis();
    assert_eq!(count(db, dev, ALICE, now - HOUR).await, 2);
    assert_eq!(count(db, dev, "ALICE@example.invalid", now - HOUR).await, 2);
    assert_eq!(count(db, dev, "bob@example.invalid", now - HOUR).await, 1);
    assert_eq!(count(db, ops, ALICE, now - HOUR).await, 1);
    assert_eq!(count(db, ops, "bob@example.invalid", now - HOUR).await, 0);
}

/// Step 2: the window is inclusive at its start and excludes what is older.
async fn window_boundary(db: &Database, dev: &ListId) {
    let now = chrono::Utc::now().timestamp_millis();
    sqlx::query("UPDATE posting_rate SET posted_at=$1 WHERE list_id=$2 AND email=$3")
        .bind(now - HOUR)
        .bind(dev.as_str())
        .bind(ALICE)
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(count(db, dev, ALICE, now - HOUR).await, 2);
    assert_eq!(count(db, dev, ALICE, now - HOUR + 1).await, 0);
}

/// Step 3: the sweep forgets rows older than a day, nothing younger, says
/// so in its summary and its audit row, and a list's rows go with the list.
async fn swept_and_cascaded(db: &Database, dev: &ListId, ops: &ListId) {
    let now = chrono::Utc::now().timestamp_millis();
    sqlx::query("UPDATE posting_rate SET posted_at=$1 WHERE list_id=$2")
        .bind(now - DAY - 1)
        .bind(dev.as_str())
        .execute(db.pool())
        .await
        .unwrap();
    let summary = db.tasks().sweep(now, HOUR).await.unwrap();
    assert_eq!(summary.expired_posting_rate, 3, "{summary:?}");
    assert_eq!(rows(db).await, vec![(ops.to_string(), ALICE.to_owned())]);
    let audited: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log WHERE action='task.sweep' AND target_id='posting_rate'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(audited, 1);
    let again = db.tasks().sweep(now, HOUR).await.unwrap();
    assert_eq!(again.expired_posting_rate, 0);
    db.lists().delete(ops).await.unwrap();
    assert!(rows(db).await.is_empty());
}

async fn scenario(db: &Database) {
    let (dev, ops) = fixture(db).await;
    written_and_counted(db, &dev, &ops).await;
    window_boundary(db, &dev).await;
    swept_and_cascaded(db, &dev, &ops).await;
}

#[tokio::test]
async fn sqlite_posting_rate_ledger() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    scenario(&db).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_posting_rate_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("posting_rate")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 2).await.unwrap();
    scenario(&db).await;
    db.pool().close().await;
    schema.drop().await.unwrap();
}
