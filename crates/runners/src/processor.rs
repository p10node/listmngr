//! The `in` queue processor: claims durable inbound submissions, applies
//! inbound posting policy, and durably transitions each to accept/hold/
//! reject/discard.
//!
//! Transitions are atomic, with audit, exactly like the tested
//! `listmngr_db::mail_queue`/`moderation` primitives.
use crate::MailRoleConfig;
use crate::policy_facts::{gather_context, resolve_recipients};
use listmngr_core::{Config, ListId};
use listmngr_db::Database;
use listmngr_db::mail_queue::{ChildJob, Lease, Queue};
use listmngr_pipeline::{Disposition, decide_posting};
use serde_json::Value;
use std::time::Duration;
use tokio::sync::watch;

const IDLE_POLL: Duration = Duration::from_millis(200);
const RETRY_BACKOFF_MS: i64 = 5_000;

fn parsed_context(context: &str) -> Value {
    serde_json::from_str(context).unwrap_or(Value::Null)
}

async fn process_one(
    db: &Database,
    config: &Config,
    role: &MailRoleConfig,
    lease: &Lease,
) -> Result<(), listmngr_core::Error> {
    let message = db.mail_queue().message(lease.job.message_id).await?;
    let context = parsed_context(&message.context);
    let list_id: ListId = context["list_id"]
        .as_str()
        .ok_or_else(|| listmngr_core::Error::Validation("submission missing list_id".into()))?
        .parse()?;
    let envelope_sender = context["envelope_sender"].as_str().map(str::to_owned);
    let subject = listmngr_mail::header_value(&message.raw, "subject").unwrap_or_default();

    let ctx = gather_context(
        db,
        config,
        &list_id,
        envelope_sender.as_deref(),
        &message.raw,
    )
    .await?;
    match decide_posting(&ctx) {
        Disposition::Accept => {
            let recipients =
                resolve_recipients(db, &list_id, envelope_sender.as_deref().unwrap_or("")).await?;
            db.mail_queue()
                .complete_with_children(
                    lease,
                    chrono::Utc::now().timestamp_millis(),
                    &[ChildJob {
                        queue: Queue::Out,
                        max_attempts: role.out_max_attempts,
                        recipients,
                    }],
                )
                .await?;
        }
        Disposition::Hold(reason) => {
            db.moderation()
                .hold(
                    lease,
                    &list_id,
                    envelope_sender.as_deref().unwrap_or(""),
                    &subject,
                    &reason,
                    chrono::Utc::now().timestamp_millis(),
                )
                .await?;
        }
        Disposition::Reject(_) | Disposition::Discard(_) => {
            db.mail_queue()
                .ack(lease, chrono::Utc::now().timestamp_millis())
                .await?;
        }
    }
    Ok(())
}

/// Run the `in` processor until `shutdown` is signalled.
///
/// Claims are bounded to `worker`'s own leases; a processing error retries with backoff (and
/// eventually shunts, per the existing queue budget semantics) rather than
/// silently dropping the submission.
pub async fn run(
    db: Database,
    config: Config,
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
            .claim(Queue::In, &worker, now_ms, role.in_lease_ms)
            .await
        {
            Ok(Some(lease)) => {
                if let crate::heartbeat::Outcome::Completed(Err(error)) =
                    crate::heartbeat::run_while_renewing(
                        &db,
                        &lease,
                        role.in_lease_ms,
                        process_one(&db, &config, &role, &lease),
                    )
                    .await
                {
                    tracing::warn!(worker, %error, "in-processor: submission processing failed; retrying");
                    let _ = db
                        .mail_queue()
                        .retry(
                            &lease,
                            chrono::Utc::now().timestamp_millis(),
                            RETRY_BACKOFF_MS,
                            "processing error",
                        )
                        .await;
                }
            }
            Ok(None) => wait_or_shutdown(&mut shutdown).await,
            Err(error) => {
                tracing::error!(worker, %error, "in-processor: claim failed");
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
