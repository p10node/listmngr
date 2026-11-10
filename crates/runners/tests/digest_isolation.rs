//! One list whose issue cannot be rendered fails only that list: the shared
//! digest sweep still publishes every other due list.
use listmngr_core::ListId;
use listmngr_db::digests::DigestRecipient;
use listmngr_db::mail_queue::{NewMessage, Queue};
use listmngr_db::{Database, NewList};

async fn collect_one(db: &Database, list: &ListId) {
    let now = chrono::Utc::now().timestamp_millis();
    db.mail_queue()
        .live()
        .enqueue(
            NewMessage {
                raw: b"Subject: post\r\n\r\nbody".to_vec(),
                external_id: format!("<{}@example.invalid>", uuid::Uuid::now_v7()),
                context: "{}".into(),
                queue: Queue::Digest,
                max_attempts: 3,
            },
            now,
        )
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .live()
        .claim(Queue::Digest, "test", now, 30_000)
        .await
        .unwrap()
        .unwrap();
    db.digests()
        .live()
        .collect(
            &lease,
            list,
            b"From: a@example.invalid\r\nSubject: hi\r\nMessage-ID: <m@example.invalid>\r\n\r\nbody",
            &[DigestRecipient {
                email: "digest@example.invalid".into(),
                mode: "mime_digests".into(),
            }],
            now,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn an_unrenderable_list_does_not_stop_the_sweep() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    let bad: ListId = "aaa.example.invalid".parse().unwrap();
    let good: ListId = "bbb.example.invalid".parse().unwrap();
    for list in [&bad, &good] {
        db.lists()
            .create(NewList {
                list_id: list.clone(),
                display_name: "List".into(),
                style: "legacy-default".into(),
            })
            .await
            .unwrap();
        collect_one(&db, list).await;
    }
    // The owner's own setting makes the issue unrenderable (> 200 bytes).
    db.lists()
        .update(&bad, &serde_json::json!({"display_name": "x".repeat(201)}))
        .await
        .unwrap();
    let published = listmngr_runners::digests::tick(&db, "digest-test", true)
        .await
        .expect("a per-list failure must not fail the sweep");
    assert_eq!(published, 1, "the healthy list still publishes");
    assert_eq!(
        db.digests()
            .live()
            .flush(
                &good,
                chrono::Utc::now().timestamp_millis(),
                true,
                listmngr_runners::digests::render
            )
            .await
            .unwrap(),
        0,
        "nothing left pending for the healthy list"
    );
}
