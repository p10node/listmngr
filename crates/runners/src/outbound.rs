//! The `out` queue processor: real SMTP delivery to a configured relay, with
//! durable per-recipient outcomes so a retried job never resends an
//! already-`Sent` recipient, and an `Ambiguous` outcome is never treated as
//! a known success.
#[cfg(test)]
#[path = "outbound_durability_tests.rs"]
mod durability_tests;
#[cfg(test)]
#[path = "outbound_tests.rs"]
mod tests;

use crate::MailRoleConfig;
use listmngr_core::ListId;
use listmngr_db::Database;
use listmngr_db::mail_queue::{Lease, Queue, RecipientOutcome};
use listmngr_mail::smtp::{RecipientStatus, SmtpClientConfig, send};
use listmngr_pipeline::{ListHeaderInfo, list_headers};
use std::time::Duration;
use tokio::sync::watch;

const IDLE_POLL: Duration = Duration::from_millis(200);
const RETRY_BACKOFF_MS: i64 = 10_000;

enum PrepareError {
    Invalid,
    Dependency,
}

/// Fail closed: invalid context/cooking is shunted; dependency failure retries.
async fn prepare(
    db: &Database,
    raw: &[u8],
    context: &str,
) -> Result<(Vec<u8>, String), PrepareError> {
    let context: serde_json::Value =
        serde_json::from_str(context).map_err(|_| PrepareError::Invalid)?;
    let list_id: ListId = context["list_id"]
        .as_str()
        .ok_or(PrepareError::Invalid)?
        .parse()
        .map_err(|_| PrepareError::Invalid)?;
    let list = db
        .lists()
        .get(&list_id)
        .await
        .map_err(|_| PrepareError::Dependency)?;
    let info = ListHeaderInfo {
        list_id: list_id.to_string(),
        posting_address: list_id.posting_address(),
        subscribe_address: list_id.join_address(),
        unsubscribe_address: list_id.leave_address(),
        archive_url: None,
    };
    let headers = list_headers(&info);
    let cooked = listmngr_mail::cook_headers(raw, Some(&list.subject_prefix), &headers)
        .map_err(|_| PrepareError::Invalid)?;
    Ok((cooked, list_id.bounces_address()))
}

async fn prepare_delivery(
    db: &Database,
    lease: &Lease,
    raw: &[u8],
    context: &str,
) -> Option<(Vec<u8>, String)> {
    let prepared = match prepare(db, raw, context).await {
        Ok(prepared) => prepared,
        Err(error) => {
            let now = chrono::Utc::now().timestamp_millis();
            let result = match error {
                PrepareError::Invalid => {
                    db.mail_queue()
                        .shunt(lease, now, "invalid outgoing context or headers")
                        .await
                }
                PrepareError::Dependency => {
                    db.mail_queue()
                        .retry(lease, now, RETRY_BACKOFF_MS, "list lookup failed")
                        .await
                }
            };
            if let Err(error) = result {
                tracing::error!(%error, "out-processor: preparation failure transition failed");
            }
            return None;
        }
    };
    Some(prepared)
}

async fn pending_for_delivery(db: &Database, lease: &Lease) -> Option<Vec<String>> {
    let pending = match db.mail_queue().pending_recipients(lease.job.id).await {
        Ok(pending) => pending,
        Err(error) => {
            tracing::warn!(%error, "out-processor: pending-recipient lookup failed");
            let _ = db
                .mail_queue()
                .retry(
                    lease,
                    chrono::Utc::now().timestamp_millis(),
                    RETRY_BACKOFF_MS,
                    "pending lookup failed",
                )
                .await;
            return None;
        }
    };
    if pending.is_empty() {
        let _ = db
            .mail_queue()
            .ack(lease, chrono::Utc::now().timestamp_millis())
            .await;
        return None;
    }
    Some(pending)
}

async fn deliver_one(db: &Database, role: &MailRoleConfig, lease: Lease) {
    let Some(pending) = pending_for_delivery(db, &lease).await else {
        return;
    };
    let Ok(message) = db.mail_queue().message(lease.job.message_id).await else {
        let _ = db
            .mail_queue()
            .shunt(
                &lease,
                chrono::Utc::now().timestamp_millis(),
                "durable message missing",
            )
            .await;
        return;
    };
    let Some((cooked, mail_from)) =
        prepare_delivery(db, &lease, &message.raw, &message.context).await
    else {
        return;
    };
    let Ok(Ok(stream)) = tokio::time::timeout(
        role.command_timeout,
        tokio::net::TcpStream::connect(role.smtp_relay),
    )
    .await
    else {
        let _ = db
            .mail_queue()
            .retry(
                &lease,
                chrono::Utc::now().timestamp_millis(),
                RETRY_BACKOFF_MS,
                "relay unreachable",
            )
            .await;
        return;
    };
    let smtp_config = SmtpClientConfig {
        local_hostname: role.local_hostname.clone(),
        command_timeout: role.command_timeout,
    };
    if let Err(error) = db
        .mail_queue()
        .begin_delivery(&lease, chrono::Utc::now().timestamp_millis(), &pending)
        .await
    {
        tracing::error!(%error, "out-processor: durable attempt reservation failed; SMTP not started");
        return;
    }
    let outcome = send(stream, &smtp_config, Some(&mail_from), &pending, &cooked).await;
    let Ok(outcome) = outcome else {
        // send() only returns Err before SMTP negotiation/DATA. Persist a
        // known safe transient, never merely release an unresolved attempt.
        let results =
            vec![RecipientStatus::TransientFailure("relay greeting failure".into()); pending.len()];
        finish_delivery(db, &lease, &pending, &results).await;
        return;
    };
    finish_delivery(db, &lease, &pending, &outcome.results).await;
}

/// Atomically fence recipient outcomes and the job transition. Ambiguous
/// recipients are quarantined, while ordinary transient failures stay pending.
async fn finish_delivery(
    db: &Database,
    lease: &Lease,
    pending: &[String],
    results: &[RecipientStatus],
) {
    let mut outcomes = Vec::new();
    for (email, status) in pending.iter().zip(results.iter()) {
        let (outcome, detail) = match status {
            RecipientStatus::Sent => (RecipientOutcome::Sent, "2.1.5 sent"),
            RecipientStatus::PermanentFailure(detail) => {
                (RecipientOutcome::Failed, detail.as_str())
            }
            RecipientStatus::Ambiguous(detail) => (RecipientOutcome::Ambiguous, detail.as_str()),
            RecipientStatus::TransientFailure(detail) => {
                (RecipientOutcome::Transient, detail.as_str())
            }
        };
        outcomes.push((email.clone(), outcome, detail.to_owned()));
    }
    let now_ms = chrono::Utc::now().timestamp_millis();
    if let Err(error) = db
        .mail_queue()
        .finish_delivery(lease, now_ms, &outcomes, RETRY_BACKOFF_MS)
        .await
    {
        tracing::error!(%error, "out-processor: fenced delivery transaction failed; job not acknowledged");
    }
}

/// Run the `out` processor until `shutdown` is signalled.
pub async fn run(
    db: Database,
    role: MailRoleConfig,
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
            .claim(Queue::Out, &worker, now_ms, role.out_lease_ms)
            .await
        {
            Ok(Some(lease)) => {
                let _ = crate::heartbeat::run_while_renewing(
                    &db,
                    &lease,
                    role.out_lease_ms,
                    deliver_one(&db, &role, lease.clone()),
                )
                .await;
            }
            Ok(None) => wait_or_shutdown(&mut shutdown).await,
            Err(error) => {
                tracing::error!(worker, %error, "out-processor: claim failed");
                wait_or_shutdown(&mut shutdown).await;
            }
        }
    }
}

async fn wait_or_shutdown(shutdown: &mut watch::Receiver<bool>) {
    tokio::select! {
        () = tokio::time::sleep(IDLE_POLL) => {}
        _ = shutdown.changed() => {}
    }
}
