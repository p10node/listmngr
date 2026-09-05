use listmngr_db::{
    Database,
    mail_queue::{NewMessage, Queue},
};

#[tokio::test]
async fn lease_debug_never_discloses_the_fencing_capability() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: b"Message-ID: <debug@example.invalid>\r\n\r\nbody".to_vec(),
                external_id: "debug@example.invalid".into(),
                context: "fixture".into(),
                queue: Queue::In,
                max_attempts: 3,
            },
            100,
        )
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::In, "test-worker", 100, 1000)
        .await
        .unwrap()
        .unwrap();
    let capability: String = sqlx::query_scalar("SELECT lease_token FROM queue_jobs WHERE id=$1")
        .bind(lease.job.id.0.to_string())
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(
        !format!("{lease:?}").contains(&capability),
        "Debug must redact the fencing capability"
    );
}
