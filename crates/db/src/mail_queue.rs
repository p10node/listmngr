//! Exact-byte database intake and leased queue. All caller times are Unix milliseconds.
use crate::{Database, db_error};
use listmngr_core::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Any, Row, Transaction, any::AnyRow};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageId(pub Uuid);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobId(pub Uuid);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Queue {
    In,
    Pipeline,
    Out,
    Retry,
    Bounces,
    Command,
    Virgin,
    Archive,
    Digest,
    Nntp,
    Shunt,
    Bad,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    Ready,
    Leased,
    Done,
    Shunted,
}
/// One recipient's terminal (or quarantined) outgoing outcome.
///
/// `Ambiguous` is deliberately not a synonym for `Failed`: it means the relay
/// connection was lost after the message write, so the relay may already
/// have accepted the message. It is durably quarantined (excluded from
/// `pending_recipients`, so it is never auto-retried) rather than folded
/// into an ordinary failure or silently
/// treated as sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecipientOutcome {
    /// A known ordinary transient failure, safe to retry.
    Transient,
    Sent,
    Failed,
    Ambiguous,
}
/// One downstream job to create atomically alongside a source job's completion.
///
/// The child references the same durable message bytes as its source. A
/// non-empty `recipients` snapshot is stored as that job's durable, per-recipient
/// outgoing progress so a retried job never resends to an already-sent recipient.
#[derive(Debug, Clone)]
pub struct ChildJob {
    pub queue: Queue,
    pub max_attempts: i64,
    pub recipients: Vec<String>,
}
#[derive(Debug, Clone)]
pub struct NewMessage {
    pub raw: Vec<u8>,
    pub external_id: String,
    pub context: String,
    pub queue: Queue,
    pub max_attempts: i64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueJob {
    pub id: JobId,
    pub message_id: MessageId,
    pub queue: Queue,
    pub state: JobState,
    pub attempts: i64,
    pub max_attempts: i64,
    pub run_after: i64,
    pub locked_by: Option<String>,
    pub lease_until: Option<i64>,
    pub last_error: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMessage {
    pub id: MessageId,
    pub store_key: String,
    pub external_id: String,
    pub context: String,
    pub created_at: i64,
    pub raw: Vec<u8>,
}
#[derive(Debug, Clone, Copy)]
pub struct MailQueueRepo<'a> {
    db: &'a Database,
}
impl Database {
    #[must_use]
    pub const fn mail_queue(&self) -> MailQueueRepo<'_> {
        MailQueueRepo { db: self }
    }
}
/// A claim capability. The token is opaque outside this crate and changes on
/// every claim, even for the same worker.
///
/// Visible within `listmngr-db` so sibling modules (e.g. moderation) can
/// compose additional atomic handoffs that fence on the same lease.
#[derive(Clone)]
pub struct Lease {
    pub job: QueueJob,
    pub(crate) token: String,
}

impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lease")
            .field("job", &self.job)
            .field("token", &"[REDACTED]")
            .finish()
    }
}

impl MailQueueRepo<'_> {
    /// Complete a currently owned, unexpired lease without deleting its raw message.
    /// # Errors
    /// Returns conflict for stale/expired/already-consumed leases, or a database error.
    pub async fn ack(&self, lease: &Lease, now_ms: i64) -> Result<QueueJob> {
        self.transition(lease, now_ms, now_ms, "ack", "").await
    }
    /// Release for another attempt at `now_ms + delay_ms`, or shunt at the budget.
    /// The caller supplies its backoff policy; negative/overflowing delays are rejected.
    /// # Errors
    /// Returns validation, stale-lease conflict, or database errors.
    pub async fn retry(
        &self,
        lease: &Lease,
        now_ms: i64,
        delay_ms: i64,
        reason: &str,
    ) -> Result<QueueJob> {
        let due = now_ms
            .checked_add(delay_ms)
            .filter(|_| delay_ms >= 0)
            .ok_or_else(|| {
                Error::Validation("retry delay must be nonnegative and not overflow".into())
            })?;
        self.transition(lease, now_ms, due, "retry", reason).await
    }
    /// Explicitly shunt a currently owned lease, recording a diagnostic reason.
    /// # Errors
    /// Returns stale-lease conflict or database errors.
    pub async fn shunt(&self, lease: &Lease, now_ms: i64, reason: &str) -> Result<QueueJob> {
        self.transition(lease, now_ms, now_ms, "shunt", reason)
            .await
    }
    /// Extend a currently owned, unexpired lease so a long-running worker keeps
    /// its claim without another worker recovering the job as expired.
    /// The fencing token is unchanged; only `lease_until` moves forward.
    /// # Errors
    /// Returns validation, stale-lease conflict, or database errors.
    pub async fn heartbeat(&self, lease: &Lease, now_ms: i64, lease_ms: i64) -> Result<Lease> {
        let until = now_ms
            .checked_add(lease_ms)
            .filter(|_| lease_ms > 0)
            .ok_or_else(|| Error::Validation("lease must be positive and not overflow".into()))?;
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        // The deadline is monotonic non-decreasing: a CASE-clamped max (not a
        // plain overwrite) so an out-of-order or short-TTL renewal can never
        // shrink an already-extended lease and let another worker reclaim it
        // early while this worker still believes it holds the job.
        let row = sqlx::query("UPDATE queue_jobs SET lease_until=CASE WHEN $1>lease_until THEN $1 ELSE lease_until END WHERE id=$2 AND state='leased' AND lease_token=$3 AND lease_until>$4 RETURNING *")
            .bind(until).bind(lease.job.id.0.to_string()).bind(&lease.token).bind(now_ms)
            .fetch_optional(&mut *tx).await.map_err(db_error)?
            .ok_or_else(|| Error::Conflict("stale or expired queue lease".into()))?;
        let job = decode_job(&row)?;
        audit(&mut tx, &job, "queue.heartbeat", now_ms).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(Lease {
            job,
            token: lease.token.clone(),
        })
    }
    /// Replay a shunted job onto a live queue with a fresh attempt budget.
    /// The target must not itself be a quarantine queue.
    /// # Errors
    /// Returns validation for a quarantine target, conflict when the job is
    /// not currently shunted, or a database error.
    pub async fn unshunt(&self, id: JobId, now_ms: i64, target: Queue) -> Result<QueueJob> {
        if matches!(target, Queue::Shunt | Queue::Bad) {
            return Err(Error::Validation(
                "unshunt target must be a live processing queue".into(),
            ));
        }
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        let row = sqlx::query("UPDATE queue_jobs SET queue=$1,state='ready',attempts=0,run_after=$2,last_error='',locked_by=NULL,lease_token=NULL,lease_until=NULL WHERE id=$3 AND state='shunted' RETURNING *")
            .bind(queue_name(target)).bind(now_ms).bind(id.0.to_string())
            .fetch_optional(&mut *tx).await.map_err(db_error)?
            .ok_or_else(|| Error::Conflict("job is not currently shunted".into()))?;
        let job = decode_job(&row)?;
        audit(&mut tx, &job, "queue.unshunt", now_ms).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(job)
    }
    /// Atomically finish a currently owned, unexpired lease and create its
    /// downstream jobs on the same durable message. A stale or already-consumed
    /// lease fences the whole handoff: neither the ack nor any child commits.
    /// # Errors
    /// Returns stale-lease conflict or database errors.
    pub async fn complete_with_children(
        &self,
        lease: &Lease,
        now_ms: i64,
        children: &[ChildJob],
    ) -> Result<(QueueJob, Vec<QueueJob>)> {
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        let source = ack_leased_job(&mut tx, lease, now_ms).await?;
        let mut created = Vec::with_capacity(children.len());
        for child in children {
            created.push(insert_child_job(&mut tx, source.message_id, child, now_ms).await?);
        }
        tx.commit().await.map_err(db_error)?;
        Ok((source, created))
    }
    /// Recipients still awaiting an outgoing attempt for this job (never
    /// includes recipients already marked `sent`, so a retried job cannot
    /// resend to them).
    /// # Errors
    /// Returns a database error.
    pub async fn pending_recipients(&self, job_id: JobId) -> Result<Vec<String>> {
        sqlx::query_scalar(
            "SELECT email FROM delivery_recipients WHERE job_id=$1 AND status='pending' ORDER BY email",
        )
        .bind(job_id.0.to_string())
        .fetch_all(self.db.pool())
        .await
        .map_err(db_error)
    }
    /// Durably reserve recipients before any SMTP protocol side effect.
    /// Unknown attempts are quarantined immediately; only this lease may resolve
    /// them. A crash (even before DATA) therefore sacrifices automatic retry,
    /// not duplicate safety. The reservation and audit commit together.
    /// # Errors
    /// Returns conflict for an expired/stale lease or non-pending recipient,
    /// or a database error. Never send if this operation fails.
    pub async fn begin_delivery(
        &self,
        lease: &Lease,
        now_ms: i64,
        recipients: &[String],
    ) -> Result<()> {
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        let row = sqlx::query("UPDATE queue_jobs SET last_error=last_error WHERE id=$1 AND state='leased' AND lease_token=$2 AND lease_until>$3 RETURNING *")
            .bind(lease.job.id.0.to_string()).bind(&lease.token).bind(now_ms)
            .fetch_optional(&mut *tx).await.map_err(db_error)?
            .ok_or_else(|| Error::Conflict("stale or expired queue lease".into()))?;
        let job = decode_job(&row)?;
        for email in recipients {
            let changed = sqlx::query("UPDATE delivery_recipients SET status='ambiguous',detail='SMTP attempt in-flight; remote outcome unknown',attempt_token=$1 WHERE job_id=$2 AND email=$3 AND status='pending' AND attempt_token IS NULL")
                .bind(&lease.token).bind(job.id.0.to_string()).bind(email)
                .execute(&mut *tx).await.map_err(db_error)?.rows_affected();
            if changed != 1 {
                return Err(Error::Conflict("recipient is not pending".into()));
            }
        }
        audit(&mut tx, &job, "queue.delivery_begin", now_ms).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(())
    }
    /// Atomically record each given recipient's terminal outcome for a
    /// currently owned, unexpired lease, then complete the job: retried (with
    /// `retry_delay_ms` backoff) if any recipient is still `pending` after
    /// applying these outcomes, or acked otherwise.
    ///
    /// Fenced exactly like [`Self::ack`]/[`Self::retry`]: a stale or
    /// already-consumed lease fences the whole handoff, so neither the
    /// recipient writes nor the job transition commits, and a stale worker
    /// can never overwrite a newer worker's progress. Known ordinary transient
    /// failures must be explicit `Transient` outcomes. Omitted recipients stay
    /// untouched: reserved attempts remain ambiguous, while recipients never
    /// reserved remain pending. A newer lease cannot resolve an old attempt.
    /// # Errors
    /// Returns stale-lease conflict, validation (a negative/overflowing
    /// `retry_delay_ms`), or database errors.
    pub async fn finish_delivery(
        &self,
        lease: &Lease,
        now_ms: i64,
        outcomes: &[(String, RecipientOutcome, String)],
        retry_delay_ms: i64,
    ) -> Result<QueueJob> {
        // With only transient outcomes the first statement would otherwise be
        // SELECT. SQLite cannot upgrade that snapshot to a writer while an
        // idle claimant/heartbeat holds a writer reservation (SQLITE_BUSY,
        // bypassing busy_timeout). Reserve the writer before any reads, just
        // as claim does; PostgreSQL retains its ordinary transaction/fencing.
        let sqlite = self
            .db
            .pool
            .acquire()
            .await
            .map_err(db_error)?
            .backend_name()
            == "SQLite";
        let mut tx = self
            .db
            .pool
            .begin_with(if sqlite { "BEGIN IMMEDIATE" } else { "BEGIN" })
            .await
            .map_err(db_error)?;
        for (email, outcome, detail) in outcomes {
            let status = match outcome {
                RecipientOutcome::Transient => "pending",
                RecipientOutcome::Sent => "sent",
                RecipientOutcome::Failed => "failed",
                RecipientOutcome::Ambiguous => "ambiguous",
            };
            sqlx::query(
                "UPDATE delivery_recipients SET status=$1,detail=$2,attempt_token=NULL WHERE job_id=$3 AND email=$4 AND (status='pending' OR attempt_token=$5)",
            )
            .bind(status).bind(detail).bind(lease.job.id.0.to_string()).bind(email).bind(&lease.token)
            .execute(&mut *tx).await.map_err(db_error)?;
        }
        let still_pending: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM delivery_recipients WHERE job_id=$1 AND status='pending'",
        )
        .bind(lease.job.id.0.to_string())
        .fetch_one(&mut *tx)
        .await
        .map_err(db_error)?;
        let job = if still_pending > 0 {
            let due = now_ms
                .checked_add(retry_delay_ms)
                .filter(|_| retry_delay_ms >= 0)
                .ok_or_else(|| {
                    Error::Validation("retry delay must be nonnegative and not overflow".into())
                })?;
            transition_leased_job(
                &mut tx,
                lease,
                now_ms,
                due,
                "retry",
                "recipients pending retry",
            )
            .await?
        } else {
            transition_leased_job(&mut tx, lease, now_ms, now_ms, "ack", "").await?
        };
        tx.commit().await.map_err(db_error)?;
        Ok(job)
    }
    async fn transition(
        &self,
        lease: &Lease,
        now_ms: i64,
        due: i64,
        operation: &str,
        reason: &str,
    ) -> Result<QueueJob> {
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        let job = transition_leased_job(&mut tx, lease, now_ms, due, operation, reason).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(job)
    }
    /// Claim one due or expired job. `PostgreSQL` skips row locks; `SQLite` serializes
    /// writers with `BEGIN IMMEDIATE`. The returned capability fences later writes.
    /// One exhausted job is shunted per call, returning `None`; callers should keep polling.
    /// # Errors
    /// Returns validation or database errors.
    pub async fn claim(
        &self,
        queue: Queue,
        worker: &str,
        now_ms: i64,
        lease_ms: i64,
    ) -> Result<Option<Lease>> {
        let until = now_ms
            .checked_add(lease_ms)
            .filter(|_| lease_ms > 0)
            .ok_or_else(|| Error::Validation("lease must be positive and not overflow".into()))?;
        if worker.trim().is_empty() {
            return Err(Error::Validation("worker must not be empty".into()));
        }
        let sqlite = self
            .db
            .pool
            .acquire()
            .await
            .map_err(db_error)?
            .backend_name()
            == "SQLite";
        let mut tx = self
            .db
            .pool
            .begin_with(if sqlite { "BEGIN IMMEDIATE" } else { "BEGIN" })
            .await
            .map_err(db_error)?;
        let lock = if sqlite {
            ""
        } else {
            " FOR UPDATE SKIP LOCKED"
        };
        let sql = format!(
            "SELECT * FROM queue_jobs WHERE queue=$1 AND run_after <= $2 AND (state='ready' OR (state='leased' AND lease_until <= $2)) ORDER BY run_after,id LIMIT 1{lock}"
        );
        let Some(row) = sqlx::query(&sql)
            .bind(queue_name(queue))
            .bind(now_ms)
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_error)?
        else {
            tx.commit().await.map_err(db_error)?;
            return Ok(None);
        };
        let old = decode_job(&row)?;
        if old.attempts >= old.max_attempts {
            let row = sqlx::query("UPDATE queue_jobs SET queue='shunt',state='shunted',locked_by=NULL,lease_token=NULL,lease_until=NULL,last_error='lease expired at attempt limit' WHERE id=$1 RETURNING *")
                .bind(old.id.0.to_string()).fetch_one(&mut *tx).await.map_err(db_error)?;
            audit(&mut tx, &decode_job(&row)?, "queue.shunt", now_ms).await?;
            tx.commit().await.map_err(db_error)?;
            return Ok(None);
        }
        let token = Uuid::now_v7().to_string();
        let row = sqlx::query("UPDATE queue_jobs SET state='leased',attempts=attempts+1,locked_by=$1,lease_token=$2,lease_until=$3 WHERE id=$4 RETURNING *")
            .bind(worker).bind(&token).bind(until).bind(old.id.0.to_string()).fetch_one(&mut *tx).await.map_err(db_error)?;
        let job = decode_job(&row)?;
        audit(&mut tx, &job, "queue.claim", now_ms).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(Some(Lease { job, token }))
    }

    /// Store exact bytes, an independent submission index, and its first job atomically.
    /// `context` is an opaque routing label, not a mailing-list foreign key.
    /// # Errors
    /// Returns validation or database errors; no partial intake is committed.
    pub async fn enqueue(&self, input: NewMessage, now_ms: i64) -> Result<QueueJob> {
        if input.max_attempts <= 0 {
            return Err(Error::Validation("max_attempts must be positive".into()));
        }
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        let key = format!("{:x}", Sha256::digest(&input.raw));
        let message_id = MessageId(Uuid::now_v7());
        let id = JobId(Uuid::now_v7());
        sqlx::query("INSERT INTO message_blobs(store_key,raw) VALUES($1,$2) ON CONFLICT(store_key) DO NOTHING")
            .bind(&key).bind(&input.raw).execute(&mut *tx).await.map_err(db_error)?;
        sqlx::query("INSERT INTO messages(id,store_key,external_id,context,created_at) VALUES($1,$2,$3,$4,$5)")
            .bind(message_id.0.to_string()).bind(&key).bind(&input.external_id).bind(&input.context).bind(now_ms)
            .execute(&mut *tx).await.map_err(db_error)?;
        let row = sqlx::query("INSERT INTO queue_jobs(id,message_id,queue,max_attempts,run_after,state) VALUES($1,$2,$3,$4,$5,CASE WHEN $3='shunt' THEN 'shunted' ELSE 'ready' END) RETURNING *")
            .bind(id.0.to_string()).bind(message_id.0.to_string()).bind(queue_name(input.queue)).bind(input.max_attempts).bind(now_ms)
            .fetch_one(&mut *tx).await.map_err(db_error)?;
        let job = decode_job(&row)?;
        audit(&mut tx, &job, "queue.enqueue", now_ms).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(job)
    }
    /// Read the immutable original bytes and their submission index.
    /// # Errors
    /// Returns not-found or database errors.
    pub async fn message(&self, id: MessageId) -> Result<StoredMessage> {
        let row = sqlx::query("SELECT m.*, b.raw FROM messages m JOIN message_blobs b ON b.store_key=m.store_key WHERE m.id=$1")
            .bind(id.0.to_string()).fetch_optional(self.db.pool()).await.map_err(db_error)?
            .ok_or_else(|| Error::NotFound("message".into()))?;
        Ok(StoredMessage {
            id,
            store_key: row.try_get("store_key").map_err(db_error)?,
            external_id: row.try_get("external_id").map_err(db_error)?,
            context: row.try_get("context").map_err(db_error)?,
            created_at: row.try_get("created_at").map_err(db_error)?,
            raw: row.try_get("raw").map_err(db_error)?,
        })
    }
    /// Inspect a job, including retained completed and shunted jobs.
    /// # Errors
    /// Returns not-found or database errors.
    pub async fn job(&self, id: JobId) -> Result<QueueJob> {
        let row = sqlx::query("SELECT * FROM queue_jobs WHERE id=$1")
            .bind(id.0.to_string())
            .fetch_optional(self.db.pool())
            .await
            .map_err(db_error)?
            .ok_or_else(|| Error::NotFound("queue job".into()))?;
        decode_job(&row)
    }
}

pub(crate) const fn queue_name(queue: Queue) -> &'static str {
    match queue {
        Queue::In => "in",
        Queue::Pipeline => "pipeline",
        Queue::Out => "out",
        Queue::Retry => "retry",
        Queue::Bounces => "bounces",
        Queue::Command => "command",
        Queue::Virgin => "virgin",
        Queue::Archive => "archive",
        Queue::Digest => "digest",
        Queue::Nntp => "nntp",
        Queue::Shunt => "shunt",
        Queue::Bad => "bad",
    }
}
/// Fence and apply an ack/retry/shunt transition to a leased job inside an
/// already-open transaction, without committing. Shared by the instance
/// [`MailQueueRepo::transition`] wrapper and [`MailQueueRepo::finish_delivery`],
/// which composes it with fenced per-recipient outcome writes in one atomic
/// handoff.
/// # Errors
/// Returns stale-lease conflict or database errors.
async fn transition_leased_job(
    tx: &mut Transaction<'_, Any>,
    lease: &Lease,
    now_ms: i64,
    due: i64,
    operation: &str,
    reason: &str,
) -> Result<QueueJob> {
    let row = sqlx::query("UPDATE queue_jobs SET state=CASE WHEN $1='ack' THEN 'done' WHEN $1='shunt' OR attempts>=max_attempts THEN 'shunted' ELSE 'ready' END, queue=CASE WHEN $1='shunt' OR ($1='retry' AND attempts>=max_attempts) THEN 'shunt' ELSE queue END, run_after=$2, last_error=$3, locked_by=NULL, lease_token=NULL, lease_until=NULL WHERE id=$4 AND state='leased' AND lease_token=$5 AND lease_until>$6 RETURNING *")
        .bind(operation).bind(due).bind(reason).bind(lease.job.id.0.to_string()).bind(&lease.token).bind(now_ms)
        .fetch_optional(&mut **tx).await.map_err(db_error)?
        .ok_or_else(|| Error::Conflict("stale or expired queue lease".into()))?;
    let job = decode_job(&row)?;
    let action = match job.state {
        JobState::Done => "queue.ack",
        JobState::Shunted => "queue.shunt",
        _ => "queue.retry",
    };
    audit(tx, &job, action, now_ms).await?;
    Ok(job)
}
/// Fence and finish a leased job's queue-side lifecycle (mark it `done`)
/// inside an already-open transaction, without committing. Shared by
/// [`MailQueueRepo::complete_with_children`] and the moderation module, which
/// composes this with a held-message write in one atomic handoff.
/// # Errors
/// Returns stale-lease conflict or database errors.
pub(crate) async fn ack_leased_job(
    tx: &mut Transaction<'_, Any>,
    lease: &Lease,
    now_ms: i64,
) -> Result<QueueJob> {
    let row = sqlx::query("UPDATE queue_jobs SET state='done',run_after=$1,last_error='',locked_by=NULL,lease_token=NULL,lease_until=NULL WHERE id=$2 AND state='leased' AND lease_token=$3 AND lease_until>$4 RETURNING *")
        .bind(now_ms).bind(lease.job.id.0.to_string()).bind(&lease.token).bind(now_ms)
        .fetch_optional(&mut **tx).await.map_err(db_error)?
        .ok_or_else(|| Error::Conflict("stale or expired queue lease".into()))?;
    let job = decode_job(&row)?;
    audit(tx, &job, "queue.ack", now_ms).await?;
    Ok(job)
}
/// Insert one child job (and, if given, its recipient snapshot) inside an
/// already-open transaction. Shared by [`MailQueueRepo::complete_with_children`]
/// and the moderation module's held-message acceptance handoff.
pub(crate) async fn insert_child_job(
    tx: &mut Transaction<'_, Any>,
    message_id: MessageId,
    child: &ChildJob,
    now_ms: i64,
) -> Result<QueueJob> {
    let id = JobId(Uuid::now_v7());
    let row = sqlx::query("INSERT INTO queue_jobs(id,message_id,queue,max_attempts,run_after,state) VALUES($1,$2,$3,$4,$5,CASE WHEN $3='shunt' THEN 'shunted' ELSE 'ready' END) RETURNING *")
        .bind(id.0.to_string()).bind(message_id.0.to_string()).bind(queue_name(child.queue)).bind(child.max_attempts).bind(now_ms)
        .fetch_one(&mut **tx).await.map_err(db_error)?;
    let job = decode_job(&row)?;
    for email in &child.recipients {
        sqlx::query("INSERT INTO delivery_recipients(id,job_id,email) VALUES($1,$2,$3)")
            .bind(Uuid::now_v7().to_string())
            .bind(job.id.0.to_string())
            .bind(email)
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
    }
    audit(tx, &job, "queue.enqueue", now_ms).await?;
    Ok(job)
}
pub(crate) fn decode_job(row: &AnyRow) -> Result<QueueJob> {
    let queue: String = row.try_get("queue").map_err(db_error)?;
    let state: String = row.try_get("state").map_err(db_error)?;
    Ok(QueueJob {
        id: JobId(
            row.try_get::<String, _>("id")
                .map_err(db_error)?
                .parse()
                .map_err(db_error)?,
        ),
        message_id: MessageId(
            row.try_get::<String, _>("message_id")
                .map_err(db_error)?
                .parse()
                .map_err(db_error)?,
        ),
        queue: serde_json::from_value(serde_json::Value::String(queue)).map_err(db_error)?,
        state: serde_json::from_value(serde_json::Value::String(state)).map_err(db_error)?,
        attempts: row.try_get("attempts").map_err(db_error)?,
        max_attempts: row.try_get("max_attempts").map_err(db_error)?,
        run_after: row.try_get("run_after").map_err(db_error)?,
        locked_by: row.try_get("locked_by").map_err(db_error)?,
        lease_until: row.try_get("lease_until").map_err(db_error)?,
        last_error: row.try_get("last_error").map_err(db_error)?,
    })
}
pub(crate) async fn audit(
    tx: &mut Transaction<'_, Any>,
    job: &QueueJob,
    action: &str,
    now_ms: i64,
) -> Result<()> {
    let at = chrono::DateTime::from_timestamp_millis(now_ms)
        .ok_or_else(|| Error::Validation("timestamp out of range".into()))?;
    sqlx::query("INSERT INTO audit_log(id,at,action,target_type,target_id,diff) VALUES($1,$2,$3,'queue_job',$4,$5)")
        .bind(Uuid::now_v7().to_string()).bind(at.to_rfc3339()).bind(action).bind(job.id.0.to_string())
        .bind(serde_json::json!({"queue": job.queue, "state": job.state, "attempts": job.attempts, "worker": job.locked_by}).to_string())
        .execute(&mut **tx).await.map_err(db_error)?;
    Ok(())
}
