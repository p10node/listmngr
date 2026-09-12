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
#[path = "metrics_tests.rs"]
mod metrics_tests;
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
pub mod tests;
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
use listmngr_mail::handlers::{Target, cook_with};
#[cfg(test)]
use listmngr_mail::smtp::send_secure;
use listmngr_mail::smtp::{RecipientStatus, SmtpClientConfig, send_secure_with_envid};
use std::time::Duration;
use tokio::sync::watch;

const IDLE_POLL: Duration = Duration::from_millis(200);

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
    /// Set when every recipient gets their own transaction: a personalized
    /// list, or a VERP delivery of an ordinary one.
    per_recipient: Option<PerRecipient>,
}

/// Why and how a delivery is split per recipient.
#[derive(Debug)]
struct PerRecipient {
    list: listmngr_core::MailingList,
    /// Mailman's `personalize`: `$user_*` decoration and, for `full`, the
    /// `To:` rewrite. `None` keeps the shared (already decorated) bytes.
    personalization: Option<Personalization>,
    /// Per-recipient VERP envelope senders.
    verp: bool,
}

/// The unexpanded decoration templates, resolved once per delivery and
/// expanded per recipient.
#[derive(Debug)]
struct Personalization {
    mode: listmngr_core::Personalization,
    header: String,
    footer: String,
}

/// One recipient's transaction: its bytes and envelope sender.
#[derive(Debug)]
struct RecipientCopy {
    bytes: Vec<u8>,
    mail_from: Option<String>,
}

/// Fail closed: invalid context/cooking is shunted; dependency failure retries.
pub async fn prepare(
    db: &Database,
    raw: &[u8],
    context: &str,
    delivery_id: uuid::Uuid,
) -> Result<(Vec<u8>, String), PrepareError> {
    let prepared = prepare_post(db, raw, context, delivery_id, false, None).await?;
    Ok((prepared.cooked, prepared.mail_from))
}

async fn prepare_post(
    db: &Database,
    raw: &[u8],
    context: &str,
    delivery_id: uuid::Uuid,
    individual: bool,
    verp: Option<VerpPolicy>,
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
    let authentication_results = context["authentication_results"]
        .as_str()
        .map(str::to_owned);
    let cooked = cook_with(
        target,
        raw,
        &list,
        &delivery_id.to_string(),
        &listmngr_mail::handlers::Admission {
            base_url: db.base_url(),
            dmarc_mitigate: context["dmarc_mitigate"] == true,
            authentication_results: authentication_results.as_deref(),
        },
    )
    .map_err(|_| PrepareError::Invalid)?;
    if !individual {
        return Ok(Prepared {
            cooked,
            mail_from: list_id.bounces_address(),
            per_recipient: None,
        });
    }
    // Mailman decorates at delivery: the archive and digest copies were
    // taken above, only subscribers see the list header and footer. A
    // personalized list expands the templates per recipient instead.
    let (header, footer) = decoration_templates(db, &list).await?;
    let mode = list.alter_messages.personalize;
    let personalized = mode != listmngr_core::Personalization::None;
    let verp = verp.is_some_and(|verp| verp.applies(&list, personalized));
    if !personalized {
        let placeholders = listmngr_mail::templates::list_placeholders(&list);
        let decorated = listmngr_mail::decorate::decorate(
            &cooked,
            &listmngr_mail::templates::expand(&header, &placeholders),
            &listmngr_mail::templates::expand(&footer, &placeholders),
        )
        .map_err(|_| PrepareError::Invalid)?;
        return Ok(Prepared {
            cooked: decorated,
            mail_from: list_id.bounces_address(),
            per_recipient: verp.then_some(PerRecipient {
                list,
                personalization: None,
                verp: true,
            }),
        });
    }
    Ok(Prepared {
        cooked,
        mail_from: list_id.bounces_address(),
        per_recipient: Some(PerRecipient {
            list,
            personalization: Some(Personalization {
                mode,
                header,
                footer,
            }),
            verp,
        }),
    })
}

/// The `[mta]` VERP policy as the out runner applies it.
#[derive(Debug, Clone, Copy)]
pub struct VerpPolicy {
    pub personalized_deliveries: bool,
    pub delivery_interval: u32,
}

impl VerpPolicy {
    /// Mailman: personalized copies are VERP'd when
    /// `verp_personalized_deliveries`; every `verp_delivery_interval`th post
    /// of any list is VERP'd.
    fn applies(self, list: &listmngr_core::MailingList, personalized: bool) -> bool {
        (personalized && self.personalized_deliveries)
            || (self.delivery_interval > 0
                && list.post_id.rem_euclid(i64::from(self.delivery_interval)) == 0)
    }
}

/// The list's `list:member:regular:header`/`footer` templates, unexpanded.
/// Template resolution needs the database; a broken template already fell
/// back inside the repository, so only a lost connection is a dependency
/// failure here.
async fn decoration_templates(
    db: &Database,
    list: &listmngr_core::MailingList,
) -> Result<(String, String), PrepareError> {
    let mut texts = Vec::with_capacity(2);
    for name in ["list:member:regular:header", "list:member:regular:footer"] {
        let resolved = db
            .templates()
            .resolve(name, list, &list.preferred_language)
            .await
            .map_err(|_| PrepareError::Dependency)?;
        texts.push(resolved.body);
    }
    let footer = texts.pop().unwrap_or_default();
    let header = texts.pop().unwrap_or_default();
    Ok((header, footer))
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
    let prepared = prepare_post(db, raw, context, delivery_id, true, None).await?;
    Ok((prepared.cooked, prepared.mail_from))
}

/// The signed bytes of a delivery: one shared copy, or one copy per pending
/// recipient. A failure has already transitioned the job when this returns
/// `None`.
async fn signed_copies(
    db: &Database,
    role: &MailRoleConfig,
    lease: &Lease,
    prepared: Prepared,
    pending: &[String],
) -> Option<(Vec<u8>, String, Option<Vec<RecipientCopy>>)> {
    let Prepared {
        cooked,
        mail_from,
        per_recipient,
    } = prepared;
    if let Some(split) = &per_recipient {
        let copies = recipient_copies(db, role, split, &cooked, pending).await;
        let copies = local_delivery_result(db, role, lease, copies).await?;
        return Some((cooked, mail_from, Some(copies)));
    }
    let signed = local_delivery_result(
        db,
        role,
        lease,
        sign_delivery(db, role, lease, cooked).await,
    )
    .await?;
    Some((signed, mail_from, None))
}

/// One signed copy per pending recipient, in order, with its envelope sender.
async fn recipient_copies(
    db: &Database,
    role: &MailRoleConfig,
    split: &PerRecipient,
    cooked: &[u8],
    pending: &[String],
) -> Result<Vec<RecipientCopy>, PrepareError> {
    let list = &split.list;
    let one_click_signer = match db.base_url() {
        Some(_) if split.personalization.is_some() => Some(
            db.one_click()
                .signer()
                .await
                .map_err(|error| lookup_error(&error))?,
        ),
        _ => None,
    };
    let now_secs = chrono::Utc::now().timestamp();
    let mut copies = Vec::with_capacity(pending.len());
    for recipient in pending {
        let mut bytes = cooked.to_vec();
        if let Some(personalization) = &split.personalization {
            let member = db
                .delivery()
                .recipient(list, recipient)
                .await
                .map_err(|error| lookup_error(&error))?;
            bytes = personalize_for(
                list,
                personalization,
                &bytes,
                recipient,
                member.as_ref(),
                one_click_signer.as_ref().zip(db.base_url()),
                now_secs,
            )?;
        }
        let signed = if role.dkim.is_empty() {
            bytes
        } else {
            role.dkim
                .sign(list.id.mail_host(), bytes)
                .map_err(|_| PrepareError::Invalid)?
        };
        let mail_from = if split.verp {
            listmngr_core::verp::encode(&role.verp_format, &list.id, recipient)
        } else {
            None
        };
        copies.push(RecipientCopy {
            bytes: signed,
            mail_from,
        });
    }
    Ok(copies)
}

/// Mailman's personalized copy: decoration expanded with the member's
/// `$user_*` placeholders, the `To:` rewrite for `full`, and the RFC 8058
/// one-click pair when the site has a base URL and the recipient is a
/// member.
fn personalize_for(
    list: &listmngr_core::MailingList,
    personalization: &Personalization,
    cooked: &[u8],
    recipient: &str,
    member: Option<&listmngr_db::delivery::DeliveryRecipient>,
    one_click: Option<(&listmngr_core::one_click::Signer, &str)>,
    now_secs: i64,
) -> Result<Vec<u8>, PrepareError> {
    use listmngr_mail::personalize;
    let profile = member.map_or_else(
        || personalize::Recipient {
            email: recipient.to_owned(),
            delivered_to: recipient.to_owned(),
            display_name: String::new(),
            language: list.preferred_language.clone(),
        },
        |member| member.profile.clone(),
    );
    let placeholders = personalize::placeholders(list, &profile);
    let mut bytes = listmngr_mail::decorate::decorate(
        cooked,
        &listmngr_mail::templates::expand(&personalization.header, &placeholders),
        &listmngr_mail::templates::expand(&personalization.footer, &placeholders),
    )
    .map_err(|_| PrepareError::Invalid)?;
    if personalization.mode == listmngr_core::Personalization::Full {
        bytes = personalize::rewrite_to(&bytes, &profile).map_err(|_| PrepareError::Invalid)?;
    }
    if let (Some(member), Some((signer, base_url))) = (member, one_click) {
        let url = listmngr_db::one_click::OneClickRepo::url_for_member(
            signer,
            base_url,
            &list.id,
            member.member_id,
            now_secs,
        );
        bytes = personalize::one_click_unsubscribe(&bytes, list, &url)
            .map_err(|_| PrepareError::Invalid)?;
    }
    Ok(bytes)
}

/// The bytes and envelope sender of a delivery, for tests of the projection.
#[cfg(test)]
async fn prepare_delivery(
    db: &Database,
    lease: &Lease,
    raw: &[u8],
    context: &str,
) -> Option<(Vec<u8>, String)> {
    let role = MailRoleConfig::from_core(&listmngr_core::Config::default()).expect("default role");
    prepare_delivery_full(db, &role, lease, raw, context)
        .await
        .map(|prepared| (prepared.cooked, prepared.mail_from))
}

async fn prepare_delivery_full(
    db: &Database,
    role: &MailRoleConfig,
    lease: &Lease,
    raw: &[u8],
    context: &str,
) -> Option<Prepared> {
    let unpersonalized = |cooked: Vec<u8>, mail_from: String| Prepared {
        cooked,
        mail_from,
        per_recipient: None,
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
            prepare_post(
                db,
                raw,
                context,
                lease.job.id.0,
                true,
                Some(VerpPolicy {
                    personalized_deliveries: role.verp_personalized_deliveries,
                    delivery_interval: role.verp_delivery_interval,
                }),
            )
            .await
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
                        .retry(
                            lease,
                            now,
                            role.backoff.delay_ms(lease.job.attempts),
                            "list lookup failed",
                        )
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

async fn pending_for_delivery(
    db: &Database,
    role: &MailRoleConfig,
    lease: &Lease,
) -> Option<Vec<String>> {
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
                    role.backoff.delay_ms(lease.job.attempts),
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
    let Some(pending) = pending_for_delivery(db, role, &lease).await else {
        return;
    };
    let message = match db.mail_queue().message(lease.job.message_id).await {
        Ok(message) => message,
        Err(error) => {
            local_delivery_result::<Vec<u8>>(db, role, &lease, Err(lookup_error(&error))).await;
            return;
        }
    };
    let Some(prepared) =
        prepare_delivery_full(db, role, &lease, &message.raw, &message.context).await
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
                role.backoff.delay_ms(lease.job.attempts),
                "relay unreachable",
            )
            .await;
        return;
    };
    let digest = match db.digests().is_delivery(lease.job.id).await {
        Ok(digest) => digest,
        Err(error) => {
            local_delivery_result::<Vec<u8>>(db, role, &lease, Err(lookup_error(&error))).await;
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
            local_delivery_result::<Vec<u8>>(db, role, &lease, Err(lookup_error(&error))).await;
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
        variants.as_deref(),
    )
    .await;
    record_delivery_metrics(message.created_at, &results);
    finish_delivery(db, role, &lease, &pending, &results).await;
}

/// Per-recipient outcomes, and the acceptance-to-relay latency once per
/// delivery that handed at least one recipient to the relay.
fn record_delivery_metrics(accepted_at_ms: i64, results: &[RecipientStatus]) {
    let metrics = listmngr_core::metrics::global();
    let mut sent = false;
    for status in results {
        metrics.recipients.inc(match status {
            RecipientStatus::Sent => {
                sent = true;
                "sent"
            }
            RecipientStatus::TransientFailure(_) => "transient",
            RecipientStatus::RemotePermanentFailure { .. }
            | RecipientStatus::PermanentFailure(_) => "permanent",
            RecipientStatus::Ambiguous(_) => "ambiguous",
        });
    }
    if sent {
        #[allow(clippy::cast_precision_loss)]
        let latency = (chrono::Utc::now().timestamp_millis() - accepted_at_ms) as f64 / 1000.0;
        metrics.delivery_latency_seconds.observe(latency);
    }
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
/// `variants`, when given, holds one copy per pending recipient (in order)
/// and forces one transaction per recipient; a copy's own envelope sender
/// (VERP) overrides `sender`.
async fn send_transactions_with_envid(
    stream: tokio::net::TcpStream,
    role: &MailRoleConfig,
    sender: Option<&str>,
    pending: &[String],
    cooked: &[u8],
    envids: &[String],
    variants: Option<&[RecipientCopy]>,
) -> Vec<RecipientStatus> {
    let config = SmtpClientConfig {
        local_hostname: role.local_hostname.clone(),
        command_timeout: role.command_timeout,
    };
    let width = if variants.is_some() || (role.smtp_single_recipient && sender.is_some()) {
        1
    } else {
        role.max_recipients_per_transaction
    };
    let mut first_stream = Some(stream);
    let mut results: Vec<Option<RecipientStatus>> = vec![None; pending.len()];
    for chunk in crate::delivery_policy::chunk_by_domain(pending, width) {
        let recipients: Vec<String> = chunk.iter().map(|index| pending[*index].clone()).collect();
        // Per-recipient material (a personalized copy, a VERP sender, an
        // ENVID) exists only for one-recipient transactions.
        let single = (chunk.len() == 1).then_some(chunk[0]);
        let copy = single.and_then(|index| variants.and_then(|copies| copies.get(index)));
        let bytes = copy.map_or(cooked, |copy| copy.bytes.as_slice());
        let transaction_sender = copy.and_then(|copy| copy.mail_from.as_deref()).or(sender);
        let envid = single
            .and_then(|index| envids.get(index))
            .map(String::as_str);
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
            for index in &chunk {
                results[*index] = Some(RecipientStatus::TransientFailure(
                    "relay unreachable".into(),
                ));
            }
            continue;
        };
        let started = std::time::Instant::now();
        let sent = send_secure_with_envid(
            stream,
            &config,
            &role.smtp_tls,
            transaction_sender,
            &recipients,
            bytes,
            envid,
        )
        .await;
        let metrics = listmngr_core::metrics::global();
        metrics
            .smtp_transaction_seconds
            .observe(started.elapsed().as_secs_f64());
        metrics
            .smtp_transactions
            .inc(if sent.is_ok() { "completed" } else { "failed" });
        let outcome = match sent {
            Ok(outcome) => outcome.results,
            // Errors are pre-DATA negotiation failures, not uncertain acceptance.
            Err(_) => vec![
                RecipientStatus::TransientFailure("relay greeting or TLS failure".into());
                recipients.len()
            ],
        };
        for (index, status) in chunk.iter().zip(outcome) {
            results[*index] = Some(status);
        }
    }
    results
        .into_iter()
        .map(|status| {
            status.unwrap_or_else(|| RecipientStatus::TransientFailure("not attempted".into()))
        })
        .collect()
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
    role: &MailRoleConfig,
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
                        .retry(
                            lease,
                            now,
                            role.backoff.delay_ms(lease.job.attempts),
                            "local delivery lookup failed",
                        )
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
    role: &MailRoleConfig,
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
        .finish_delivery_with_smtp(
            lease,
            now_ms,
            &outcomes,
            role.backoff.delay_ms(lease.job.attempts),
            &smtp,
        )
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
