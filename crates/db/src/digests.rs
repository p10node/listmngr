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
    /// Mailman's `list:member:digest:masthead`, `:header` and `:footer`,
    /// resolved for the list's language with the issue's placeholders.
    pub masthead: String,
    pub header: String,
    pub footer: String,
}

/// The list's digest settings as the flush reads them under the lock.
struct DigestSettings {
    display_name: String,
    volume: i32,
    number: i64,
    last_sent_at: Option<chrono::DateTime<chrono::Utc>>,
    size_threshold_bytes: usize,
    send_periodic: bool,
    frequency: listmngr_core::DigestFrequency,
}

/// Lock the list row for publication and read what the flush needs.
async fn load_settings(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    list: &ListId,
) -> Result<DigestSettings> {
    let row = sqlx::query("UPDATE mailing_lists SET next_digest_number=next_digest_number WHERE list_id=$1 RETURNING display_name,volume,next_digest_number,digest_last_sent_at,digest_size_threshold,digest_send_periodic,digest_volume_frequency").bind(list.as_str()).fetch_optional(&mut **tx).await.map_err(db_error)?.ok_or_else(|| Error::NotFound(list.to_string()))?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(DigestSettings {
        display_name: row.try_get("display_name").map_err(db_error)?,
        volume: i32::try_from(row.try_get::<i64, _>("volume").map_err(db_error)?)
            .map_err(db_error)?,
        number: row.try_get("next_digest_number").map_err(db_error)?,
        last_sent_at: row
            .try_get::<Option<String>, _>("digest_last_sent_at")
            .map_err(db_error)?
            .map(|value| crate::parse_time(&value))
            .transpose()?,
        size_threshold_bytes: (row
            .try_get::<f64, _>("digest_size_threshold")
            .map_err(db_error)?
            .max(0.0)
            * 1024.0) as usize,
        send_periodic: row
            .try_get::<i64, _>("digest_send_periodic")
            .map_err(db_error)?
            != 0,
        frequency: row
            .try_get::<String, _>("digest_volume_frequency")
            .map_err(db_error)?
            .parse()
            .map_err(|_| Error::Validation("digest_volume_frequency".into()))?,
    })
}

/// Mailman's volume period: the calendar unit the volume number counts.
fn period(
    at: chrono::DateTime<chrono::Utc>,
    frequency: listmngr_core::DigestFrequency,
) -> (i32, u32) {
    use chrono::Datelike as _;
    match frequency {
        listmngr_core::DigestFrequency::Yearly => (at.year(), 0),
        listmngr_core::DigestFrequency::Monthly => (at.year(), at.month()),
        listmngr_core::DigestFrequency::Quarterly => (at.year(), (at.month() - 1) / 3),
        listmngr_core::DigestFrequency::Weekly => {
            let week = at.iso_week();
            (week.year(), week.week())
        }
        listmngr_core::DigestFrequency::Daily => (at.year(), at.ordinal()),
    }
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
        self.bump_with_context(list, &AuditContext::system()).await
    }

    /// See [`Self::bump`]; the audit row carries the caller's context.
    /// # Errors
    /// Returns missing-list, overflow, database or audit errors.
    pub async fn bump_with_context(&self, list: &ListId, context: &AuditContext) -> Result<()> {
        let mut tx = self.db.pool().begin().await.map_err(db_error)?;
        Self::bump_tx(&mut tx, list, context).await?;
        tx.commit().await.map_err(db_error)
    }

    /// [`Self::bump_with_context`] inside the caller's transaction.
    pub(crate) async fn bump_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Any>,
        list: &ListId,
        context: &AuditContext,
    ) -> Result<()> {
        let current = crate::lock_list_for_patch(tx, list).await?;
        let volume = current
            .volume
            .checked_add(1)
            .ok_or_else(|| Error::Validation("digest volume overflow".into()))?;
        sqlx::query("UPDATE mailing_lists SET volume=$1,next_digest_number=1 WHERE list_id=$2")
            .bind(i64::from(volume))
            .bind(list.as_str())
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
        Database::record_tx_with_context(
            tx,
            context,
            "digest.bump",
            "list",
            list.as_str(),
            serde_json::json!({"volume": volume}),
        )
        .await
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
        let settings = load_settings(&mut tx, list).await?;
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
        // Mailman's triggers: the size threshold (0 never), the daily
        // periodic run when the list allows it, the hard cap, or force.
        let by_size = settings.size_threshold_bytes > 0 && size >= settings.size_threshold_bytes;
        let by_period = settings.send_periodic && now_ms.saturating_sub(oldest) >= 86_400_000;
        if !force && !by_size && !by_period && rows.len() < 1000 {
            tx.commit().await.map_err(db_error)?;
            return Ok(0);
        }
        // The volume follows the calendar: a new period since the last
        // issue advances it and restarts the numbering.
        let now = chrono::DateTime::from_timestamp_millis(now_ms)
            .ok_or_else(|| Error::Validation("invalid digest timestamp".into()))?;
        let (mut volume, mut number) = (settings.volume, settings.number);
        if settings
            .last_sent_at
            .is_some_and(|last| period(last, settings.frequency) != period(now, settings.frequency))
        {
            volume = volume
                .checked_add(1)
                .ok_or_else(|| Error::Validation("digest volume overflow".into()))?;
            number = 1;
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
                serde_json::json!({"volume": volume, "frequency": settings.frequency.as_str()}),
            )
            .await?;
        }
        let snapshot = crate::notices::list_snapshot(&mut tx, list).await?;
        let language = snapshot.preferred_language.clone();
        let values = crate::notices::list_placeholders(&snapshot)
            .set("volume", volume.to_string())
            .set("issue", number.to_string());
        let mut templates = Vec::with_capacity(3);
        for name in [
            "list:member:digest:masthead",
            "list:member:digest:header",
            "list:member:digest:footer",
        ] {
            templates
                .push(crate::notices::render(&mut tx, &snapshot, name, &language, &values).await?);
        }
        let footer = templates.pop().unwrap_or_default();
        let header = templates.pop().unwrap_or_default();
        let masthead = templates.pop().unwrap_or_default();
        let issue = DigestIssue {
            list: list.clone(),
            display_name: settings.display_name,
            volume,
            number,
            timestamp: now_ms / 1000,
            posts,
            masthead,
            header,
            footer,
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

/// Render one issue into one message per recipient group.
///
/// Recipients are grouped by delivery mode and exact post set; each message
/// carries the MIME or RFC 1153 body from `listmngr_mail::digest`, the list
/// headers, a fresh `Message-ID` and the posts' loop history.
/// # Errors
/// Returns a validation error when a post's mode or the built issue is
/// invalid.
pub fn render(issue: &DigestIssue) -> Result<Vec<DigestOutput>> {
    // Per-post exclusions must not accidentally reappear in a shared digest.
    let mut membership: std::collections::BTreeMap<(String, String), Vec<usize>> =
        std::collections::BTreeMap::new();
    for (index, post) in issue.posts.iter().enumerate() {
        for recipient in &post.recipients {
            membership
                .entry((recipient.mode.clone(), recipient.email.clone()))
                .or_default()
                .push(index);
        }
    }
    let mut groups: std::collections::BTreeMap<(String, Vec<usize>), Vec<String>> =
        std::collections::BTreeMap::new();
    for ((mode, email), indices) in membership {
        groups.entry((mode, indices)).or_default().push(email);
    }
    groups
        .into_iter()
        .map(|((mode, indices), recipients)| {
            let messages: Vec<&[u8]> = indices
                .iter()
                .map(|i| issue.posts[*i].raw.as_slice())
                .collect();
            let history: std::collections::BTreeSet<_> = messages
                .iter()
                .flat_map(|raw| listmngr_mail::facts::loop_markers(raw))
                .collect();
            let raw = listmngr_mail::digest::build(&listmngr_mail::digest::Digest {
                list: issue.list.clone(),
                display_name: issue.display_name.clone(),
                volume: issue.volume,
                number: issue.number,
                mode: mode.parse()?,
                timestamp: issue.timestamp,
                masthead: issue.masthead.clone(),
                header: issue.header.clone(),
                footer: issue.footer.clone(),
                messages,
            })
            .map_err(|e| Error::Validation(e.to_string()))?;
            let mut headers = headers(&issue.list);
            headers.push((
                "Message-ID".into(),
                format!("<{}@{}>", uuid::Uuid::now_v7(), issue.list.mail_host()),
            ));
            headers.extend(history.into_iter().map(|v| ("X-BeenThere".into(), v)));
            let raw = listmngr_mail::cook_headers(&raw, None, &headers)
                .map_err(|e| Error::Validation(e.to_string()))?;
            Ok(DigestOutput {
                raw,
                recipients,
                mode,
            })
        })
        .collect()
}
fn headers(list: &ListId) -> Vec<(String, String)> {
    listmngr_pipeline::list_headers(&listmngr_pipeline::ListHeaderInfo {
        list_id: list.to_string(),
        posting_address: list.posting_address(),
        subscribe_address: list.join_address(),
        unsubscribe_address: list.leave_address(),
        archive_url: None,
    })
}
