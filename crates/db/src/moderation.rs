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

/// A moderator decision; accepting includes a durable delivery snapshot.
#[derive(Debug, Clone)]
pub enum ReviewAction {
    Accept(ChildJob),
    Reject,
    Discard,
    Defer,
}

#[derive(Debug, Clone, Copy)]
pub struct ModerationRepo<'a> {
    db: &'a Database,
}
impl Database {
    #[must_use]
    pub const fn moderation(&self) -> ModerationRepo<'_> {
        ModerationRepo { db: self }
    }
}

impl ModerationRepo<'_> {
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
        let job = ack_leased_job(&mut tx, lease, now_ms).await?;
        let id = HeldId(Uuid::now_v7());
        sqlx::query("INSERT INTO held_messages(id,list_id,message_id,sender,subject,reason,hold_date) VALUES($1,$2,$3,$4,$5,$6,$7)")
            .bind(id.0.to_string()).bind(list_id.as_str()).bind(job.message_id.0.to_string())
            .bind(sender).bind(subject).bind(reason).bind(now_ms)
            .execute(&mut *tx).await.map_err(db_error)?;
        audit_held(&mut tx, id, "moderation.hold", None, reason, now_ms).await?;
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
        let mut tx = self.db.pool().begin().await.map_err(db_error)?;
        let held = set_disposition(&mut tx, id, "accepted", moderator, now_ms).await?;
        let job = insert_child_job(
            &mut tx,
            held.message_id,
            &ChildJob {
                queue: Queue::Out,
                max_attempts,
                recipients: recipients.to_vec(),
            },
            now_ms,
        )
        .await?;
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
        let mut tx = self.db.pool().begin().await.map_err(db_error)?;
        let moderator = context.user_id;
        let (name, disposition) = match action {
            ReviewAction::Accept(_) => ("accept", Some("accepted")),
            ReviewAction::Reject => ("rejected", Some("rejected")),
            ReviewAction::Discard => ("discarded", Some("discarded")),
            ReviewAction::Defer => ("defer", None),
        };
        let held = if let Some(disposition) = disposition {
            set_disposition(&mut tx, id, disposition, moderator, now_ms).await?
        } else {
            let row = sqlx::query("UPDATE held_messages SET reason=reason WHERE id=$1 AND disposition IS NULL RETURNING *")
                .bind(id.0.to_string()).fetch_optional(&mut *tx).await.map_err(db_error)?
                .ok_or_else(|| Error::Conflict("held message already has a disposition".into()))?;
            decode_held(&row)?
        };
        if let ReviewAction::Accept(child) = action {
            if child.queue != Queue::Out {
                return Err(Error::Validation(
                    "moderation accept requires out queue".into(),
                ));
            }
            insert_child_job(&mut tx, held.message_id, child, now_ms).await?;
        }
        insert_moderation_log(&mut tx, id, name, reason, moderator, now_ms).await?;
        Database::record_tx_with_context(
            &mut tx,
            context,
            &format!("moderation.{name}"),
            "held_message",
            &id.0.to_string(),
            serde_json::json!({"reason":reason}),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
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
