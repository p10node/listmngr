//! Durable archive queue consumer. Parsing/indexing and queue ack are replay-safe.
use listmngr_db::{Database, mail_queue::Queue};
use std::time::Duration;
use tokio::sync::watch;
pub async fn run(db: Database, mut shutdown: watch::Receiver<bool>) {
    loop {
        if *shutdown.borrow() {
            return;
        }
        match db
            .mail_queue()
            .live()
            .claim(
                Queue::Archive,
                "archive-0",
                chrono::Utc::now().timestamp_millis(),
                30_000,
            )
            .await
        {
            Ok(Some(lease)) => {
                process(&db, &lease).await;
            }
            Ok(None) => {
                tokio::select! { ()=tokio::time::sleep(Duration::from_millis(200))=>{}, _=shutdown.changed()=>{} }
            }
            Err(error) => {
                tracing::error!(%error,"archive claim failed");
                tokio::select! { ()=tokio::time::sleep(Duration::from_millis(200))=>{}, _=shutdown.changed()=>{} }
            }
        }
    }
}

async fn process(db: &Database, lease: &listmngr_db::mail_queue::Lease) {
    if let crate::heartbeat::Outcome::Completed(Err(error)) = crate::heartbeat::run_while_renewing(
        db,
        lease,
        30_000,
        listmngr_archive::process_live(db, lease),
    )
    .await
    {
        tracing::warn!(%error,"archive indexing failed; retrying");
        let _ = db
            .mail_queue()
            .live()
            .retry(
                lease,
                chrono::Utc::now().timestamp_millis(),
                5000,
                "archive indexing error",
            )
            .await;
    }
}
