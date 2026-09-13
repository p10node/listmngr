//! The bounce runner: consumes `Queue::Bounces`, one leased report at a
//! time, through `BounceProcessingRepo::process`.
//!
//! Recognition, scoring, forwarding and the acknowledgement commit together
//! in the repository; this loop only claims, renews and retries.
use crate::MailRoleConfig;
use listmngr_db::Database;
use listmngr_db::mail_queue::Queue;
use std::time::Duration;
use tokio::sync::watch;

const IDLE_POLL: Duration = Duration::from_millis(500);

/// Run the bounce processor until `shutdown` is signalled.
pub async fn run(
    db: Database,
    role: MailRoleConfig,
    site_owner: String,
    worker: String,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        if *shutdown.borrow() {
            return;
        }
        let now_ms = chrono::Utc::now().timestamp_millis();
        match db
            .mail_queue()
            .live()
            .claim(Queue::Bounces, &worker, now_ms, role.in_lease_ms)
            .await
        {
            Ok(Some(lease)) => process_leased(&db, &role, &site_owner, &worker, &lease).await,
            Ok(None) => wait_or_shutdown(&mut shutdown).await,
            Err(error) => {
                tracing::error!(worker, %error, "bounce runner: claim failed");
                wait_or_shutdown(&mut shutdown).await;
            }
        }
    }
}

/// Process one claimed bounce under lease renewal, retrying on error.
async fn process_leased(
    db: &Database,
    role: &MailRoleConfig,
    site_owner: &str,
    worker: &str,
    lease: &listmngr_db::mail_queue::Lease,
) {
    let outcome = crate::heartbeat::run_while_renewing(
        db,
        lease,
        role.in_lease_ms,
        Box::pin(async {
            db.bounce_processing()
                .live()
                .process(
                    lease,
                    role.dsn_issuer.as_ref(),
                    site_owner,
                    chrono::Utc::now().timestamp_millis(),
                )
                .await
        }),
    )
    .await;
    let metrics = listmngr_core::metrics::global();
    match outcome {
        crate::heartbeat::Outcome::Completed(Ok(outcome)) => {
            metrics.bounces.add("scored", outcome.scored.len() as u64);
            metrics.bounces.inc(if outcome.unrecognized {
                "unrecognized"
            } else {
                "recognized"
            });
            tracing::debug!(
                worker,
                scored = outcome.scored.len(),
                unrecognized = outcome.unrecognized,
                "bounce processed"
            );
        }
        crate::heartbeat::Outcome::Completed(Err(error)) => {
            tracing::warn!(worker, %error, "bounce processing failed; retrying");
            metrics.bounces.inc("failed");
            let _ = db
                .mail_queue()
                .live()
                .retry(
                    lease,
                    chrono::Utc::now().timestamp_millis(),
                    role.backoff.delay_ms(lease.job.attempts),
                    "bounce processing error",
                )
                .await;
        }
        crate::heartbeat::Outcome::LeaseLost => {}
    }
}

async fn wait_or_shutdown(shutdown: &mut watch::Receiver<bool>) {
    tokio::select! {
        () = tokio::time::sleep(IDLE_POLL) => {}
        _ = shutdown.changed() => {}
    }
}
