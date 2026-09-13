use super::cases::at;
use super::*;

struct OwnedDir(std::path::PathBuf);
impl Drop for OwnedDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn sqlite_file_multiconnection_sweeps_have_one_positive_interval_winner() {
    let dir = OwnedDir(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(format!(
                "bounce-maintenance-sqlite-{}",
                uuid::Uuid::now_v7()
            )),
    );
    std::fs::create_dir_all(&dir.0).unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.0.join("concurrent.db").display()
    );
    let db = Database::connect(&url, 5).await.unwrap();
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    db.lists()
        .create(NewList {
            list_id: "maintenance.example.invalid".parse().unwrap(),
            display_name: String::new(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    disabled(
        &db,
        &"maintenance.example.invalid".parse().unwrap(),
        "Concurrent@Example.invalid",
    )
    .await;
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(3));
    let mut tasks = Vec::new();
    for _ in 0..2 {
        let db = db.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            db.bounce_maintenance()
                .sweep_at(100, None, at())
                .await
                .unwrap()
        }));
    }
    barrier.wait().await;
    let mut warned = 0;
    let mut failed = 0;
    for task in tasks {
        let result = task.await.unwrap();
        warned += result.warned;
        failed += result.failed;
    }
    assert_eq!(warned, 1);
    assert_eq!(failed, 0);
    db.pool().close().await;
    let reopened = Database::connect(&url, 1).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workflow_notices")
        .fetch_one(reopened.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
    reopened.pool().close().await;
}
