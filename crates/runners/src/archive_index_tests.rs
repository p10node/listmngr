//! The archive runner's search step: a stored post reaches the index; a
//! job that archived nothing (policy `never`) adds nothing.
use crate::archive::index_message;
use listmngr_archive::search::{Query, SearchIndex};
use listmngr_db::mail_queue::{NewMessage, Queue};
use listmngr_db::{Database, NewList};
use std::sync::{Arc, Mutex};

async fn fixture() -> Database {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    for (name, policy) in [("dev", "public"), ("quiet", "never")] {
        let list = db
            .lists()
            .create(NewList {
                list_id: format!("{name}.example.invalid").parse().unwrap(),
                display_name: name.into(),
                style: "legacy-default".into(),
            })
            .await
            .unwrap();
        db.lists()
            .update(&list.id, &serde_json::json!({"archive_policy": policy}))
            .await
            .unwrap();
    }
    db
}

async fn archived(db: &Database, list: &str, id: &str) -> listmngr_db::mail_queue::MessageId {
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: format!("Message-ID: {id}\r\nFrom: A <a@example.invalid>\r\nSubject: Quarterly numbers\r\nContent-Type: text/plain\r\n\r\nThe quarterly numbers are in.\r\n").into_bytes(),
                external_id: id.into(),
                context: serde_json::json!({"version":1,"list_id":list}).to_string(),
                queue: Queue::Archive,
                max_attempts: 3,
            },
            100,
        )
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::Archive, "archive", 100, 1000)
        .await
        .unwrap()
        .unwrap();
    listmngr_archive::process(db, &lease, 101).await.unwrap();
    lease.job.message_id
}

#[tokio::test]
async fn an_archived_post_reaches_the_search_index_and_a_never_list_adds_nothing() {
    let db = fixture().await;
    let dir = tempfile::tempdir().unwrap();
    let index = SearchIndex::open(dir.path()).unwrap();
    let writer = Arc::new(Mutex::new(index.writer().unwrap()));
    let stored = archived(&db, "dev.example.invalid", "<q@example.invalid>").await;
    let skipped = archived(&db, "quiet.example.invalid", "<n@example.invalid>").await;
    index_message(&db, &writer, stored).await;
    index_message(&db, &writer, skipped).await;
    assert_eq!(
        writer.lock().unwrap().pending(),
        1,
        "one add waits for its batch"
    );
    writer.lock().unwrap().commit().unwrap();
    index.reload().unwrap();
    let query = |list: &'static str| Query {
        list,
        text: "quarterly numbers",
        thread: None,
        since_ms: None,
        until_ms: None,
        limit: 10,
        offset: 0,
    };
    assert_eq!(
        index.search(&query("dev.example.invalid")).unwrap().total,
        1
    );
    assert_eq!(
        index.search(&query("quiet.example.invalid")).unwrap().total,
        0
    );
}
