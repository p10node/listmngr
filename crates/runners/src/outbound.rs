//! The `out` queue processor: real SMTP delivery to a configured relay, with
//! durable per-recipient outcomes so a retried job never resends an
//! already-`Sent` recipient, and an `Ambiguous` outcome is never treated as
//! a known success.
#[cfg(test)]
#[path = "bounce_notice_tests.rs"]
mod bounce_notice_tests;
#[cfg(test)]
#[path = "dkim_tests.rs"]
mod dkim_tests;
#[cfg(test)]
#[path = "dsn_issuance_tests.rs"]
mod dsn_issuance_tests;
#[cfg(test)]
#[path = "outbound_durability_tests.rs"]
mod durability_tests;
#[cfg(test)]
#[path = "owner_tests.rs"]
mod owner_tests;
#[cfg(test)]
#[path = "signing_failure_tests.rs"]
mod signing_failure_tests;
#[cfg(test)]
#[path = "smtp_bounce_tests.rs"]
mod smtp_bounce_tests;
#[cfg(test)]
#[path = "outbound_tests.rs"]
mod tests;
#[cfg(test)]
#[path = "workflow_tests.rs"]
mod workflow_tests;

#[cfg(test)]
#[path = "smtp_auth_config_tests.rs"]
mod smtp_auth_config_tests;
#[cfg(test)]
#[path = "smtp_auth_failure_tests.rs"]
mod smtp_auth_failure_tests;
#[cfg(test)]
#[path = "smtp_auth_tests.rs"]
mod smtp_auth_tests;
#[cfg(test)]
#[path = "starttls_tests.rs"]
mod starttls_tests;
use crate::MailRoleConfig;
use listmngr_core::ListId;
use listmngr_db::Database;
use listmngr_db::mail_queue::{Lease, Queue, RecipientOutcome};
use listmngr_mail::handlers::{Target, cook_for_site};
#[cfg(test)]
use listmngr_mail::smtp::send_secure;
use listmngr_mail::smtp::{RecipientStatus, SmtpClientConfig, send_secure_with_envid};
use std::time::Duration;
use tokio::sync::watch;

const IDLE_POLL: Duration = Duration::from_millis(200);
const RETRY_BACKOFF_MS: i64 = 10_000;

#[derive(Debug)]
pub enum PrepareError {
    Invalid,
    Dependency,
}

/// One delivery as the SMTP loop needs it.
#[derive(Debug)]
struct Prepared {
    cooked: Vec<u8>,
    mail_from: String,
    /// The list, when this is a subscriber copy of a personalized list:
    /// every recipient then gets their own transaction and headers.
    personalized: Option<listmngr_core::MailingList>,
}

/// Fail closed: invalid context/cooking is shunted; dependency failure retries.
pub async fn prepare(
    db: &Database,
    raw: &[u8],
    context: &str,
    delivery_id: uuid::Uuid,
) -> Result<(Vec<u8>, String), PrepareError> {
    let prepared = prepare_post(db, raw, context, delivery_id, false).await?;
    Ok((prepared.cooked, prepared.mail_from))
}

async fn prepare_post(
    db: &Database,
    raw: &[u8],
    context: &str,
    delivery_id: uuid::Uuid,
    individual: bool,
) -> Result<Prepared, PrepareError> {
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
    // The outgoing copy is the pipeline up to `to-outgoing` (delivery-only
    // DMARC mitigation included); the digest copy stops at `to-digest`.
    let target = if individual {
        Target::Out
    } else {
        Target::Digest
    };
    let cooked = cook_for_site(target, raw, &list, &delivery_id.to_string(), db.base_url())
        .map_err(|_| PrepareError::Invalid)?;
    if !individual {
        return Ok(Prepared {
            cooked,
            mail_from: list_id.bounces_address(),
            personalized: None,
        });
    }
    // Mailman decorates at delivery: the archive and digest copies were
    // taken above, only subscribers see the list header and footer.
    let decorated = decorate_for_delivery(db, &list, &cooked).await?;
    let personalized =
        (list.alter_messages.personalize != listmngr_core::Personalization::None).then_some(list);
    Ok(Prepared {
        cooked: decorated,
        mail_from: list_id.bounces_address(),
        personalized,
    })
}

/// Expand and add the list's `list:member:regular:header`/`footer`.
/// Template resolution needs the database; a broken template already fell
/// back inside the repository, so only a lost connection is a dependency
/// failure here.
async fn decorate_for_delivery(
    db: &Database,
    list: &listmngr_core::MailingList,
    cooked: &[u8],
) -> Result<Vec<u8>, PrepareError> {
    let placeholders = listmngr_mail::templates::list_placeholders(list);
    let mut texts = Vec::with_capacity(2);
    for name in ["list:member:regular:header", "list:member:regular:footer"] {
        let resolved = db
            .templates()
            .resolve(name, list, &list.preferred_language)
            .await
            .map_err(|_| PrepareError::Dependency)?;
        texts.push(listmngr_mail::templates::expand(
            &resolved.body,
            &placeholders,
        ));
    }
    listmngr_mail::decorate::decorate(cooked, &texts[0], &texts[1])
        .map_err(|_| PrepareError::Invalid)
}

/// The subscriber copy of a post: the pipeline up to `to-outgoing`, then
/// delivery decoration. Exposed for tests of the delivery projection.
/// # Errors
/// Returns [`PrepareError`] for an invalid context or message, or a lost
/// database.
pub async fn prepare_individual(
    db: &Database,
    raw: &[u8],
    context: &str,
    delivery_id: uuid::Uuid,
) -> Result<(Vec<u8>, String), PrepareError> {
    let prepared = prepare_post(db, raw, context, delivery_id, true).await?;
    Ok((prepared.cooked, prepared.mail_from))
}

/// The signed bytes of a delivery: one shared copy, or — for a personalized
/// list — one copy per pending recipient. A failure has already transitioned
/// the job when this returns `None`.
async fn signed_copies(
    db: &Database,
    role: &MailRoleConfig,
    lease: &Lease,
    prepared: Prepared,
    pending: &[String],
) -> Option<(Vec<u8>, String, Option<Vec<Vec<u8>>>)> {
    let Prepared {
        cooked,
        mail_from,
        personalized,
    } = prepared;
    if let Some(list) = &personalized {
        let copies = personalized_copies(db, role, list, &cooked, pending).await;
        let copies = local_delivery_result(db, lease, copies).await?;
        return Some((cooked, mail_from, Some(copies)));
    }
    let signed =
        local_delivery_result(db, lease, sign_delivery(db, role, lease, cooked).await).await?;
    Some((signed, mail_from, None))
}

/// One personalized, signed copy per pending recipient, in order.
async fn personalized_copies(
    db: &Database,
    role: &MailRoleConfig,
    list: &listmngr_core::MailingList,
    cooked: &[u8],
    pending: &[String],
) -> Result<Vec<Vec<u8>>, PrepareError> {
    let mut copies = Vec::with_capacity(pending.len());
    for recipient in pending {
        let copy = personalize_for(db, list, cooked, recipient).await?;
        let signed = if role.dkim.is_empty() {
            copy
        } else {
            role.dkim
                .sign(list.id.mail_host(), copy)
                .map_err(|_| PrepareError::Invalid)?
        };
        copies.push(signed);
    }
    Ok(copies)
}

/// The recipient's own copy of a personalized delivery: the RFC 8058
/// one-click unsubscribe pair when the site has a base URL and the
/// recipient is a member. (Further personalization — VERP, `$user_*`
/// placeholders, `To:` rewriting — belongs to the personalize work package.)
async fn personalize_for(
    db: &Database,
    list: &listmngr_core::MailingList,
    cooked: &[u8],
    recipient: &str,
) -> Result<Vec<u8>, PrepareError> {
    let Some(base_url) = db.base_url() else {
        return Ok(cooked.to_vec());
    };
    let url = db
        .one_click()
        .url_for(
            base_url,
            &list.id,
            recipient,
            chrono::Utc::now().timestamp(),
        )
        .await
        .map_err(|error| lookup_error(&error))?;
    url.map_or_else(
        || Ok(cooked.to_vec()),
        |url| {
            listmngr_mail::personalize::one_click_unsubscribe(cooked, list, &url)
                .map_err(|_| PrepareError::Invalid)
        },
    )
}

/// The bytes and envelope sender of a delivery, for tests of the projection.
#[cfg(test)]
async fn prepare_delivery(
    db: &Database,
    lease: &Lease,
    raw: &[u8],
    context: &str,
) -> Option<(Vec<u8>, String)> {
    prepare_delivery_full(db, lease, raw, context)
        .await
        .map(|prepared| (prepared.cooked, prepared.mail_from))
}

async fn prepare_delivery_full(
    db: &Database,
    lease: &Lease,
    raw: &[u8],
    context: &str,
) -> Option<Prepared> {
    let unpersonalized = |cooked: Vec<u8>, mail_from: String| Prepared {
        cooked,
        mail_from,
        personalized: None,
    };
    let result = async {
        if db
            .owner_mail()
            .is_delivery(lease.job.id)
            .await
            .map_err(|_| PrepareError::Dependency)?
        {
            let cooked = listmngr_mail::owner::cook(raw).map_err(|_| PrepareError::Invalid)?;
            return Ok(unpersonalized(cooked, String::new()));
        }
        if db
            .workflows()
            .is_notice(lease.job.id)
            .await
            .map_err(|_| PrepareError::Dependency)?
        {
            return Ok(unpersonalized(raw.to_vec(), String::new()));
        }
        if db
            .digests()
            .is_delivery(lease.job.id)
            .await
            .map_err(|_| PrepareError::Dependency)?
        {
            let context: serde_json::Value =
                serde_json::from_str(context).map_err(|_| PrepareError::Invalid)?;
            let list: ListId = context["list_id"]
                .as_str()
                .ok_or(PrepareError::Invalid)?
                .parse()
                .map_err(|_| PrepareError::Invalid)?;
            db.lists()
                .get(&list)
                .await
                .map_err(|_| PrepareError::Dependency)?;
            Ok(unpersonalized(raw.to_vec(), list.bounces_address()))
        } else {
            prepare_post(db, raw, context, lease.job.id.0, true).await
        }
    }
    .await;
    let prepared = match result {
        Ok(prepared) => prepared,
        Err(error) => {
            let now = chrono::Utc::now().timestamp_millis();
            let result = match error {
                PrepareError::Invalid => {
                    db.mail_queue()
                        .live()
                        .shunt(lease, now, "invalid outgoing context or headers")
                        .await
                }
                PrepareError::Dependency => {
                    db.mail_queue()
                        .live()
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
                .live()
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
            .live()
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
    let message = match db.mail_queue().message(lease.job.message_id).await {
        Ok(message) => message,
        Err(error) => {
            local_delivery_result::<Vec<u8>>(db, &lease, Err(lookup_error(&error))).await;
            return;
        }
    };
    let Some(prepared) = prepare_delivery_full(db, &lease, &message.raw, &message.context).await
    else {
        return;
    };
    let Some((cooked, mail_from, variants)) =
        signed_copies(db, role, &lease, prepared, &pending).await
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
            .live()
            .retry(
                &lease,
                chrono::Utc::now().timestamp_millis(),
                RETRY_BACKOFF_MS,
                "relay unreachable",
            )
            .await;
        return;
    };
    let digest = match db.digests().is_delivery(lease.job.id).await {
        Ok(digest) => digest,
        Err(error) => {
            local_delivery_result::<Vec<u8>>(db, &lease, Err(lookup_error(&error))).await;
            return;
        }
    };
    let ordinary = !mail_from.is_empty() && !digest;
    let issuer = if ordinary {
        role.dsn_issuer.as_ref()
    } else {
        None
    };
    let envids = match db
        .mail_queue()
        .live()
        .begin_delivery_with_dsn(
            &lease,
            chrono::Utc::now().timestamp_millis(),
            &pending,
            issuer,
        )
        .await
    {
        Ok(envids) => envids,
        Err(error) => {
            if issuer.is_none() {
                tracing::error!(%error, "out-processor: durable attempt reservation failed; SMTP not started");
                return;
            }
            local_delivery_result::<Vec<u8>>(db, &lease, Err(lookup_error(&error))).await;
            return;
        }
    };
    let envelope_sender = if mail_from.is_empty() {
        None
    } else {
        Some(mail_from.as_str())
    };
    let results = send_transactions_with_envid(
        stream,
        role,
        envelope_sender,
        &pending,
        &cooked,
        &envids,
        variants.as_deref().map(<[Vec<u8>]>::as_ref),
    )
    .await;
    finish_delivery(db, &lease, &pending, &results).await;
}

// The complete recipient set is durably reserved before any SMTP command.
// Cancellation therefore quarantines even not-yet-attempted recipients; it
// never silently resends a recipient accepted in an earlier session.
#[cfg(test)]
async fn send_transactions(
    stream: tokio::net::TcpStream,
    role: &MailRoleConfig,
    sender: Option<&str>,
    pending: &[String],
    cooked: &[u8],
) -> Vec<RecipientStatus> {
    send_transactions_with_envid(stream, role, sender, pending, cooked, &[], None).await
}
/// `variants`, when given, holds one message per pending recipient (in
/// order) and forces one transaction per recipient.
async fn send_transactions_with_envid(
    stream: tokio::net::TcpStream,
    role: &MailRoleConfig,
    sender: Option<&str>,
    pending: &[String],
    cooked: &[u8],
    envids: &[String],
    variants: Option<&[Vec<u8>]>,
) -> Vec<RecipientStatus> {
    let config = SmtpClientConfig {
        local_hostname: role.local_hostname.clone(),
        command_timeout: role.command_timeout,
    };
    let width = if variants.is_some() || (role.smtp_single_recipient && sender.is_some()) {
        1
    } else {
        pending.len().max(1)
    };
    let mut first_stream = Some(stream);
    let mut results = Vec::with_capacity(pending.len());
    for (index, recipients) in pending.chunks(width).enumerate() {
        let stream = if let Some(stream) = first_stream.take() {
            stream
        } else if let Ok(Ok(stream)) = tokio::time::timeout(
            role.command_timeout,
            tokio::net::TcpStream::connect(role.smtp_relay),
        )
        .await
        {
            stream
        } else {
            results.extend(vec![
                RecipientStatus::TransientFailure(
                    "relay unreachable".into()
                );
                recipients.len()
            ]);
            continue;
        };
        let bytes = variants
            .and_then(|copies| copies.get(index))
            .map_or(cooked, Vec::as_slice);
        match send_secure_with_envid(
            stream,
            &config,
            &role.smtp_tls,
            sender,
            recipients,
            bytes,
            envids.get(index).map(String::as_str),
        )
        .await
        {
            Ok(outcome) => results.extend(outcome.results),
            // Errors are pre-DATA negotiation failures, not uncertain acceptance.
            Err(_) => results.extend(vec![
                RecipientStatus::TransientFailure(
                    "relay greeting or TLS failure".into()
                );
                recipients.len()
            ]),
        }
    }
    results
}

// Resolve signing authority from the stored job-to-message association, not
// MIME From, caller-supplied context, or mutable public lease metadata.
async fn sign_delivery(
    db: &Database,
    role: &MailRoleConfig,
    lease: &Lease,
    cooked: Vec<u8>,
) -> Result<Vec<u8>, PrepareError> {
    if role.dkim.is_empty() {
        return Ok(cooked);
    }
    let stored = db
        .mail_queue()
        .job(lease.job.id)
        .await
        .map_err(|error| lookup_error(&error))?;
    let message = db
        .mail_queue()
        .message(stored.message_id)
        .await
        .map_err(|error| lookup_error(&error))?;
    let context: serde_json::Value =
        serde_json::from_str(&message.context).map_err(|_| PrepareError::Invalid)?;
    let id: ListId = context["list_id"]
        .as_str()
        .ok_or(PrepareError::Invalid)?
        .parse()
        .map_err(|_| PrepareError::Invalid)?;
    let list = db
        .lists()
        .get(&id)
        .await
        .map_err(|error| lookup_error(&error))?;
    role.dkim
        .sign(list.id.mail_host(), cooked)
        .map_err(|_| PrepareError::Invalid)
}

const fn lookup_error(error: &listmngr_core::Error) -> PrepareError {
    match error {
        listmngr_core::Error::Database(_) => PrepareError::Dependency,
        _ => PrepareError::Invalid,
    }
}

async fn local_delivery_result<T>(
    db: &Database,
    lease: &Lease,
    result: Result<T, PrepareError>,
) -> Option<T> {
    match result {
        Ok(bytes) => Some(bytes),
        Err(error) => {
            let now = chrono::Utc::now().timestamp_millis();
            let transition = match error {
                PrepareError::Dependency => {
                    db.mail_queue()
                        .live()
                        .retry(lease, now, RETRY_BACKOFF_MS, "local delivery lookup failed")
                        .await
                }
                PrepareError::Invalid => {
                    db.mail_queue()
                        .live()
                        .shunt(lease, now, "invalid local delivery or signing context")
                        .await
                }
            };
            if let Err(error) = transition {
                tracing::error!(%error, "out-processor: local failure transition failed");
            }
            None
        }
    }
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
    let mut smtp = Vec::new();
    for (email, status) in pending.iter().zip(results.iter()) {
        let (outcome, detail) = match status {
            RecipientStatus::Sent => (RecipientOutcome::Sent, "2.1.5 sent"),
            RecipientStatus::RemotePermanentFailure { failure, detail } => {
                smtp.push((email.clone(), *failure));
                (RecipientOutcome::Failed, detail.as_str())
            }
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
        .live()
        .finish_delivery_with_smtp(lease, now_ms, &outcomes, RETRY_BACKOFF_MS, &smtp)
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
            .live()
            .claim(Queue::Out, &worker, now_ms, role.out_lease_ms)
            .await
        {
            Ok(Some(lease)) => {
                let _ = crate::heartbeat::run_while_renewing(
                    &db,
                    &lease,
                    role.out_lease_ms,
                    // Boxed: the delivery path carries the personalized and
                    // shared variants; keep the runner's own frame small.
                    Box::pin(deliver_one(&db, &role, lease.clone())),
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
