use listmngr_db::{
    Database,
    mail_queue::{NewMessage, Queue},
};
use std::{
    future::{Future, poll_fn},
    task::Poll,
};

fn input(context: &str) -> NewMessage {
    NewMessage {
        raw: b"Message-ID: <batch@example.invalid>\r\n\r\n\x00\xff\r\n".to_vec(),
        external_id: "<batch@example.invalid>".into(),
        context: context.into(),
        queue: Queue::In,
        max_attempts: 3,
    }
}

#[tokio::test]
async fn invalid_budget_remains_validation_even_when_database_is_unavailable() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.pool().close().await;
    let mut invalid = input("invalid");
    invalid.max_attempts = 0;
    assert!(matches!(
        db.mail_queue().enqueue(invalid.clone(), 100).await,
        Err(listmngr_core::Error::Validation(_))
    ));
    assert!(matches!(
        db.mail_queue()
            .enqueue_batch(&[input("valid"), invalid], 100)
            .await,
        Err(listmngr_core::Error::Validation(_))
    ));
}

#[tokio::test]
async fn batch_preserves_order_bytes_identity_and_audit() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let inputs = [input("second"), input("first"), input("second")];
    let jobs = db.mail_queue().enqueue_batch(&inputs, 100).await.unwrap();
    assert_eq!(jobs.len(), 3);
    assert_ne!(jobs[0].message_id, jobs[2].message_id);
    for (job, expected) in jobs.iter().zip(&inputs) {
        let stored = db.mail_queue().message(job.message_id).await.unwrap();
        assert_eq!(stored.raw, expected.raw);
        assert_eq!(stored.context, expected.context);
    }
    assert_eq!(counts(&db).await, (1, 3, 3, 3));
}

async fn counts(db: &Database) -> (i64, i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT COUNT(*) FROM message_blobs), (SELECT COUNT(*) FROM messages), (SELECT COUNT(*) FROM queue_jobs), (SELECT COUNT(*) FROM audit_log WHERE action='queue.enqueue')").fetch_one(db.pool()).await.unwrap()
}

// Cancel at successive actual asynchronous suspension points, not wall-clock
// sleeps. This covers inserts, audits, recipient boundaries and COMMIT. At the
// COMMIT boundary its acknowledgement may be lost: all or none is allowed,
// but a committed subset is never allowed.
#[tokio::test]
async fn cancellation_at_batch_suspension_points_never_commits_a_subset() {
    let mut cancelled = 0;
    let mut completed = 0;
    for limit in 1..=80 {
        struct TempDb(std::path::PathBuf);
        impl Drop for TempDb {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let directory = TempDb(std::env::temp_dir().join(uuid::Uuid::now_v7().to_string()));
        std::fs::create_dir(&directory.0).unwrap();
        let url = format!(
            "sqlite://{}?mode=rwc",
            directory.0.join("batch.db").display()
        );
        let db = Database::connect(&url, 1).await.unwrap();
        db.migrate().await.unwrap();
        let inputs = [input("first"), input("second"), input("third")];
        let repo = db.mail_queue();
        let mut future = Box::pin(repo.enqueue_batch(&inputs, 100));
        let mut pending = 0;
        let finished = poll_fn(|cx| match future.as_mut().poll(cx) {
            Poll::Ready(result) => {
                result.unwrap();
                Poll::Ready(true)
            }
            Poll::Pending => {
                pending += 1;
                if pending >= limit {
                    Poll::Ready(false)
                } else {
                    Poll::Pending
                }
            }
        })
        .await;
        drop(future);
        if finished {
            completed += 1;
        } else {
            cancelled += 1;
        }
        let actual = counts(&db).await;
        assert!(
            actual == (0, 0, 0, 0) || actual == (1, 3, 3, 3),
            "cancellation point {limit}: {actual:?}"
        );
    }
    assert!(cancelled > 0);
    assert!(completed > 0, "sweep must reach beyond batch completion");
}

#[tokio::test]
async fn second_audit_failure_rolls_back_bytes_jobs_and_first_audit() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    sqlx::query("CREATE TRIGGER fail_second_audit BEFORE INSERT ON audit_log WHEN NEW.action='queue.enqueue' AND (SELECT COUNT(*) FROM audit_log WHERE action='queue.enqueue')=1 BEGIN SELECT RAISE(ABORT, 'fixture audit failure'); END").execute(db.pool()).await.unwrap();
    assert!(
        db.mail_queue()
            .enqueue_batch(&[input("first"), input("second")], 100)
            .await
            .is_err()
    );
    assert_eq!(counts(&db).await, (0, 0, 0, 0));
}
