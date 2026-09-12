//! The `in` queue processor: claims durable inbound submissions, applies
//! inbound posting policy, and durably transitions each to accept/hold/
//! reject/discard.
//!
//! Transitions are atomic, with audit, exactly like the tested
//! `listmngr_db::mail_queue`/`moderation` primitives.
use crate::MailRoleConfig;
use crate::policy_facts::gather_context;
use listmngr_core::{Config, ListId};
use listmngr_db::Database;
use listmngr_db::mail_queue::{AcceptEffects, AcknowledgeRequest, ChildJob, Lease, Queue};
use listmngr_db::moderation::{PostRefusal, Refusal};
use listmngr_pipeline::handlers::{Effect, FanOut};
use listmngr_pipeline::{Disposition, decide_posting};
use serde_json::Value;
use std::time::Duration;
use tokio::sync::watch;

const IDLE_POLL: Duration = Duration::from_millis(200);

fn parsed_context(context: &str) -> Value {
    serde_json::from_str(context).unwrap_or(Value::Null)
}

/// Fan an accepted post out as the list's posting pipeline directs: the
/// `member-recipients` effect resolves the delivery roster and each `to-*`
/// effect becomes a child job, all bound in the transaction that acknowledges
/// the inbound job. A handler that ends the pipeline with a disposition
/// (the content filter's `filter_action`) is applied durably instead; any
/// other pipeline refusal shunts rather than delivers.
/// One accepted submission as the runner knows it.
struct Submission<'a> {
    list_id: &'a ListId,
    envelope_sender: Option<&'a str>,
    subject: &'a str,
    raw: &'a [u8],
}

async fn accept_post(
    db: &Database,
    config: &Config,
    role: &MailRoleConfig,
    lease: &Lease,
    submission: &Submission<'_>,
) -> Result<(), listmngr_core::Error> {
    let Submission {
        list_id,
        envelope_sender,
        subject,
        raw,
    } = *submission;
    let list = db.lists().get(list_id).await?;
    let data = match listmngr_mail::handlers::plan(raw, &list, &lease.job.message_id.0.to_string())
    {
        Ok(data) => data,
        Err(listmngr_mail::Error::Refused {
            handler,
            reason,
            refusal,
        }) => {
            return db
                .moderation()
                .live()
                .refuse(
                    lease,
                    list_id,
                    &PostRefusal {
                        sender: envelope_sender.unwrap_or(""),
                        handler,
                        reason: &reason,
                        action: refusal,
                        preservable: config.mailman.filtered_messages_are_preservable,
                    },
                    chrono::Utc::now().timestamp_millis(),
                )
                .await;
        }
        Err(error) => return Err(listmngr_core::Error::Validation(error.to_string())),
    };
    let mut plan = None;
    let mut children = Vec::new();
    let mut effects = AcceptEffects {
        list_id,
        record_post: false,
        acknowledge: None,
    };
    for effect in &data.effects {
        match effect {
            Effect::RecordPost => effects.record_post = true,
            Effect::Acknowledge => {
                effects.acknowledge =
                    envelope_sender.map(|sender| AcknowledgeRequest { sender, subject });
            }
            Effect::PlanRecipients => {
                plan = Some(
                    db.mail_queue()
                        .plan_recipients(list_id, envelope_sender.unwrap_or(""), raw)
                        .await?,
                );
            }
            Effect::Enqueue(FanOut::Out) => children.push(ChildJob {
                queue: Queue::Out,
                max_attempts: role.out_max_attempts,
                recipients: plan
                    .as_ref()
                    .ok_or_else(|| {
                        listmngr_core::Error::Validation(
                            "pipeline enqueued outgoing mail before resolving recipients".into(),
                        )
                    })?
                    .emails(),
            }),
            Effect::Enqueue(FanOut::Digest) => children.push(ChildJob {
                queue: Queue::Digest,
                max_attempts: role.out_max_attempts,
                recipients: Vec::new(),
            }),
            Effect::Enqueue(FanOut::Archive) => children.push(ChildJob {
                queue: Queue::Archive,
                max_attempts: 5,
                recipients: Vec::new(),
            }),
        }
    }
    db.mail_queue()
        .live()
        .complete_accepted(
            lease,
            chrono::Utc::now().timestamp_millis(),
            &children,
            plan.as_ref(),
            Some(&effects),
        )
        .await?;
    Ok(())
}

async fn refuse_by_chain(
    db: &Database,
    lease: &Lease,
    list_id: &ListId,
    envelope_sender: Option<&str>,
    reason: &str,
    action: Refusal,
) -> Result<(), listmngr_core::Error> {
    db.moderation()
        .live()
        .refuse(
            lease,
            list_id,
            &PostRefusal {
                sender: envelope_sender.unwrap_or(""),
                handler: "chain",
                reason,
                action,
                preservable: false,
            },
            chrono::Utc::now().timestamp_millis(),
        )
        .await
}

async fn process_one(
    db: &Database,
    config: &Config,
    role: &MailRoleConfig,
    lease: &Lease,
) -> Result<(), listmngr_core::Error> {
    let message = db.mail_queue().live().message(lease.job.message_id).await?;
    let context = parsed_context(&message.context);
    if context["owner_route"] == true {
        let result = db
            .owner_mail()
            .live()
            .forward(
                lease,
                role.out_max_attempts,
                chrono::Utc::now().timestamp_millis(),
            )
            .await;
        if let Err(listmngr_core::Error::Validation(reason)) = &result {
            return db
                .mail_queue()
                .live()
                .shunt(lease, chrono::Utc::now().timestamp_millis(), reason)
                .await
                .map(|_| ());
        }
        return result;
    }
    if context.get("subscription_command").is_some() {
        return db
            .workflows()
            .live()
            .request_from_lease(lease, chrono::Utc::now().timestamp_millis())
            .await;
    }
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
            accept_post(
                db,
                config,
                role,
                lease,
                &Submission {
                    list_id: &list_id,
                    envelope_sender: envelope_sender.as_deref(),
                    subject: &subject,
                    raw: &message.raw,
                },
            )
            .await?;
        }
        Disposition::Hold(reason) => {
            db.moderation()
                .live()
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
        // Mailman's reject chain bounces the post back to its author with the
        // rule's reason; discard is silent. Both leave a `post.*` audit event.
        Disposition::Reject(reason) => {
            refuse_by_chain(
                db,
                lease,
                &list_id,
                envelope_sender.as_deref(),
                &reason,
                Refusal::Reject,
            )
            .await?;
        }
        Disposition::Discard(reason) => {
            refuse_by_chain(
                db,
                lease,
                &list_id,
                envelope_sender.as_deref(),
                &reason,
                Refusal::Discard,
            )
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
            .live()
            .claim(Queue::In, &worker, now_ms, role.in_lease_ms)
            .await
        {
            Ok(Some(lease)) => {
                if let crate::heartbeat::Outcome::Completed(Err(error)) =
                    crate::heartbeat::run_while_renewing(
                        &db,
                        &lease,
                        role.in_lease_ms,
                        // Boxed: the admission path carries every disposition's
                        // future; keep the runner's own frame small.
                        Box::pin(process_one(&db, &config, &role, &lease)),
                    )
                    .await
                {
                    tracing::warn!(worker, %error, "in-processor: submission processing failed; retrying");
                    let _ = db
                        .mail_queue()
                        .live()
                        .retry(
                            &lease,
                            chrono::Utc::now().timestamp_millis(),
                            role.backoff.delay_ms(lease.job.attempts),
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
