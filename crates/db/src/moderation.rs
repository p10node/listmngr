//! Held-message moderation: durable hold, and authenticated accept/reject/discard.
//!
//! Every transition here is one atomic transaction combining the moderation
//! record, its queue-side effect (an `in`-job ack, or a new `out` job with a
//! durable recipient snapshot), a `moderation_log` row, and an audit entry.
use crate::mail_queue::{
    ChildJob, Lease, MessageId, Queue, QueueJob, ack_leased_job, insert_child_job,
};
use crate::{Database, db_error};
use listmngr_core::{Error, ListId, Result, UserId};
use serde::{Deserialize, Serialize};
use sqlx::{Any, Row, Transaction, any::AnyRow};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldId(pub Uuid);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Disposition {
    Accepted,
    Rejected,
    Discarded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldMessage {
    pub id: HeldId,
    pub list_id: ListId,
    pub message_id: MessageId,
    pub sender: String,
    pub subject: String,
    pub reason: String,
    pub hold_date: i64,
    pub disposition: Option<Disposition>,
    pub moderator_id: Option<UserId>,
    pub disposed_at: Option<i64>,
}

/// A moderator decision; acceptance resolves its delivery snapshot under locks.
#[derive(Debug, Clone)]
pub enum ReviewAction {
    Accept { max_attempts: i64 },
    Reject,
    Discard,
    Defer,
}

#[derive(Debug, Clone, Copy)]
pub struct ModerationRepo<'a> {
    db: &'a Database,
    clock: Option<&'a dyn crate::mail_queue::LeaseClock>,
}
impl Database {
    #[must_use]
    pub const fn moderation(&self) -> ModerationRepo<'_> {
        ModerationRepo {
            db: self,
            clock: None,
        }
    }
}

impl<'a> ModerationRepo<'a> {
    /// Use the same post-lock lease clock as queue operations.
    #[must_use]
    pub fn with_clock(mut self, clock: &'a dyn crate::mail_queue::LeaseClock) -> Self {
        self.clock = Some(clock);
        self
    }
    #[must_use]
    pub fn live(self) -> Self {
        self.with_clock(&crate::mail_queue::SystemLeaseClock)
    }
    /// Atomically finish the source `in`-job lease and durably hold its message
    /// for moderator review. A stale/expired lease fences the whole write.
    /// # Errors
    /// Returns stale-lease conflict or database errors.
    pub async fn hold(
        &self,
        lease: &Lease,
        list_id: &ListId,
        sender: &str,
        subject: &str,
        reason: &str,
        now_ms: i64,
    ) -> Result<HeldMessage> {
        let mut tx = self.db.pool().begin().await.map_err(db_error)?;
        let queue = self.db.mail_queue();
        let queue = self.clock.map_or(queue, |clock| queue.with_clock(clock));
        let now_ms = queue.lock_time(&mut tx, lease, now_ms).await?;
        let deadline = crate::mail_queue::MailQueueRepo::locked_deadline(&mut tx, lease).await?;
        let job = ack_leased_job(&mut tx, lease, now_ms).await?;
        let id = HeldId(Uuid::now_v7());
        sqlx::query("INSERT INTO held_messages(id,list_id,message_id,sender,subject,reason,hold_date) VALUES($1,$2,$3,$4,$5,$6,$7)")
            .bind(id.0.to_string()).bind(list_id.as_str()).bind(job.message_id.0.to_string())
            .bind(sender).bind(subject).bind(reason).bind(now_ms)
            .execute(&mut *tx).await.map_err(db_error)?;
        audit_held(&mut tx, id, "moderation.hold", None, reason, now_ms).await?;
        crate::workflows::enqueue_hold_notices(
            &mut tx, self.db, list_id, sender, subject, reason, now_ms,
        )
        .await?;
        // ACK has cleared the row, but its locked deadline still governs all hold writes.
        queue.check_final_deadline(Some(deadline), now_ms)?;
        tx.commit().await.map_err(db_error)?;
        Ok(HeldMessage {
            id,
            list_id: list_id.clone(),
            message_id: job.message_id,
            sender: sender.to_owned(),
            subject: subject.to_owned(),
            reason: reason.to_owned(),
            hold_date: now_ms,
            disposition: None,
            moderator_id: None,
            disposed_at: None,
        })
    }
    /// # Errors
    /// Returns not-found or database errors.
    pub async fn get(&self, id: HeldId) -> Result<HeldMessage> {
        let row = sqlx::query("SELECT * FROM held_messages WHERE id=$1")
            .bind(id.0.to_string())
            .fetch_optional(self.db.pool())
            .await
            .map_err(db_error)?
            .ok_or_else(|| Error::NotFound("held message".into()))?;
        decode_held(&row)
    }
    /// Messages still awaiting a moderator decision, oldest first.
    /// # Errors
    /// Returns a database error.
    pub async fn list_pending(&self, list_id: &ListId) -> Result<Vec<HeldMessage>> {
        let rows = sqlx::query(
            "SELECT * FROM held_messages WHERE list_id=$1 AND disposition IS NULL ORDER BY hold_date,id",
        )
        .bind(list_id.as_str())
        .fetch_all(self.db.pool())
        .await
        .map_err(db_error)?;
        rows.iter().map(decode_held).collect()
    }
    /// Count pending records without loading metadata or MIME payloads.
    /// # Errors
    /// Returns database errors or an unrepresentable count.
    pub async fn count_pending(&self, list_id: &ListId) -> Result<usize> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM held_messages WHERE list_id=$1 AND disposition IS NULL",
        )
        .bind(list_id.as_str())
        .fetch_one(self.db.pool())
        .await
        .map_err(db_error)?;
        usize::try_from(count).map_err(|_| Error::Database("invalid held count".into()))
    }

    /// Fetch only a bounded, deterministic pending metadata window.
    /// # Errors
    /// Returns invalid-range or database errors. A page contains at most 100 records.
    pub async fn pending_page(
        &self,
        list_id: &ListId,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<HeldMessage>> {
        if !(1..=100).contains(&limit) {
            return Err(Error::Validation("held page limit must be 1..=100".into()));
        }
        let offset =
            i64::try_from(offset).map_err(|_| Error::Validation("held offset too large".into()))?;
        let limit =
            i64::try_from(limit).map_err(|_| Error::Validation("held limit too large".into()))?;
        let rows = sqlx::query("SELECT * FROM held_messages WHERE list_id=$1 AND disposition IS NULL ORDER BY hold_date,id LIMIT $2 OFFSET $3")
            .bind(list_id.as_str()).bind(limit).bind(offset).fetch_all(self.db.pool()).await.map_err(db_error)?;
        rows.iter().map(decode_held).collect()
    }

    /// Accept a pending held message: atomically records the disposition,
    /// creates its outgoing job with a durable recipient snapshot, and audits
    /// both. Fenced on `disposition IS NULL`, so a duplicate accept/reject/
    /// discard request (e.g. a racing moderator) has no additional effect.
    /// # Errors
    /// Returns conflict if already disposed, or a database error.
    pub async fn accept(
        &self,
        id: HeldId,
        moderator: Option<UserId>,
        recipients: &[String],
        max_attempts: i64,
        now_ms: i64,
    ) -> Result<(HeldMessage, QueueJob)> {
        self.accept_impl(id, moderator, (recipients, None), max_attempts, now_ms)
            .await
    }
    /// Accept with identity and preference authority captured by the producer.
    /// # Errors
    /// Returns conflict, invalid plan, or database errors.
    pub async fn accept_with_plan(
        &self,
        id: HeldId,
        moderator: Option<UserId>,
        plan: &crate::mail_queue::RecipientPlan,
        max_attempts: i64,
        now_ms: i64,
    ) -> Result<(HeldMessage, QueueJob)> {
        self.accept_impl(
            id,
            moderator,
            (&plan.emails(), Some(plan)),
            max_attempts,
            now_ms,
        )
        .await
    }
    async fn accept_impl(
        &self,
        id: HeldId,
        moderator: Option<UserId>,
        recipients: (&[String], Option<&crate::mail_queue::RecipientPlan>),
        max_attempts: i64,
        now_ms: i64,
    ) -> Result<(HeldMessage, QueueJob)> {
        let mut tx = self.db.pool().begin().await.map_err(db_error)?;
        let held = set_disposition(&mut tx, id, "accepted", moderator, now_ms).await?;
        let job = insert_child_job(
            &mut tx,
            held.message_id,
            &ChildJob {
                queue: Queue::Out,
                max_attempts,
                recipients: recipients.0.to_vec(),
            },
            now_ms,
        )
        .await?;
        insert_child_job(
            &mut tx,
            held.message_id,
            &ChildJob {
                queue: Queue::Digest,
                max_attempts,
                recipients: Vec::new(),
            },
            now_ms,
        )
        .await?;
        if let Some(plan) = recipients.1 {
            crate::mail_queue::plan::bind(&mut tx, job.id, plan).await?;
        }
        crate::archive::schedule_accepted(&mut tx, &held.list_id, held.message_id, now_ms).await?;
        insert_moderation_log(&mut tx, id, "accept", "", moderator, now_ms).await?;
        audit_held(&mut tx, id, "moderation.accept", moderator, "", now_ms).await?;
        tx.commit().await.map_err(db_error)?;
        Ok((held, job))
    }
    /// # Errors
    /// Returns conflict if already disposed, or a database error.
    pub async fn reject(
        &self,
        id: HeldId,
        moderator: Option<UserId>,
        reason: &str,
        now_ms: i64,
    ) -> Result<HeldMessage> {
        self.dispose(id, "rejected", moderator, reason, now_ms)
            .await
    }
    /// # Errors
    /// Returns conflict if already disposed, or a database error.
    pub async fn discard(
        &self,
        id: HeldId,
        moderator: Option<UserId>,
        reason: &str,
        now_ms: i64,
    ) -> Result<HeldMessage> {
        self.dispose(id, "discarded", moderator, reason, now_ms)
            .await
    }
    /// Apply a moderator decision and preserve the comment atomically.
    ///
    /// # Errors
    /// Returns conflict for disposed messages, or a database error.
    pub async fn review(
        &self,
        id: HeldId,
        context: &crate::AuditContext,
        action: &ReviewAction,
        reason: &str,
        now_ms: i64,
    ) -> Result<()> {
        let mut tx = self.db.browser_write_tx().await?;
        Self::review_tx(&mut tx, self.db, id, context, action, reason, now_ms).await?;
        tx.commit().await.map_err(db_error)
    }

    pub(crate) async fn review_tx(
        tx: &mut Transaction<'_, Any>,
        db: &Database,
        id: HeldId,
        context: &crate::AuditContext,
        action: &ReviewAction,
        reason: &str,
        now_ms: i64,
    ) -> Result<()> {
        let moderator = context.user_id;
        let (name, disposition) = match action {
            ReviewAction::Accept { .. } => ("accept", Some("accepted")),
            ReviewAction::Reject => ("rejected", Some("rejected")),
            ReviewAction::Discard => ("discarded", Some("discarded")),
            ReviewAction::Defer => ("defer", None),
        };
        let held = if let Some(disposition) = disposition {
            set_disposition(tx, id, disposition, moderator, now_ms).await?
        } else {
            let row = sqlx::query("UPDATE held_messages SET reason=reason WHERE id=$1 AND disposition IS NULL RETURNING *")
                .bind(id.0.to_string()).fetch_optional(&mut **tx).await.map_err(db_error)?
                .ok_or_else(|| Error::Conflict("held message already has a disposition".into()))?;
            decode_held(&row)?
        };
        if let ReviewAction::Accept { max_attempts } = action {
            let recipients = held_recipients_tx(tx, &held).await?;
            let job = insert_child_job(
                tx,
                held.message_id,
                &ChildJob {
                    queue: Queue::Out,
                    max_attempts: *max_attempts,
                    recipients: recipients.emails(),
                },
                now_ms,
            )
            .await?;
            insert_child_job(
                tx,
                held.message_id,
                &ChildJob {
                    queue: Queue::Digest,
                    max_attempts: *max_attempts,
                    recipients: Vec::new(),
                },
                now_ms,
            )
            .await?;
            crate::mail_queue::plan::bind(tx, job.id, &recipients).await?;
            crate::archive::schedule_accepted(tx, &held.list_id, held.message_id, now_ms).await?;
        }
        if matches!(action, ReviewAction::Reject) {
            rejection_notice(tx, db, &held, reason, now_ms).await?;
        }
        insert_moderation_log(tx, id, name, reason, moderator, now_ms).await?;
        Database::record_tx_with_context(
            tx,
            context,
            &format!("moderation.{name}"),
            "held_message",
            &id.0.to_string(),
            serde_json::json!({"reason":reason}),
        )
        .await?;
        Ok(())
    }

    async fn dispose(
        &self,
        id: HeldId,
        disposition: &str,
        moderator: Option<UserId>,
        reason: &str,
        now_ms: i64,
    ) -> Result<HeldMessage> {
        let mut tx = self.db.pool().begin().await.map_err(db_error)?;
        let held = set_disposition(&mut tx, id, disposition, moderator, now_ms).await?;
        if disposition == "rejected" {
            rejection_notice(&mut tx, self.db, &held, reason, now_ms).await?;
        }
        insert_moderation_log(&mut tx, id, disposition, reason, moderator, now_ms).await?;
        audit_held(
            &mut tx,
            id,
            &format!("moderation.{disposition}"),
            moderator,
            reason,
            now_ms,
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(held)
    }
}

/// Only the stored envelope is reply authority; held metadata must agree by
/// canonical identity. Legacy/malformed context suppresses mail, not disposition
/// or the complete moderator reason in `moderation_log`/`audit_log`.
async fn rejection_notice(
    tx: &mut Transaction<'_, Any>,
    db: &Database,
    held: &HeldMessage,
    reason: &str,
    now_ms: i64,
) -> Result<()> {
    let row = sqlx::query("SELECT m.context,b.raw FROM messages m JOIN message_blobs b ON b.store_key=m.store_key WHERE m.id=$1")
        .bind(held.message_id.0.to_string()).fetch_one(&mut **tx).await.map_err(db_error)?;
    let context: String = row.try_get("context").map_err(db_error)?;
    let Ok(context) = serde_json::from_str::<serde_json::Value>(&context) else {
        return Ok(());
    };
    let Some(sender) = context["envelope_sender"].as_str() else {
        return Ok(());
    };
    if context["list_id"].as_str() != Some(held.list_id.as_str())
        || context["owner_route"] == true
        || context.get("subscription_command").is_some()
    {
        return Ok(());
    }
    let original: Vec<u8> = row.try_get("raw").map_err(db_error)?;
    if !listmngr_mail::owner::allows_forward(&original, Some(sender), &held.list_id) {
        return Ok(());
    }
    let Ok(author) = listmngr_core::Address::new(sender, String::new()) else {
        return Ok(());
    };
    if !listmngr_core::Address::new(&held.sender, String::new())
        .is_ok_and(|metadata| metadata.email == author.email)
    {
        return Ok(());
    }
    crate::workflows::enqueue_rejection_notice(tx, db, &held.list_id, sender, reason, now_ms).await
}

async fn set_disposition(
    tx: &mut Transaction<'_, Any>,
    id: HeldId,
    disposition: &str,
    moderator: Option<UserId>,
    now_ms: i64,
) -> Result<HeldMessage> {
    let row = sqlx::query("UPDATE held_messages SET disposition=$1,moderator_id=$2,disposed_at=$3 WHERE id=$4 AND disposition IS NULL RETURNING *")
        .bind(disposition).bind(moderator.map(|value| value.to_string())).bind(now_ms).bind(id.0.to_string())
        .fetch_optional(&mut **tx).await.map_err(db_error)?
        .ok_or_else(|| Error::Conflict("held message already has a disposition".into()))?;
    decode_held(&row)
}

async fn insert_moderation_log(
    tx: &mut Transaction<'_, Any>,
    held_id: HeldId,
    action: &str,
    reason: &str,
    moderator: Option<UserId>,
    now_ms: i64,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO moderation_log(id,held_id,action,reason,moderator_id,at) VALUES($1,$2,$3,$4,$5,$6)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(held_id.0.to_string())
    .bind(action)
    .bind(reason)
    .bind(moderator.map(|value| value.to_string()))
    .bind(now_ms)
    .execute(&mut **tx)
    .await
    .map_err(db_error)?;
    Ok(())
}

async fn audit_held(
    tx: &mut Transaction<'_, Any>,
    id: HeldId,
    action: &str,
    moderator: Option<UserId>,
    reason: &str,
    now_ms: i64,
) -> Result<()> {
    let at = chrono::DateTime::from_timestamp_millis(now_ms)
        .ok_or_else(|| Error::Validation("timestamp out of range".into()))?;
    sqlx::query("INSERT INTO audit_log(id,at,actor_user_id,action,target_type,target_id,diff) VALUES($1,$2,$3,$4,'held_message',$5,$6)")
        .bind(Uuid::now_v7().to_string()).bind(at.to_rfc3339()).bind(moderator.map(|value| value.to_string()))
        .bind(action).bind(id.0.to_string()).bind(serde_json::json!({"reason": reason}).to_string())
        .execute(&mut **tx).await.map_err(db_error)?;
    Ok(())
}

fn decode_held(row: &AnyRow) -> Result<HeldMessage> {
    let disposition: Option<String> = row.try_get("disposition").map_err(db_error)?;
    let disposition = disposition
        .map(|value| match value.as_str() {
            "accepted" => Ok(Disposition::Accepted),
            "rejected" => Ok(Disposition::Rejected),
            "discarded" => Ok(Disposition::Discarded),
            _ => Err(Error::Database("unknown held-message disposition".into())),
        })
        .transpose()?;
    let moderator_id: Option<String> = row.try_get("moderator_id").map_err(db_error)?;
    Ok(HeldMessage {
        id: HeldId(
            row.try_get::<String, _>("id")
                .map_err(db_error)?
                .parse()
                .map_err(db_error)?,
        ),
        list_id: row
            .try_get::<String, _>("list_id")
            .map_err(db_error)?
            .parse()
            .map_err(db_error)?,
        message_id: MessageId(
            row.try_get::<String, _>("message_id")
                .map_err(db_error)?
                .parse()
                .map_err(db_error)?,
        ),
        sender: row.try_get("sender").map_err(db_error)?,
        subject: row.try_get("subject").map_err(db_error)?,
        reason: row.try_get("reason").map_err(db_error)?,
        hold_date: row.try_get("hold_date").map_err(db_error)?,
        disposition,
        moderator_id: moderator_id
            .map(|value| value.parse())
            .transpose()
            .map_err(db_error)?,
        disposed_at: row.try_get("disposed_at").map_err(db_error)?,
    })
}

/// Resolve the same member > address > user > system delivery layers as
/// `PreferencesRepo::resolve_member`, on the transaction's locked roster.
/// `AsUser` retains the member address and uses `member.user_id` for the user layer;
/// normalized email is policy identity, `original_email` is the SMTP mailbox.
async fn held_recipients_tx(
    tx: &mut Transaction<'_, Any>,
    held: &HeldMessage,
) -> Result<crate::mail_queue::RecipientPlan> {
    let raw: Vec<u8> = sqlx::query_scalar("SELECT b.raw FROM messages m JOIN message_blobs b ON b.store_key=m.store_key WHERE m.id=$1")
        .bind(held.message_id.0.to_string()).fetch_one(&mut **tx).await.map_err(db_error)?;
    crate::mail_queue::plan::select(tx, &held.list_id, &held.sender, &raw).await
}
