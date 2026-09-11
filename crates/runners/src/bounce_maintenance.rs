//! Opt-in, owned periodic bounce maintenance using the repository's real clock.
use listmngr_db::{Database, bounce_maintenance::BounceSweepSummary};
use std::{future::Future, time::Duration};
use tokio::sync::watch;
use uuid::Uuid;

#[cfg(test)]
#[path = "bounce_maintenance_tests.rs"]
mod tests;

/// Run one bounded page per completion-based interval until shutdown.
///
/// Caller must own/supervise this future; the mail role uses its `JoinSet`.
/// Supply validated positive interval/batch bounds from `MailRoleConfig`.
/// Shutdown cancels the in-flight page future, not earlier member commits.
/// `SQLx` transaction drop initiates rollback; this is not whole-page rollback
/// or proof that a commit whose acknowledgement was lost did not occur.
pub async fn run(
    db: Database,
    interval: Duration,
    batch_size: u32,
    shutdown: watch::Receiver<bool>,
) {
    schedule(interval, shutdown, |cursor| {
        let db = db.clone();
        async move { db.bounce_maintenance().sweep(batch_size, cursor).await }
    })
    .await;
}

async fn schedule<F, Fut>(interval: Duration, mut shutdown: watch::Receiver<bool>, mut page: F)
where
    F: FnMut(Option<Uuid>) -> Fut,
    Fut: Future<Output = listmngr_core::Result<BounceSweepSummary>>,
{
    let mut cursor = None;
    loop {
        if *shutdown.borrow() {
            return;
        }
        tokio::select! {
            biased;
            _ = shutdown.changed() => return,
            () = tokio::time::sleep(interval) => {}
        }
        let result = tokio::select! {
            biased;
            _ = shutdown.changed() => return,
            result = page(cursor) => result,
        };
        if let Ok(summary) = result {
            cursor = summary.next_cursor;
            tracing::info!(
                scanned = summary.scanned,
                warned = summary.warned,
                removed = summary.removed,
                failed = summary.failed,
                "bounce maintenance page completed"
            );
        } else {
            tracing::warn!("bounce maintenance page failed; retry deferred");
        }
    }
}
