use super::{command, migrated, report, status};
use listmngr_db::Database;

#[tokio::test]
async fn ledger_failures_are_read_only_and_fail_closed() {
    for (mutation, reason) in [
        (
            "UPDATE _sqlx_migrations SET success=0 WHERE version=(SELECT min(version) FROM _sqlx_migrations)",
            "schema_dirty",
        ),
        (
            "UPDATE _sqlx_migrations SET checksum=zeroblob(length(checksum)) WHERE version=(SELECT min(version) FROM _sqlx_migrations)",
            "schema_mismatch",
        ),
        (
            "UPDATE _sqlx_migrations SET version=999999 WHERE version=(SELECT max(version) FROM _sqlx_migrations)",
            "schema_mismatch",
        ),
        (
            "DELETE FROM _sqlx_migrations WHERE version=(SELECT max(version) FROM _sqlx_migrations)",
            "schema_mismatch",
        ),
    ] {
        let (dir, url) = migrated().await;
        let db = Database::connect(&url, 1).await.unwrap();
        sqlx::query(mutation).execute(db.pool()).await.unwrap();
        db.pool().close().await;
        let path = dir.path().join("fixture.sqlite");
        let before = std::fs::read(&path).unwrap();
        let result = report(&mut command(&url), false);
        assert_eq!(result["checks"][0]["detail"], reason);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[tokio::test]
async fn postgres_errors_never_expose_credentials() {
    // Owned listening socket deliberately accepts no PostgreSQL sessions.
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "postgres://SENTINEL_USER:SENTINEL_SECRET@{}/SENTINEL_DB",
        socket.local_addr().unwrap()
    );
    let output = command(&url).env("RUST_LOG", "trace").output().unwrap();
    assert_eq!(output.status.code(), Some(12));
    for bytes in [&output.stdout, &output.stderr] {
        assert!(!String::from_utf8_lossy(bytes).contains("SENTINEL"));
    }
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status(&result, "database"), "fail");
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_doctor_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("doctor")
        .await
        .unwrap();
    let url = schema.url.clone();
    let result = tokio::spawn(async move {
        let db = Database::connect(&url, 1).await.unwrap();
        assert_eq!(status(&report(&mut command(&url), false), "database"), "fail");
        let exists: Option<String> = sqlx::query_scalar("SELECT to_regclass('_sqlx_migrations')::text").fetch_one(db.pool()).await.unwrap();
        assert!(exists.is_none(), "doctor created a migration ledger");
        db.migrate().await.unwrap();
        let before: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations").fetch_one(db.pool()).await.unwrap();
        assert_eq!(status(&report(&mut command(&url), true), "database"), "ok");
        let after: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations").fetch_one(db.pool()).await.unwrap();
        assert_eq!(before, after);
        sqlx::query("UPDATE _sqlx_migrations SET success=false WHERE version=(SELECT min(version) FROM _sqlx_migrations)").execute(db.pool()).await.unwrap();
        let unhealthy = report(&mut command(&url), false);
        assert_eq!(unhealthy["checks"][0]["detail"], "schema_dirty");
        db.pool().close().await;
    }).await;
    schema.drop().await.unwrap();
    result.unwrap();
}
