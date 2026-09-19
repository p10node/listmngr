//! Durable archive queue consumer. Parsing/indexing and queue ack are
//! replay-safe; the search index follows each archived post in batches.
use listmngr_archive::search::Writer;
use listmngr_db::{Database, mail_queue::Queue};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;

/// The search index's writer, shared with nobody else in the process.
pub type SharedWriter = Arc<Mutex<Writer>>;

/// Commit after this many changes, or after a change has waited this long.
const COMMIT_BATCH: usize = 100;
const COMMIT_AGE: Duration = Duration::from_secs(2);

pub async fn run(
    db: Database,
    index: Option<SharedWriter>,
    archivers: listmngr_archive::archivers::Settings,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        if *shutdown.borrow() {
            if let Some(index) = &index {
                commit(index, true).await;
            }
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
            Ok(Some(lease)) => archive_and_index(&db, index.as_ref(), &archivers, &lease).await,
            Ok(None) => {
                if let Some(index) = &index {
                    commit(index, false).await;
                }
                tokio::select! { ()=tokio::time::sleep(Duration::from_millis(200))=>{}, _=shutdown.changed()=>{} }
            }
            Err(error) => {
                tracing::error!(%error,"archive claim failed");
                tokio::select! { ()=tokio::time::sleep(Duration::from_millis(200))=>{}, _=shutdown.changed()=>{} }
            }
        }
    }
}

/// Archive one leased post and, when it was stored, add it to the index
/// and hand it to the list's remote archivers.
async fn archive_and_index(
    db: &Database,
    index: Option<&SharedWriter>,
    archivers: &listmngr_archive::archivers::Settings,
    lease: &listmngr_db::mail_queue::Lease,
) {
    if process(db, lease).await {
        if let Some(index) = index {
            index_message(db, index, lease.job.message_id).await;
        }
        forward_to_archivers(db, lease.job.message_id, archivers).await;
    }
}

/// Hand one archived post to the list's enabled remote archivers that act
/// outside the database; the names that ran.
///
/// This follows the archive transaction rather than joining it, so a
/// crash between the two loses a forward and never the archived post. A
/// post the archive did not store (an archive policy of `never`) and a
/// list with every archiver off both forward nothing. `mail-archive` is
/// not here: its copy is queued inside the archive's own transaction.
pub async fn forward_to_archivers(
    db: &Database,
    message_id: listmngr_db::mail_queue::MessageId,
    archivers: &listmngr_archive::archivers::Settings,
) -> Vec<&'static str> {
    let Some((list, hash)) = archived_post(db, message_id, archivers).await else {
        return Vec::new();
    };
    match listmngr_archive::archivers::run(db, &list, &hash, archivers).await {
        Ok(names) => names,
        Err(error) => {
            tracing::warn!(%error, list = %list, "archivers failed");
            Vec::new()
        }
    }
}

/// The list and Message-ID-Hash of an archived post, when there is one
/// and any archiver could act on it.
async fn archived_post(
    db: &Database,
    message_id: listmngr_db::mail_queue::MessageId,
    archivers: &listmngr_archive::archivers::Settings,
) -> Option<(listmngr_core::ListId, String)> {
    if archivers.is_empty() {
        return None;
    }
    let row = match db.archive().index_row_for_message(message_id).await {
        Ok(row) => row?,
        Err(error) => {
            tracing::warn!(%error, "archivers: archived post not readable");
            return None;
        }
    };
    match row.list.parse() {
        Ok(list) => Some((list, row.hash)),
        Err(error) => {
            tracing::warn!(%error, "archivers: unreadable list id");
            None
        }
    }
}

/// Archive one leased post; whether it was stored (a failure retries).
async fn process(db: &Database, lease: &listmngr_db::mail_queue::Lease) -> bool {
    match crate::heartbeat::run_while_renewing(
        db,
        lease,
        30_000,
        listmngr_archive::process_live(db, lease),
    )
    .await
    {
        crate::heartbeat::Outcome::Completed(Ok(())) => true,
        crate::heartbeat::Outcome::Completed(Err(error)) => {
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
            false
        }
        _ => false,
    }
}

/// Add the post an archive job stored to the search index and commit when
/// the batch is due. A failure here is logged: the database has the post,
/// and `listmngr archive reindex` rebuilds the index.
pub async fn index_message(
    db: &Database,
    index: &SharedWriter,
    message_id: listmngr_db::mail_queue::MessageId,
) {
    let document = match listmngr_archive::index_document(db, message_id).await {
        Ok(Some(document)) => document,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(%error, "search index: archived post not readable");
            return;
        }
    };
    let index = index.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        let mut writer = index.lock().map_err(|_| "poisoned")?;
        writer.add(&document).map_err(|e| e.to_string())?;
        writer
            .commit_if_due(COMMIT_BATCH, COMMIT_AGE)
            .map_err(|e| e.to_string())
    })
    .await;
    match outcome {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => tracing::warn!(error, "search index: add failed"),
        Err(error) => tracing::warn!(%error, "search index: add task failed"),
    }
}

/// Commit what waits: always on shutdown, otherwise when the batch is due.
async fn commit(index: &SharedWriter, force: bool) {
    let index = index.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        let mut writer = index.lock().map_err(|_| "poisoned".to_owned())?;
        if force {
            writer.commit().map(|()| true).map_err(|e| e.to_string())
        } else {
            writer
                .commit_if_due(1, COMMIT_AGE)
                .map_err(|e| e.to_string())
        }
    })
    .await;
    if let Ok(Err(error)) = outcome {
        tracing::warn!(error, "search index: commit failed");
    }
}
