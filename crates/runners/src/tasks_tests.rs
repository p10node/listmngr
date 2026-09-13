use super::*;

async fn fixture() -> Database {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    // A help cooldown row from an hour ago is the smallest thing the sweep
    // expires without a list or a member.
    sqlx::query("INSERT INTO email_help_requests(list_id,email,requested_at) VALUES('dev.example.invalid','x@example.net',$1)")
        .bind(chrono::Utc::now().timestamp_millis() - 7_200_000)
        .execute(db.pool())
        .await
        .unwrap();
    db
}

async fn sweeps(db: &Database) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='task.sweep'")
        .fetch_one(db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn the_sweep_runs_on_its_interval_and_stops_on_shutdown() {
    let db = fixture().await;
    let (tx, rx) = watch::channel(false);
    let mut tasks = tokio::task::JoinSet::new();
    tasks.spawn(run(
        db.clone(),
        Duration::from_millis(20),
        Duration::from_secs(86_400),
        rx,
    ));
    let started = std::time::Instant::now();
    while sweeps(&db).await == 0 {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the first interval never swept"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM email_help_requests")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(left, 0, "the expired cooldown row is gone");
    tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(1), tasks.join_next())
        .await
        .expect("the runner leaves on shutdown")
        .unwrap()
        .unwrap();
}
