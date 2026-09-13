//! Mailman's task runner: the periodic sweep, on the mail role's clock.
use listmngr_db::{Database, tasks::TaskSummary};
use std::time::Duration;
use tokio::sync::watch;

#[cfg(test)]
#[path = "tasks_tests.rs"]
mod tests;

/// Sweep every `interval` until shutdown, collecting finished work older
/// than `retention`. The caller owns and supervises this future.
pub async fn run(
    db: Database,
    interval: Duration,
    retention: Duration,
    mut shutdown: watch::Receiver<bool>,
) {
    let retention_ms = i64::try_from(retention.as_millis()).unwrap_or(i64::MAX);
    loop {
        if *shutdown.borrow() {
            return;
        }
        tokio::select! {
            biased;
            _ = shutdown.changed() => return,
            () = tokio::time::sleep(interval) => {}
        }
        let repo = db.tasks();
        let result = tokio::select! {
            biased;
            _ = shutdown.changed() => return,
            result = repo.sweep(chrono::Utc::now().timestamp_millis(), retention_ms) => result,
        };
        report(result);
    }
}

fn report(result: listmngr_core::Result<TaskSummary>) {
    match result {
        Ok(summary) if summary.changed() => {
            tracing::info!(?summary, "task sweep completed");
        }
        Ok(_) => tracing::debug!("task sweep found nothing to do"),
        Err(error) => tracing::warn!(%error, "task sweep failed; retry deferred"),
    }
}
