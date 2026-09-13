use super::*;
use std::time::Duration;

async fn wait_at_ack(
    db: &Database,
    lease: &Lease,
    final_ms: i64,
    empty: bool,
) -> listmngr_core::Result<()> {
    let mut blocker = db.pool().begin().await.unwrap();
    let key: i64 = sqlx::query_scalar("SELECT pg_backend_pid()::bigint")
        .fetch_one(&mut *blocker)
        .await
        .unwrap();
    sqlx::query(&format!("CREATE OR REPLACE FUNCTION digest_audit_barrier() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='queue.ack' THEN PERFORM pg_advisory_xact_lock({key}::bigint); END IF; RETURN NEW; END $$")).execute(db.pool()).await.unwrap();
    sqlx::query("CREATE TRIGGER digest_audit_barrier BEFORE INSERT ON audit_log FOR EACH ROW EXECUTE FUNCTION digest_audit_barrier()").execute(db.pool()).await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(key)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let clock = Clock::new(0, 101);
    let mutation = complete(db, lease, &clock, empty);
    tokio::pin!(mutation);
    let observed = async {
        loop {
            let waiting: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pg_locks w WHERE w.locktype='advisory' AND w.classid=0 AND w.objid::bigint=$1 AND NOT w.granted AND w.database=(SELECT oid FROM pg_database WHERE datname=current_database()) AND EXISTS(SELECT 1 FROM pg_locks q WHERE q.pid=w.pid AND q.database=w.database AND q.relation='queue_jobs'::regclass AND q.granted)")
                .bind(key).fetch_one(db.pool()).await.unwrap();
            if waiting > 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
    };
    tokio::select! {
        result = &mut mutation => panic!("digest did not reach ACK audit barrier: {result:?}"),
        result = tokio::time::timeout(Duration::from_secs(5), observed) => result.expect("digest ACK audit wait not observed"),
    }
    clock.final_ms.store(final_ms, Ordering::SeqCst);
    blocker.commit().await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), mutation)
        .await
        .unwrap();
    sqlx::query("DROP TRIGGER digest_audit_barrier ON audit_log")
        .execute(db.pool())
        .await
        .unwrap();
    result
}

async fn matrix(db: &Database) {
    let list = seed(db).await;
    for empty in [false, true] {
        for (final_ms, renewed, expires) in [
            (200, false, true),
            (199, false, false),
            (250, true, false),
            (401, true, true),
        ] {
            let lease = fixture(db, &list).await;
            if renewed {
                db.mail_queue().heartbeat(&lease, 101, 300).await.unwrap();
            }
            let job = db.mail_queue().job(lease.job.id).await.unwrap();
            let before = snapshot(db).await;
            let result = wait_at_ack(db, &lease, final_ms, empty).await;
            if expires {
                assert!(
                    matches!(result, Err(Error::Conflict(_))),
                    "digest ACK audit expiry committed: {result:?}"
                );
                assert_eq!(db.mail_queue().job(lease.job.id).await.unwrap(), job);
                assert_eq!(snapshot(db).await, before);
                complete(db, &lease, &Clock::new(0, 101), empty)
                    .await
                    .unwrap();
            } else {
                result.unwrap();
            }
            success(db, &lease, &before, empty).await;
            eprintln!(
                "observed digest ACK audit wait: empty={empty}, renewed={renewed}, final_ms={final_ms}, expires={expires}: PASS"
            );
        }
    }
}

#[tokio::test]
#[ignore = "requires explicit owned disposable TEST_POSTGRES_URL; owns schema"]
async fn postgres_digest_final_audit_wait() {
    let url = std::env::var("TEST_POSTGRES_URL").expect("explicit disposable fixture required");
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("digest_audit_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let isolated = format!(
        "{url}{}options=-csearch_path%3D{schema}",
        if url.contains('?') { '&' } else { '?' }
    );
    let result = tokio::spawn(async move {
        let db = Database::connect(&isolated, 4).await.unwrap();
        matrix(&db).await;
        db.pool().close().await;
    })
    .await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    result.unwrap();
}
