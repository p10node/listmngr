//! Shared lease-renewal wrapper for both queue processors.
//!
//! The renewal period is derived from the lease's own TTL (never from an
//! unrelated per-operation timeout such as an SMTP command timeout).
//! Renewal failure or timeout cancels work conservatively. Scheduler stalls
//! can still expire a lease; persistent writes must retain their own fencing.
use listmngr_db::Database;
use listmngr_db::mail_queue::Lease;
use std::time::Duration;

#[cfg(test)]
#[path = "heartbeat_schedule_tests.rs"]
mod schedule_tests;

/// The outcome of a future run under active lease renewal.
pub enum Outcome<T> {
    /// The future completed before the lease was ever lost.
    Completed(T),
    /// A renewal was fenced (the lease is stale, expired, or was reclaimed by
    /// another worker). `future` was dropped immediately — aborting whatever
    /// in-flight work it represented as early as possible — and the caller
    /// must not perform any further writes against this job: another worker
    /// may now own it.
    LeaseLost,
}

/// Poll `future` to completion while periodically renewing `lease`, derived
/// from `lease_ms` (never from an unrelated per-operation timeout).
pub async fn run_while_renewing<F, T>(
    db: &Database,
    lease: &Lease,
    lease_ms: i64,
    future: F,
) -> Outcome<T>
where
    F: std::future::Future<Output = T>,
{
    let result = renewing(lease_ms, future, || async {
        db.mail_queue()
            .live()
            .heartbeat(lease, chrono::Utc::now().timestamp_millis(), lease_ms)
            .await
            .is_ok()
    })
    .await;
    if matches!(result, Outcome::LeaseLost) {
        tracing::warn!(job = %lease.job.id.0, "lease lost mid-operation; abandoning without further writes");
    }
    result
}

// The production scheduler, with only the renewal I/O injected. This lets
// virtual-time tests exercise cadence/cancellation without timing SQLite's
// background thread against Tokio's automatically advancing test clock.
async fn renewing<F, T, R, RF>(lease_ms: i64, future: F, mut renew: R) -> Outcome<T>
where
    F: std::future::Future<Output = T>,
    R: FnMut() -> RF,
    RF: std::future::Future<Output = bool>,
{
    tokio::pin!(future);
    // Renew at roughly a third of the lease TTL, so a single missed or slow
    // renewal round-trip still leaves margin before the lease can expire.
    let period = Duration::from_millis(u64::try_from(lease_ms / 3).unwrap_or(1).max(1));
    let mut ticker = tokio::time::interval(period);
    // Validate ownership before first polling work, then renew within the TTL.
    let mut first = true;
    loop {
        if first {
            ticker.tick().await;
            first = false;
        } else {
            tokio::select! {
                biased;
                _ = ticker.tick() => {},
                output = &mut future => return Outcome::Completed(output),
            }
        }
        if !matches!(tokio::time::timeout(period, renew()).await, Ok(true)) {
            return Outcome::LeaseLost;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use listmngr_db::mail_queue::{NewMessage, Queue};

    async fn fixture(ttl: i64) -> (Database, Lease) {
        let db = Database::connect("sqlite::memory:", 1).await.unwrap();
        db.migrate().await.unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        db.mail_queue()
            .enqueue(
                NewMessage {
                    raw: b"x".to_vec(),
                    external_id: "renew".into(),
                    context: "{}".into(),
                    queue: Queue::In,
                    max_attempts: 5,
                },
                now,
            )
            .await
            .unwrap();
        let lease = db
            .mail_queue()
            .claim(Queue::In, "owner", now, ttl)
            .await
            .unwrap()
            .unwrap();
        (db, lease)
    }

    #[tokio::test]
    async fn short_lease_renews_during_slow_work_and_blocks_second_claimant() {
        let (db, lease) = fixture(150).await;
        // Repository authority uses explicit fixture time. Cadence and work
        // cancellation are exercised by the production scheduler tests.
        let start = lease.job.lease_until.unwrap() - 150;
        for elapsed in (0..=350).step_by(50) {
            db.mail_queue()
                .heartbeat(&lease, start + elapsed, 150)
                .await
                .unwrap();
        }
        assert!(
            db.mail_queue()
                .claim(Queue::In, "intruder", start + 351, 150)
                .await
                .unwrap()
                .is_none()
        );
        let replacement = db
            .mail_queue()
            .claim(Queue::In, "intruder", start + 500, 150)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(replacement.job.id, lease.job.id);
        assert!(
            db.mail_queue()
                .heartbeat(&lease, start + 500, 150)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn failed_renewal_drops_work_before_next_side_effect() {
        let (db, lease) = fixture(60_000).await;
        db.mail_queue()
            .live()
            .ack(&lease, chrono::Utc::now().timestamp_millis())
            .await
            .unwrap();
        let work = async {
            panic!("side effect after lease loss");
        };
        assert!(matches!(
            run_while_renewing(&db, &lease, 60_000, work).await,
            Outcome::LeaseLost
        ));
    }

    #[tokio::test]
    async fn unavailable_connection_cancels_before_polling_ready_work() {
        let (db, lease) = fixture(60_000).await;
        let connection = db.pool().acquire().await.unwrap();
        let result = run_while_renewing(&db, &lease, 150, async {
            panic!("work must not run without a successful renewal");
        })
        .await;
        assert!(matches!(result, Outcome::LeaseLost));
        drop(connection);
        assert!(
            db.mail_queue()
                .live()
                .heartbeat(&lease, 0, 60_000)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn runtime_wrapper_renews_in_database_before_work() {
        let (db, lease) = fixture(60_000).await;
        let previous = lease.job.lease_until.unwrap();
        let result = run_while_renewing(&db, &lease, 120_000, async {
            let until: i64 = sqlx::query_scalar("SELECT lease_until FROM queue_jobs WHERE id=$1")
                .bind(lease.job.id.0.to_string())
                .fetch_one(db.pool())
                .await
                .unwrap();
            assert!(until > previous, "actual database renewal precedes work");
            42
        })
        .await;
        assert!(matches!(result, Outcome::Completed(42)));
    }
}
