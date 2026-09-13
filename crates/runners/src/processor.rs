//! The `in` queue processor: claims durable inbound submissions, applies
//! inbound posting policy, and durably transitions each to accept/hold/
//! reject/discard.
//!
//! Transitions are atomic, with audit, exactly like the tested
//! `listmngr_db::mail_queue`/`moderation` primitives.
use crate::MailRoleConfig;
use crate::policy_facts::gather_context;
use listmngr_core::{Config, ListId, ResponseAction};
use listmngr_db::Database;
use listmngr_db::autoresponse::ResponseKind;
use listmngr_db::mail_queue::{AcceptEffects, AcknowledgeRequest, ChildJob, Lease, Queue};
use listmngr_db::moderation::{PostRefusal, Refusal};
use listmngr_pipeline::handlers::{Effect, FanOut};
use listmngr_pipeline::{Disposition, decide_posting_traced};
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
    /// The `dmarc-mitigation` rule tagged the post.
    dmarc_mitigate: bool,
    /// `validate-authenticity`'s `Authentication-Results` value.
    authentication_results: Option<&'a str>,
}

/// Returns the `listmngr_posts_total` disposition: `accepted`, or
/// `filtered` when a handler refused the post.
async fn accept_post(
    db: &Database,
    config: &Config,
    role: &MailRoleConfig,
    lease: &Lease,
    submission: &Submission<'_>,
) -> Result<&'static str, listmngr_core::Error> {
    let Submission {
        list_id,
        envelope_sender,
        subject,
        raw,
        dmarc_mitigate,
        authentication_results,
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
            db.moderation()
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
                .await?;
            return Ok("filtered");
        }
        Err(error) => return Err(listmngr_core::Error::Validation(error.to_string())),
    };
    let mut plan = None;
    let mut children = Vec::new();
    let mut effects = AcceptEffects {
        list_id,
        record_post: false,
        acknowledge: None,
        dmarc_mitigate,
        authentication_results,
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
    Ok("accepted")
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

/// Owner mail bypasses the posting chain; an invalid forward is shunted.
async fn forward_to_owners(
    db: &Database,
    role: &MailRoleConfig,
    lease: &Lease,
) -> Result<(), listmngr_core::Error> {
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
    result
}

/// The list's automatic response for `kind`, sent to the writer when the
/// message allows a reply at all: an automatic or null-sender message is
/// never answered, which is also what keeps two responders from looping.
async fn autorespond(
    db: &Database,
    list_id: &ListId,
    kind: ResponseKind,
    envelope_sender: Option<&str>,
    raw: &[u8],
) -> Result<ResponseAction, listmngr_core::Error> {
    let Some(sender) = envelope_sender.filter(|sender| !sender.is_empty()) else {
        return Ok(ResponseAction::None);
    };
    if !listmngr_mail::commands::allows_reply(raw) {
        return Ok(ResponseAction::None);
    }
    db.autoresponse()
        .respond(list_id, kind, sender, chrono::Utc::now().timestamp_millis())
        .await
}

/// `respond_and_discard`: the original ends here, audited like any other
/// discard.
async fn discard_after_response(
    db: &Database,
    lease: &Lease,
    list_id: &ListId,
    envelope_sender: Option<&str>,
) -> Result<(), listmngr_core::Error> {
    db.moderation()
        .live()
        .refuse(
            lease,
            list_id,
            &PostRefusal {
                sender: envelope_sender.unwrap_or(""),
                handler: "replybot",
                reason: "automatic response discards the original",
                action: Refusal::Discard,
                preservable: false,
            },
            chrono::Utc::now().timestamp_millis(),
        )
        .await
}

/// `validate-authenticity`: SPF/DKIM/DMARC against DNS, before the chain
/// so `dmarc-mitigation` sees the From domain's policy.
async fn authenticity_verdict(
    role: &MailRoleConfig,
    raw: &[u8],
    envelope_sender: Option<&str>,
) -> listmngr_mail::authenticity::Verdict {
    match &role.authenticity {
        Some(verifier) => {
            let client = listmngr_mail::authenticity::received_client(raw);
            verifier.verify(raw, envelope_sender, client.as_ref()).await
        }
        None => listmngr_mail::authenticity::Verdict::default(),
    }
}

/// Run the posting chain on an ordinary post and apply its disposition
/// durably; returns the `listmngr_posts_total` label.
async fn admit_post(
    db: &Database,
    config: &Config,
    role: &MailRoleConfig,
    lease: &Lease,
    list_id: &ListId,
    envelope_sender: Option<&str>,
    raw: &[u8],
) -> Result<&'static str, listmngr_core::Error> {
    let subject = listmngr_mail::header_value(raw, "subject").unwrap_or_default();
    let mut ctx = gather_context(db, config, list_id, envelope_sender, raw).await?;
    let verdict = authenticity_verdict(role, raw, envelope_sender).await;
    ctx.sender.dmarc_policy_restrictive = verdict.dmarc_policy_restrictive;
    let outcome = decide_posting_traced(&ctx);
    Ok(match outcome.disposition {
        Disposition::Accept => {
            accept_post(
                db,
                config,
                role,
                lease,
                &Submission {
                    list_id,
                    envelope_sender,
                    subject: &subject,
                    raw,
                    dmarc_mitigate: outcome
                        .tags
                        .iter()
                        .any(|tag| tag == listmngr_pipeline::rules::DMARC_TAG),
                    authentication_results: verdict.header.as_deref(),
                },
            )
            .await?
        }
        Disposition::Hold(reason) => {
            db.moderation()
                .live()
                .hold(
                    lease,
                    list_id,
                    envelope_sender.unwrap_or(""),
                    &subject,
                    &reason,
                    chrono::Utc::now().timestamp_millis(),
                )
                .await?;
            "held"
        }
        // Mailman's reject chain bounces the post back to its author with the
        // rule's reason; discard is silent. Both leave a `post.*` audit event.
        Disposition::Reject(reason) => {
            refuse_by_chain(
                db,
                lease,
                list_id,
                envelope_sender,
                &reason,
                Refusal::Reject,
            )
            .await?;
            "rejected"
        }
        Disposition::Discard(reason) => {
            refuse_by_chain(
                db,
                lease,
                list_id,
                envelope_sender,
                &reason,
                Refusal::Discard,
            )
            .await?;
            "discarded"
        }
    })
}

async fn process_one(
    db: &Database,
    config: &Config,
    role: &MailRoleConfig,
    lease: &Lease,
) -> Result<(), listmngr_core::Error> {
    let message = db.mail_queue().live().message(lease.job.message_id).await?;
    let context = parsed_context(&message.context);
    let metrics = listmngr_core::metrics::global();
    let list_id: ListId = context["list_id"]
        .as_str()
        .ok_or_else(|| listmngr_core::Error::Validation("submission missing list_id".into()))?
        .parse()?;
    let envelope_sender = context["envelope_sender"].as_str().map(str::to_owned);
    // Mailman's `replybot` answers first; `respond_and_discard` ends the
    // message here, before it is forwarded, run or admitted.
    let kind = if context["owner_route"] == true {
        ResponseKind::Owner
    } else if context.get("subscription_command").is_some() {
        ResponseKind::Requests
    } else {
        ResponseKind::Postings
    };
    if autorespond(db, &list_id, kind, envelope_sender.as_deref(), &message.raw).await?
        == ResponseAction::RespondAndDiscard
    {
        discard_after_response(db, lease, &list_id, envelope_sender.as_deref()).await?;
        metrics.posts.inc("discarded");
        return Ok(());
    }
    if kind == ResponseKind::Owner {
        forward_to_owners(db, role, lease).await?;
        metrics.posts.inc("owner");
        return Ok(());
    }
    if kind == ResponseKind::Requests {
        db.workflows()
            .live()
            .request_from_lease(lease, chrono::Utc::now().timestamp_millis())
            .await?;
        metrics.posts.inc("command");
        return Ok(());
    }
    let disposition = admit_post(
        db,
        config,
        role,
        lease,
        &list_id,
        envelope_sender.as_deref(),
        &message.raw,
    )
    .await?;
    metrics.posts.inc(disposition);
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
                    listmngr_core::metrics::global().posts.inc("failed");
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
