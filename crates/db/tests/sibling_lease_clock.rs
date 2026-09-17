use listmngr_core::{Error, ListId, Result};
use listmngr_db::{
    Database, NewList,
    archive::ArchiveMessage,
    mail_queue::{Lease, LeaseClock, NewMessage, Queue},
};
use std::sync::atomic::{AtomicI64, Ordering};

#[derive(Debug)]
struct Clock(AtomicI64);
impl LeaseClock for Clock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}
async fn seed(db: &Database) -> ListId {
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    let list: ListId = "clock.example.invalid".parse().unwrap();
    db.lists()
        .create(NewList {
            list_id: list.clone(),
            display_name: "Clock".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    list
}
async fn fixture(db: &Database, list: &ListId, queue: Queue) -> Lease {
    db.mail_queue().enqueue(NewMessage {
        raw: b"From: sender@example.invalid\r\nMessage-ID: <clock@example.invalid>\r\nSubject: clock\r\n\r\nbody".to_vec(),
        external_id: "<clock@example.invalid>".into(),
        context: serde_json::json!({"list_id":list.as_str()}).to_string(), queue, max_attempts:3,
    }, 100).await.unwrap();
    db.mail_queue()
        .claim(queue, "owner", 100, 100)
        .await
        .unwrap()
        .unwrap()
}
async fn complete(db: &Database, list: &ListId, lease: &Lease, clock: &Clock) -> Result<()> {
    if lease.job.queue == Queue::Archive {
        db.archive()
            .with_clock(clock)
            .complete(
                lease,
                &ArchiveMessage {
                    hash: lease.job.id.0.to_string(),
                    thread: lease.job.id.0.to_string(),
                    subject: String::new(),
                    body: String::new(),
                    raw: vec![],
                    ..ArchiveMessage::default()
                },
                101,
            )
            .await
    } else {
        db.digests()
            .with_clock(clock)
            .collect(lease, list, b"Subject: clock\r\n\r\nbody", &[], 101)
            .await
    }
}
async fn counts(db: &Database) -> (i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT COUNT(*) FROM audit_log), (SELECT COUNT(*) FROM archive_messages), (SELECT COUNT(*) FROM digest_posts)").fetch_one(db.pool()).await.unwrap()
}
async fn assert_result(
    db: &Database,
    list: &ListId,
    lease: &Lease,
    before: (i64, i64, i64),
    result: Result<()>,
    expired: bool,
) {
    if expired {
        assert!(
            matches!(result, Err(Error::Conflict(_))),
            "expired sibling committed: {result:?}"
        );
        assert_eq!(db.mail_queue().job(lease.job.id).await.unwrap(), lease.job);
        assert_eq!(
            counts(db).await,
            before,
            "index/collection/audit must roll back"
        );
        // Same capability is a positive control under an explicitly valid clock.
        complete(db, list, lease, &Clock(AtomicI64::new(199)))
            .await
            .unwrap();
    } else {
        result.unwrap();
    }
    let after = counts(db).await;
    assert!(after.0 > before.0);
    assert_eq!(
        after.1 - before.1,
        i64::from(lease.job.queue == Queue::Archive)
    );
    assert_eq!(
        after.2 - before.2,
        i64::from(lease.job.queue == Queue::Digest)
    );
}
async fn pool_matrix(db: &Database, list: &ListId) {
    for queue in [Queue::Digest, Queue::Archive] {
        for expired in [true, false] {
            let lease = fixture(db, list, queue).await;
            let before = counts(db).await;
            let clock = Clock(AtomicI64::new(101));
            let connection = db.pool().acquire().await.unwrap();
            let mutation = complete(db, list, &lease, &clock);
            tokio::pin!(mutation);
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(30), &mut mutation)
                    .await
                    .is_err(),
                "must wait on the exhausted pool"
            );
            clock
                .0
                .store(if expired { 200 } else { 199 }, Ordering::SeqCst);
            drop(connection);
            assert_result(db, list, &lease, before, mutation.await, expired).await;
        }
    }
}
#[tokio::test]
async fn sqlite_sibling_pool_expiry_rolls_back_and_valid_lease_commits() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    let list = seed(&db).await;
    pool_matrix(&db, &list).await;
    db.pool().close().await;
}

async fn hold_barrier(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    schema: &str,
    table: &str,
    list: &ListId,
    lease: &Lease,
) {
    let sql = match table {
        "mailing_lists" => {
            format!("UPDATE {schema}.mailing_lists SET display_name=display_name WHERE list_id=$1")
        }
        "queue_jobs" => format!("UPDATE {schema}.queue_jobs SET last_error=last_error WHERE id=$1"),
        _ if lease.job.queue == Queue::Archive => format!(
            "INSERT INTO {schema}.archive_messages(list_id,hash,thread,subject,body,raw_b64,created_at) VALUES($1,$2,$2,'','','',100)"
        ),
        _ => format!(
            "INSERT INTO {schema}.digest_posts(id,list_id,raw,recipients,accepted_at) VALUES($2,$1,decode('78','hex'),'[]',100)"
        ),
    };
    let query = sqlx::query(&sql);
    if table == "queue_jobs" {
        query
            .bind(lease.job.id.0.to_string())
            .execute(&mut **tx)
            .await
            .unwrap();
    } else if table == "mailing_lists" {
        query.bind(list.as_str()).execute(&mut **tx).await.unwrap();
    } else {
        let key = if lease.job.queue == Queue::Archive {
            lease.job.id.0
        } else {
            lease.job.message_id.0
        };
        query
            .bind(list.as_str())
            .bind(key.to_string())
            .execute(&mut **tx)
            .await
            .unwrap();
    }
}
#[tokio::test]
#[ignore = "requires explicit disposable TEST_POSTGRES_URL; owns isolated schema"]
async fn postgres_sibling_list_queue_and_pool_waits_are_fenced() {
    let url = std::env::var("TEST_POSTGRES_URL").expect("disposable PG fixture required");
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("sibling_clock_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let url = format!(
        "{url}{}options=-csearch_path%3D{schema}",
        if url.contains('?') { '&' } else { '?' }
    );
    let db = Database::connect(&url, 1).await.unwrap();
    let list = seed(&db).await;
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(db.pool())
        .await
        .unwrap();
    for queue in [Queue::Digest, Queue::Archive] {
        for table in ["mailing_lists", "queue_jobs", "publication"] {
            for expired in [true, false] {
                let lease = fixture(&db, &list, queue).await;
                let before = counts(&db).await;
                let clock = Clock(AtomicI64::new(101));
                let mut blocker = admin.begin().await.unwrap();
                hold_barrier(&mut blocker, &schema, table, &list, &lease).await;
                let mutation = complete(&db, &list, &lease, &clock);
                tokio::pin!(mutation);
                let wait = async {
                    loop {
                        let waiting: bool =
                            sqlx::query_scalar("SELECT cardinality(pg_blocking_pids($1)) > 0")
                                .bind(pid)
                                .fetch_one(&admin)
                                .await
                                .unwrap();
                        if waiting {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                };
                tokio::select! {
                    result = &mut mutation => panic!("must actually reach lock barrier, got {result:?}"),
                    result = tokio::time::timeout(std::time::Duration::from_secs(5), wait) => result.expect("PG lock wait not observed"),
                }
                clock
                    .0
                    .store(if expired { 200 } else { 199 }, Ordering::SeqCst);
                blocker.rollback().await.unwrap();
                assert_result(&db, &list, &lease, before, mutation.await, expired).await;
            }
        }
    }
    pool_matrix(&db, &list).await;
    db.pool().close().await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
