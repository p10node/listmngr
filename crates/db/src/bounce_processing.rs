//! Mailman's bounce runner: the consumer of `Queue::Bounces`.
//!
//! A message that reached `list-bounces@` (or a VERP bounce address) names
//! the member it concerns in one of two trustworthy ways — the VERP
//! address the intake decoded, or a delivery-status report whose
//! `Original-Envelope-Id` this server issued — and, failing those, in the
//! report's own `Final-Recipient` claims or, for MTAs that write prose
//! instead of a report, in what `listmngr_mail::bounce` can read from it. Recognized members are scored with
//! exactly the rules an SMTP-time failure uses; what cannot be attributed is
//! forwarded where `forward_unrecognized_bounces_to` points, or dropped.
//! Every effect commits with the lease acknowledgement.
use crate::mail_queue::{ChildJob, Lease, Queue, ack_leased_job, insert_child_job};
use crate::{Database, db_error};
use listmngr_core::dsn_issuance::Issuer;
use listmngr_core::{Address, Error, ListId, Result, UnrecognizedBounceDisposition};
use sqlx::{Any, Row, Transaction};
use uuid::Uuid;

/// What processing one bounce did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Members whose score the bounce raised (or whose delivery it disabled).
    pub scored: Vec<String>,
    /// Nothing in the message named a recipient at all.
    pub unrecognized: bool,
    /// Where an unrecognized bounce was forwarded, if anywhere.
    pub forwarded_to: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct BounceProcessingRepo<'a> {
    db: &'a Database,
    clock: Option<&'a dyn crate::mail_queue::LeaseClock>,
}

impl Database {
    #[must_use]
    pub const fn bounce_processing(&self) -> BounceProcessingRepo<'_> {
        BounceProcessingRepo {
            db: self,
            clock: None,
        }
    }
}

/// A recipient the bounce named, and how.
struct Named {
    recipient: String,
    source: &'static str,
}

impl<'a> BounceProcessingRepo<'a> {
    #[must_use]
    pub fn with_clock(mut self, clock: &'a dyn crate::mail_queue::LeaseClock) -> Self {
        self.clock = Some(clock);
        self
    }
    #[must_use]
    pub fn live(self) -> Self {
        self.with_clock(&crate::mail_queue::SystemLeaseClock)
    }
    fn queue(&self) -> crate::mail_queue::MailQueueRepo<'a> {
        let queue = self.db.mail_queue();
        self.clock.map_or(queue, |clock| queue.with_clock(clock))
    }

    /// Process one leased bounce: recognize, score, forward or drop, and
    /// acknowledge the job — all in one fenced transaction.
    /// # Errors
    /// Returns a validation error for a lease that is not a bounce, a stale
    /// lease, or database errors; nothing is committed on error.
    pub async fn process(
        &self,
        lease: &Lease,
        issuer: Option<&Issuer>,
        site_owner: &str,
        now_ms: i64,
    ) -> Result<Outcome> {
        if lease.job.queue != Queue::Bounces {
            return Err(Error::Validation("bounce lease required".into()));
        }
        let message = self.db.mail_queue().message(lease.job.message_id).await?;
        let context: serde_json::Value =
            serde_json::from_str(&message.context).map_err(db_error)?;
        let list: ListId = context["list_id"]
            .as_str()
            .ok_or_else(|| Error::Validation("bounce without a list".into()))?
            .parse()?;
        let mut tx = self.db.pool().begin().await.map_err(db_error)?;
        let queue = self.queue();
        let now = queue.lock_time(&mut tx, lease, now_ms).await?;
        let disposition: Option<String> = sqlx::query_scalar(
            "UPDATE mailing_lists SET list_id=list_id WHERE list_id=$1 RETURNING forward_unrecognized_bounces_to",
        )
        .bind(list.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?;
        let mut outcome = Outcome::default();
        if let Some(disposition) = disposition {
            let named = named_recipients(&mut tx, &context, &message.raw, issuer, now).await?;
            if let Some(named) = named {
                for named in named {
                    if score_member(&mut tx, self.db, lease, &list, &message.id.0, &named, now)
                        .await?
                    {
                        outcome.scored.push(named.recipient);
                    }
                }
            } else {
                outcome.unrecognized = true;
                outcome.forwarded_to = forward_unrecognized(
                    &mut tx,
                    &list,
                    &disposition,
                    site_owner,
                    message.id,
                    lease.job.max_attempts,
                    now,
                )
                .await?;
            }
        } else {
            // The list is gone: nothing to score, nobody to tell.
            outcome.unrecognized = true;
        }
        crate::mail_queue::audit(&mut tx, &lease.job, "bounce.process", now).await?;
        ack_leased_job(&mut tx, lease, queue.time(now_ms)).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(outcome)
    }
}

/// The recipients the bounce names, most trustworthy source first, or
/// `None` when it names nobody. A DSN that only reports delays names
/// nobody actionable but is still recognized (an empty list).
async fn named_recipients(
    tx: &mut Transaction<'_, Any>,
    context: &serde_json::Value,
    raw: &[u8],
    issuer: Option<&Issuer>,
    now: i64,
) -> Result<Option<Vec<Named>>> {
    if let Some(recipient) = context["verp_recipient"].as_str() {
        return Ok(Some(vec![Named {
            recipient: recipient.to_owned(),
            source: "verp",
        }]));
    }
    if let Some(report) = listmngr_mail::dsn::parse_report(raw) {
        if let (Some(envid), Some(issuer)) = (&report.original_envelope_id, issuer)
            && let Some(recipient) = verified_issuance(tx, envid, issuer, now).await?
        {
            return Ok(Some(vec![Named {
                recipient,
                source: "dsn_envid",
            }]));
        }
        return Ok(Some(
            report
                .recipients
                .into_iter()
                .filter(|claim| claim.action == "failed")
                .map(|claim| Named {
                    recipient: claim.final_recipient,
                    source: "dsn",
                })
                .collect(),
        ));
    }
    // No standard report: the MTA-family detectors read the prose.
    Ok(match listmngr_mail::bounce::detect(raw).detection {
        listmngr_mail::bounce::Detection::Failed(addresses) => Some(
            addresses
                .into_iter()
                .map(|recipient| Named {
                    recipient,
                    source: "heuristic",
                })
                .collect(),
        ),
        listmngr_mail::bounce::Detection::Temporary => Some(Vec::new()),
        listmngr_mail::bounce::Detection::Unrecognized => None,
    })
}

/// The recipient an ENVID this server issued was for, when the MAC and the
/// validity window check out; the report's own claims are then irrelevant.
async fn verified_issuance(
    tx: &mut Transaction<'_, Any>,
    envid: &str,
    issuer: &Issuer,
    now: i64,
) -> Result<Option<String>> {
    let row = sqlx::query(
        "SELECT recipient, claims, issued_at, expires_at FROM dsn_issuances WHERE envid=$1",
    )
    .bind(envid)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_error)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let claims: String = row.try_get("claims").map_err(db_error)?;
    let issued_at: i64 = row.try_get("issued_at").map_err(db_error)?;
    let expires_at: i64 = row.try_get("expires_at").map_err(db_error)?;
    if issuer.verify(envid, &claims, now, issued_at, expires_at) {
        Ok(Some(row.try_get("recipient").map_err(db_error)?))
    } else {
        Ok(None)
    }
}

/// Record and score one named recipient when they are a member of the list.
/// Returns whether the score changed hands (a disabled member is left as
/// they are, exactly as an SMTP-time failure would).
async fn score_member(
    tx: &mut Transaction<'_, Any>,
    db: &Database,
    lease: &Lease,
    list: &ListId,
    message_id: &Uuid,
    named: &Named,
    now: i64,
) -> Result<bool> {
    let Ok(address) = Address::new(&named.recipient, String::new()) else {
        return Ok(false);
    };
    let member: Option<String> = sqlx::query_scalar("SELECT m.id FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id=$1 AND m.role='member' AND a.email=$2")
        .bind(list.as_str())
        .bind(&address.email)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_error)?;
    if member.is_none() {
        return Ok(false);
    }
    let id = Uuid::now_v7().to_string();
    let inserted = sqlx::query("INSERT INTO bounce_events(id,list_id,recipient,job_id,message_id,created_at,source,context,processed) VALUES($1,$2,$3,$4,$5,$6,$7,'normal',0) ON CONFLICT(job_id,recipient) DO NOTHING")
        .bind(&id)
        .bind(list.as_str())
        .bind(&address.original_email)
        .bind(lease.job.id.0.to_string())
        .bind(message_id.to_string())
        .bind(now)
        .bind(named.source)
        .execute(&mut **tx)
        .await
        .map_err(db_error)?
        .rows_affected();
    if inserted == 0 {
        return Ok(false);
    }
    let at = chrono::DateTime::from_timestamp_millis(now)
        .ok_or_else(|| Error::Validation("timestamp out of range".into()))?;
    crate::smtp_bounces::score(tx, db, list, &address.original_email, &id, at).await?;
    let processed: i64 = sqlx::query_scalar("SELECT processed FROM bounce_events WHERE id=$1")
        .bind(&id)
        .fetch_one(&mut **tx)
        .await
        .map_err(db_error)?;
    Ok(processed == 1)
}

/// Hand an unattributable bounce to the people the list names, as an owner
/// delivery (sanitized, null reverse path), or drop it.
async fn forward_unrecognized(
    tx: &mut Transaction<'_, Any>,
    list: &ListId,
    disposition: &str,
    site_owner: &str,
    message_id: crate::mail_queue::MessageId,
    attempts: i64,
    now: i64,
) -> Result<Vec<String>> {
    let disposition: UnrecognizedBounceDisposition = disposition
        .parse()
        .map_err(|_| Error::Validation("forward_unrecognized_bounces_to".into()))?;
    let recipients: Vec<String> = match disposition {
        UnrecognizedBounceDisposition::Discard => Vec::new(),
        UnrecognizedBounceDisposition::SiteOwner => vec![site_owner.to_owned()],
        UnrecognizedBounceDisposition::Administrators => {
            sqlx::query_scalar("SELECT a.original_email FROM addresses a WHERE EXISTS (SELECT 1 FROM members m WHERE m.address_id=a.id AND m.list_id=$1 AND m.role IN ('owner','moderator')) ORDER BY a.email")
                .bind(list.as_str())
                .fetch_all(&mut **tx)
                .await
                .map_err(db_error)?
        }
    };
    let recipients: Vec<String> = recipients
        .into_iter()
        .filter(|email| {
            listmngr_mail::owner::safe_mailbox(email)
                && !listmngr_mail::owner::points_to_list(email, list)
        })
        .collect();
    if recipients.is_empty() {
        return Ok(recipients);
    }
    let child = insert_child_job(
        tx,
        message_id,
        &ChildJob {
            queue: Queue::Out,
            max_attempts: attempts.max(1),
            recipients: recipients.clone(),
        },
        now,
    )
    .await?;
    sqlx::query("INSERT INTO owner_deliveries(job_id) VALUES($1)")
        .bind(child.id.0.to_string())
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    crate::mail_queue::audit(tx, &child, "bounce.forward", now).await?;
    Ok(recipients)
}
