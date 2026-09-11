//! Durable digest collection and atomic issue/outgoing-job publication.
use crate::mail_queue::{
    ChildJob, JobId, Lease, MessageId, Queue, ack_leased_job, insert_child_job,
};
use crate::{AuditContext, Database, db_error};
use listmngr_core::{Error, ListId, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigestRecipient {
    pub email: String,
    pub mode: String,
}
#[derive(Debug)]
pub struct DigestPost {
    pub raw: Vec<u8>,
    pub recipients: Vec<DigestRecipient>,
}
#[derive(Debug)]
pub struct DigestIssue {
    pub list: ListId,
    pub display_name: String,
    pub volume: i32,
    pub number: i64,
    pub timestamp: i64,
    pub posts: Vec<DigestPost>,
}
#[derive(Debug)]
pub struct DigestOutput {
    pub raw: Vec<u8>,
    pub recipients: Vec<String>,
    pub mode: String,
}
#[derive(Debug, Clone, Copy)]
pub struct DigestRepo<'a> {
    db: &'a Database,
    clock: Option<&'a dyn crate::mail_queue::LeaseClock>,
}
impl Database {
    #[must_use]
    pub const fn digests(&self) -> DigestRepo<'_> {
        DigestRepo {
            db: self,
            clock: None,
        }
    }
}
fn valid_mode(mode: &str) -> bool {
    matches!(
        mode,
        "plaintext_digests" | "mime_digests" | "summary_digests"
    )
}
pub(crate) async fn delete_list(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    list: &ListId,
) -> Result<()> {
    // A list row lock is also the collector/publication mutex.
    sqlx::query("UPDATE mailing_lists SET next_digest_number=next_digest_number WHERE list_id=$1")
        .bind(list.as_str())
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    let rows=sqlx::query("SELECT q.id,q.message_id,m.store_key FROM digest_deliveries d JOIN digest_issues i ON i.id=d.issue_id JOIN queue_jobs q ON q.id=d.job_id JOIN messages m ON m.id=q.message_id WHERE i.list_id=$1").bind(list.as_str()).fetch_all(&mut **tx).await.map_err(db_error)?;
    for row in rows {
        let job: String = row.try_get("id").map_err(db_error)?;
        let message: String = row.try_get("message_id").map_err(db_error)?;
        let key: String = row.try_get("store_key").map_err(db_error)?;
        for table in ["digest_deliveries", "delivery_recipients"] {
            sqlx::query(&format!("DELETE FROM {table} WHERE job_id=$1"))
                .bind(&job)
                .execute(&mut **tx)
                .await
                .map_err(db_error)?;
        }
        sqlx::query("DELETE FROM queue_jobs WHERE id=$1")
            .bind(job)
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
        sqlx::query("DELETE FROM messages WHERE id=$1")
            .bind(message)
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
        sqlx::query("DELETE FROM message_blobs WHERE store_key=$1 AND NOT EXISTS(SELECT 1 FROM messages WHERE store_key=$1)").bind(key).execute(&mut **tx).await.map_err(db_error)?;
    }
    for table in ["digest_posts", "digest_issues"] {
        sqlx::query(&format!("DELETE FROM {table} WHERE list_id=$1"))
            .bind(list.as_str())
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
    }
    Ok(())
}
impl<'a> DigestRepo<'a> {
    /// Inject a post-lock lease clock; explicit timestamps remain the default.
    #[must_use]
    pub fn with_clock(mut self, clock: &'a dyn crate::mail_queue::LeaseClock) -> Self {
        self.clock = Some(clock);
        self
    }
    /// Production completion uses wall time after acquiring authority locks.
    #[must_use]
    pub fn live(self) -> Self {
        self.with_clock(&crate::mail_queue::SystemLeaseClock)
    }
    /// Advance the digest volume and reset numbering under the publication lock.
    /// # Errors
    /// Returns missing-list, overflow, database or audit errors.
    pub async fn bump(&self, list: &ListId) -> Result<()> {
        let mut tx = self.db.pool().begin().await.map_err(db_error)?;
        let current = crate::lock_list_for_patch(&mut tx, list).await?;
        let volume = current
            .volume
            .checked_add(1)
            .ok_or_else(|| Error::Validation("digest volume overflow".into()))?;
        sqlx::query("UPDATE mailing_lists SET volume=$1,next_digest_number=1 WHERE list_id=$2")
            .bind(i64::from(volume))
            .bind(list.as_str())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        Database::record_tx_with_context(
            &mut tx,
            &AuditContext::system(),
            "digest.bump",
            "list",
            list.as_str(),
            serde_json::json!({"volume": volume}),
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }

    /// Fences collection with the source digest lease; retries cannot append twice.
    /// # Errors
    /// Returns invalid input, stale lease or database errors.
    pub async fn collect(
        &self,
        lease: &Lease,
        list: &ListId,
        raw: &[u8],
        recipients: &[DigestRecipient],
        now_ms: i64,
    ) -> Result<()> {
        if lease.job.queue != Queue::Digest
            || raw.is_empty()
            || raw.len() > 10 * 1024 * 1024
            || recipients.iter().any(|r| !valid_mode(&r.mode))
        {
            return Err(Error::Validation("invalid digest collection".into()));
        }
        let mut tx = self.db.pool().begin().await.map_err(db_error)?;
        // Serialize with issue publication BEFORE reading pending rows (PG + SQLite).
        sqlx::query(
            "UPDATE mailing_lists SET next_digest_number=next_digest_number WHERE list_id=$1",
        )
        .bind(list.as_str())
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        let queue = self.db.mail_queue();
        let queue = self.clock.map_or(queue, |clock| queue.with_clock(clock));
        let now_ms = queue.lock_time(&mut tx, lease, now_ms).await?;
        let deadline = crate::mail_queue::MailQueueRepo::locked_deadline(&mut tx, lease).await?;
        sqlx::query("INSERT INTO digest_posts(id,list_id,raw,recipients,accepted_at) VALUES($1,$2,$3,$4,$5) ON CONFLICT(id) DO NOTHING")
            .bind(lease.job.message_id.0.to_string()).bind(list.as_str()).bind(raw).bind(serde_json::to_string(recipients).map_err(db_error)?).bind(now_ms).execute(&mut *tx).await.map_err(db_error)?;
        // The insert may acquire FK/index locks: do not carry pre-insert time.
        ack_leased_job(&mut tx, lease, queue.time(now_ms)).await?;
        // ACK clears the lease, then awaits its audit: retain locked authority.
        queue.check_final_deadline(Some(deadline), now_ms)?;
        tx.commit().await.map_err(db_error)
    }
    /// Flush at 1 MiB/1000 posts or after 24h from the oldest pending post.
    /// Render is synchronous and runs under the list lock: failure rolls back
    /// counter, audit, post assignment, blobs, jobs and recipient snapshots.
    /// # Errors
    /// Returns render, invalid snapshot, counter, database or audit errors.
    #[allow(clippy::too_many_lines)] // Keep the publication transaction explicit.
    pub async fn flush<F>(
        &self,
        list: &ListId,
        now_ms: i64,
        force: bool,
        render: F,
    ) -> Result<usize>
    where
        F: FnOnce(&DigestIssue) -> Result<Vec<DigestOutput>>,
    {
        let mut tx = self.db.pool().begin().await.map_err(db_error)?;
        let settings = sqlx::query("UPDATE mailing_lists SET next_digest_number=next_digest_number WHERE list_id=$1 RETURNING display_name,volume,next_digest_number").bind(list.as_str()).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or_else(|| Error::NotFound(list.to_string()))?;
        let rows = sqlx::query("SELECT id,raw,recipients,accepted_at FROM digest_posts WHERE list_id=$1 AND issue_id IS NULL ORDER BY accepted_at,id LIMIT 1000").bind(list.as_str()).fetch_all(&mut *tx).await.map_err(db_error)?;
        if rows.is_empty() {
            tx.commit().await.map_err(db_error)?;
            return Ok(0);
        }
        let oldest: i64 = rows[0].try_get("accepted_at").map_err(db_error)?;
        let mut ids = Vec::new();
        let mut posts = Vec::new();
        let mut size = 0;
        for row in &rows {
            let raw: Vec<u8> = row.try_get("raw").map_err(db_error)?;
            if size + raw.len() > 10 * 1024 * 1024 {
                break;
            }
            size += raw.len();
            ids.push(row.try_get::<String, _>("id").map_err(db_error)?);
            posts.push(DigestPost {
                raw,
                recipients: serde_json::from_str(
                    &row.try_get::<String, _>("recipients").map_err(db_error)?,
                )
                .map_err(db_error)?,
            });
        }
        if !force
            && size < 1024 * 1024
            && rows.len() < 1000
            && now_ms.saturating_sub(oldest) < 86_400_000
        {
            tx.commit().await.map_err(db_error)?;
            return Ok(0);
        }
        let issue = DigestIssue {
            list: list.clone(),
            display_name: settings.try_get("display_name").map_err(db_error)?,
            volume: i32::try_from(settings.try_get::<i64, _>("volume").map_err(db_error)?)
                .map_err(db_error)?,
            number: settings.try_get("next_digest_number").map_err(db_error)?,
            timestamp: now_ms / 1000,
            posts,
        };
        let outputs = render(&issue)?;
        if outputs
            .iter()
            .any(|o| !valid_mode(&o.mode) || o.raw.is_empty() || o.recipients.is_empty())
        {
            return Err(Error::Validation("invalid digest output".into()));
        }
        // No eligible subscriber may disappear silently at the renderer boundary.
        let expected: std::collections::BTreeSet<_> = issue
            .posts
            .iter()
            .flat_map(|p| {
                p.recipients
                    .iter()
                    .map(|r| (r.email.clone(), r.mode.clone()))
            })
            .collect();
        let actual: std::collections::BTreeSet<_> = outputs
            .iter()
            .flat_map(|o| o.recipients.iter().map(|r| (r.clone(), o.mode.clone())))
            .collect();
        if expected != actual
            || actual.len() != outputs.iter().map(|o| o.recipients.len()).sum::<usize>()
        {
            return Err(Error::Validation(
                "digest recipient snapshot mismatch".into(),
            ));
        }
        let issue_id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO digest_issues(id,list_id,volume,number,created_at) VALUES($1,$2,$3,$4,$5)",
        )
        .bind(&issue_id)
        .bind(list.as_str())
        .bind(i64::from(issue.volume))
        .bind(issue.number)
        .bind(now_ms)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        for output in outputs {
            let key = format!("{:x}", Sha256::digest(&output.raw));
            let message = MessageId(Uuid::now_v7());
            sqlx::query("INSERT INTO message_blobs(store_key,raw) VALUES($1,$2) ON CONFLICT(store_key) DO NOTHING").bind(&key).bind(&output.raw).execute(&mut *tx).await.map_err(db_error)?;
            sqlx::query("INSERT INTO messages(id,store_key,external_id,context,created_at) VALUES($1,$2,$3,$4,$5)").bind(message.0.to_string()).bind(key).bind(&issue_id).bind(serde_json::json!({"list_id":list,"kind":"digest"}).to_string()).bind(now_ms).execute(&mut *tx).await.map_err(db_error)?;
            let job = insert_child_job(
                &mut tx,
                message,
                &ChildJob {
                    queue: Queue::Out,
                    max_attempts: 8,
                    recipients: output.recipients,
                },
                now_ms,
            )
            .await?;
            sqlx::query("INSERT INTO digest_deliveries(job_id,issue_id,mode) VALUES($1,$2,$3)")
                .bind(job.id.0.to_string())
                .bind(&issue_id)
                .bind(output.mode)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
        }
        for id in ids {
            sqlx::query("UPDATE digest_posts SET issue_id=$1 WHERE id=$2")
                .bind(&issue_id)
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
        }
        sqlx::query("UPDATE mailing_lists SET next_digest_number=next_digest_number+1,digest_last_sent_at=$1 WHERE list_id=$2").bind(chrono::DateTime::from_timestamp_millis(now_ms).ok_or_else(|| Error::Validation("invalid digest timestamp".into()))?.to_rfc3339()).bind(list.as_str()).execute(&mut *tx).await.map_err(db_error)?;
        Database::record_tx_with_context(
            &mut tx,
            &AuditContext::system(),
            "digest.publish",
            "list",
            list.as_str(),
            serde_json::json!({"issue_id":issue_id,"number":issue.number}),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(1)
    }
    /// Only DB-proven digest jobs may bypass ordinary cooking. Context supplied
    /// by an inbound author is never sufficient to select this path.
    /// # Errors
    /// Returns database errors.
    pub async fn is_delivery(&self, job: JobId) -> Result<bool> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM digest_deliveries WHERE job_id=$1")
                .bind(job.0.to_string())
                .fetch_one(self.db.pool())
                .await
                .map_err(db_error)?;
        Ok(count == 1)
    }
}
