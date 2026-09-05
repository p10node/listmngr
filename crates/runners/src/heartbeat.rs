//! Shared lease-renewal wrapper for both queue processors.
//!
//! The renewal period is derived from the lease's own TTL (never from an
//! unrelated per-operation timeout such as an SMTP command timeout), so a
//! slow operation can never outlive its lease before the first renewal.
use listmngr_db::Database;
use listmngr_db::mail_queue::Lease;
use std::time::Duration;

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
        let now_ms = chrono::Utc::now().timestamp_millis();
        if !matches!(
            tokio::time::timeout(period, db.mail_queue().heartbeat(lease, now_ms, lease_ms)).await,
            Ok(Ok(_))
        ) {
            tracing::warn!(job = %lease.job.id.0, "lease lost mid-operation; abandoning without further writes");
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
        let work = async {
            tokio::time::sleep(Duration::from_millis(350)).await;
            assert!(
                db.mail_queue()
                    .claim(
                        Queue::In,
                        "intruder",
                        chrono::Utc::now().timestamp_millis(),
                        150
                    )
                    .await
                    .unwrap()
                    .is_none(),
                "live work lost its lease"
            );
        };
        assert!(matches!(
            run_while_renewing(&db, &lease, 150, work).await,
            Outcome::Completed(())
        ));
    }

    #[tokio::test]
    async fn failed_renewal_drops_work_before_next_side_effect() {
        let (db, lease) = fixture(150).await;
        db.mail_queue()
            .ack(&lease, chrono::Utc::now().timestamp_millis())
            .await
            .unwrap();
        let work = async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            panic!("side effect after lease loss");
        };
        assert!(matches!(
            run_while_renewing(&db, &lease, 150, work).await,
            Outcome::LeaseLost
        ));
    }
}
