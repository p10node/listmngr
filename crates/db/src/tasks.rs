//! Mailman's task runner and `notify` command.
//!
//! The sweep is housekeeping nothing else does on its own schedule: it
//! expires confirmations nobody answered, probes nobody bounced and the
//! per-writer cooldown rows, collects finished queue jobs — and the
//! messages, blobs and disposed held rows nothing references any more —
//! once they are older than the retention, and forgets bounce scores older
//! than the list's `bounce_info_stale_after`. Every step is a bounded
//! batch in its own transaction with its own audit row, so a huge backlog
//! never holds one lock for long and a crash mid-sweep loses nothing.
//!
//! `notify` is Mailman's daily reminder: owners and moderators of every
//! list with something waiting get `list:admin:notice:pending`.
use crate::{AuditContext, Database, db_error, workflows};
use listmngr_core::{ListId, Result};
use serde::Serialize;
use sqlx::{Any, Row, Transaction};
use std::fmt::Write as _;

/// Rows one statement touches; the sweep loops until a batch comes up short.
const BATCH: i64 = 100;
/// Batches per step per sweep, so one call always ends.
const MAX_BATCHES: usize = 100;
/// The help command's per-address cooldown.
const HELP_COOLDOWN_MS: i64 = 3_600_000;
/// How long the posting-rate ledger keeps a row: a day, the longest
/// window `security.rate_limit.post` can name.
const POSTING_RATE_LIFE_MS: i64 = 86_400_000;
/// Entries listed per section of the pending notice.
const LISTED: usize = 50;

/// What one sweep did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct TaskSummary {
    pub expired_workflows: u64,
    pub expired_probes: u64,
    pub expired_help_requests: u64,
    pub expired_autoresponses: u64,
    pub collected_jobs: u64,
    pub collected_held: u64,
    pub collected_messages: u64,
    pub stale_bounces_reset: u64,
    /// Webhook deliveries posted or given up longer ago than the retention.
    pub collected_webhook_deliveries: u64,
    /// Objects of the `fs` or `s3` message store no row names any more,
    /// older than the store's grace period.
    pub collected_blobs: u64,
    /// Posting-rate ledger rows older than a day, the longest window.
    pub expired_posting_rate: u64,
}

impl TaskSummary {
    /// Whether the sweep changed anything at all.
    #[must_use]
    pub const fn changed(&self) -> bool {
        self.expired_workflows
            + self.expired_probes
            + self.expired_help_requests
            + self.expired_autoresponses
            + self.collected_jobs
            + self.collected_held
            + self.collected_messages
            + self.stale_bounces_reset
            + self.collected_webhook_deliveries
            + self.collected_blobs
            + self.expired_posting_rate
            > 0
    }
}

/// What a list's moderators still owe.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PendingSummary {
    pub held_messages: u64,
    pub subscriptions: u64,
    pub unsubscriptions: u64,
}

impl PendingSummary {
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.held_messages + self.subscriptions + self.unsubscriptions
    }
}

#[derive(Debug)]
pub struct TaskRepo<'a> {
    db: &'a Database,
}

impl Database {
    #[must_use]
    pub const fn tasks(&self) -> TaskRepo<'_> {
        TaskRepo { db: self }
    }
}

impl TaskRepo<'_> {
    /// One sweep at `now_ms`; finished work older than `retention_ms` is
    /// collected.
    /// # Errors
    /// Returns a validation error for a non-positive retention, or the
    /// first database error; batches already committed stay.
    pub async fn sweep(&self, now_ms: i64, retention_ms: i64) -> Result<TaskSummary> {
        if retention_ms <= 0 {
            return Err(listmngr_core::Error::Validation(
                "retention must be positive".into(),
            ));
        }
        let cutoff = now_ms.saturating_sub(retention_ms);
        let expired_workflows = self
            .purge(
                "workflows",
                "DELETE FROM subscription_workflows WHERE id IN (SELECT id FROM subscription_workflows WHERE expires_at<=$1 AND state<>'pending_moderation' ORDER BY expires_at,id LIMIT $2)",
                now_ms,
            )
            .await?;
        let expired_probes = self
            .purge(
                "probes",
                "DELETE FROM bounce_probes WHERE token_hash IN (SELECT token_hash FROM bounce_probes WHERE expires_at<=$1 ORDER BY expires_at,token_hash LIMIT $2)",
                now_ms,
            )
            .await?;
        let expired_help_requests = self
            .purge(
                "help_requests",
                "DELETE FROM email_help_requests WHERE (list_id,email) IN (SELECT list_id,email FROM email_help_requests WHERE requested_at<=$1 ORDER BY requested_at,list_id,email LIMIT $2)",
                now_ms.saturating_sub(HELP_COOLDOWN_MS),
            )
            .await?;
        let expired_autoresponses = self
            .purge(
                "autoresponses",
                "DELETE FROM autoresponse_records WHERE (list_id,email,kind) IN (SELECT r.list_id,r.email,r.kind FROM autoresponse_records r JOIN mailing_lists l ON l.list_id=r.list_id WHERE r.responded_at<=$1-CAST(l.autoresponse_grace_period AS BIGINT)*86400000 ORDER BY r.responded_at,r.list_id,r.email,r.kind LIMIT $2)",
                now_ms,
            )
            .await?;
        let collected_jobs = self.collect_jobs(now_ms, cutoff).await?;
        let collected_held = self
            .purge_with(
                "held",
                cutoff,
                &[
                    "DELETE FROM moderation_log WHERE held_id IN (SELECT id FROM held_messages WHERE disposition IS NOT NULL AND disposed_at<=$1 ORDER BY disposed_at,id LIMIT $2)",
                    "DELETE FROM held_messages WHERE id IN (SELECT id FROM held_messages WHERE disposition IS NOT NULL AND disposed_at<=$1 ORDER BY disposed_at,id LIMIT $2)",
                ],
            )
            .await?;
        let collected_messages = self.collect_messages(cutoff).await?;
        let stale_bounces_reset = self.reset_stale_bounces(now_ms).await?;
        let collected_webhook_deliveries = self
            .purge(
                "webhook_deliveries",
                "DELETE FROM webhook_deliveries WHERE id IN (SELECT id FROM webhook_deliveries WHERE state<>'pending' AND finished_at<=$1 ORDER BY finished_at,id LIMIT $2)",
                cutoff,
            )
            .await?;
        let expired_posting_rate = self
            .purge(
                "posting_rate",
                "DELETE FROM posting_rate WHERE (list_id,email,posted_at) IN (SELECT list_id,email,posted_at FROM posting_rate WHERE posted_at<=$1 ORDER BY posted_at,list_id,email LIMIT $2)",
                now_ms.saturating_sub(POSTING_RATE_LIFE_MS),
            )
            .await?;
        let collected_blobs = self.db.blobs().sweep_orphans(self.db, now_ms).await?;
        if collected_blobs > 0 {
            let mut tx = self.db.write_tx().await?;
            audit(&mut tx, "blobs", collected_blobs).await?;
            tx.commit().await.map_err(db_error)?;
        }
        Ok(TaskSummary {
            expired_workflows,
            expired_probes,
            expired_help_requests,
            expired_autoresponses,
            collected_jobs,
            collected_held,
            collected_messages,
            stale_bounces_reset,
            collected_webhook_deliveries,
            collected_blobs,
            expired_posting_rate,
        })
    }

    /// Run `sql` (bound to `value` and the batch size) until a batch comes
    /// up short, each batch in its own audited transaction.
    async fn purge(&self, step: &str, sql: &str, value: i64) -> Result<u64> {
        self.purge_with(step, value, &[sql]).await
    }

    /// [`Self::purge`] over several statements sharing one bound value;
    /// the last statement's count is the batch's.
    async fn purge_with(&self, step: &str, value: i64, statements: &[&str]) -> Result<u64> {
        let mut total = 0;
        for _ in 0..MAX_BATCHES {
            let mut tx = self.db.write_tx().await?;
            let mut deleted = 0;
            for sql in statements {
                deleted = sqlx::query(sql)
                    .bind(value)
                    .bind(BATCH)
                    .execute(&mut *tx)
                    .await
                    .map_err(db_error)?
                    .rows_affected();
            }
            if deleted > 0 {
                audit(&mut tx, step, deleted).await?;
            }
            tx.commit().await.map_err(db_error)?;
            total += deleted;
            if i64::try_from(deleted).is_ok_and(|n| n < BATCH) {
                break;
            }
        }
        Ok(total)
    }

    /// Finished jobs acknowledged before `cutoff`, never one a DSN could
    /// still answer. Their recipient snapshots, notice provenance and
    /// issuances go with them; the message stays for [`Self::collect_messages`].
    async fn collect_jobs(&self, now_ms: i64, cutoff: i64) -> Result<u64> {
        let mut total = 0;
        for _ in 0..MAX_BATCHES {
            let mut tx = self.db.write_tx().await?;
            let ids: Vec<String> = sqlx::query_scalar(
                "SELECT q.id FROM queue_jobs q WHERE q.state='done' AND q.run_after<=$1 AND NOT EXISTS(SELECT 1 FROM dsn_issuances d WHERE d.job_id=q.id AND d.expires_at>$2) ORDER BY q.run_after,q.id LIMIT $3",
            )
            .bind(cutoff)
            .bind(now_ms)
            .bind(BATCH)
            .fetch_all(&mut *tx)
            .await
            .map_err(db_error)?;
            for id in &ids {
                sqlx::query("DELETE FROM dsn_issuances WHERE job_id=$1")
                    .bind(id)
                    .execute(&mut *tx)
                    .await
                    .map_err(db_error)?;
                sqlx::query("DELETE FROM queue_jobs WHERE id=$1 AND state='done'")
                    .bind(id)
                    .execute(&mut *tx)
                    .await
                    .map_err(db_error)?;
            }
            let deleted = ids.len() as u64;
            if deleted > 0 {
                audit(&mut tx, "jobs", deleted).await?;
            }
            tx.commit().await.map_err(db_error)?;
            total += deleted;
            if i64::try_from(deleted).is_ok_and(|n| n < BATCH) {
                break;
            }
        }
        Ok(total)
    }

    /// Messages older than `cutoff` that no job, held row or issuance
    /// references, and the blobs left without a message.
    async fn collect_messages(&self, cutoff: i64) -> Result<u64> {
        let mut total = 0;
        for _ in 0..MAX_BATCHES {
            let mut tx = self.db.write_tx().await?;
            let rows = sqlx::query(
                "SELECT m.id,m.store_key FROM messages m WHERE m.created_at<=$1 AND NOT EXISTS(SELECT 1 FROM queue_jobs q WHERE q.message_id=m.id) AND NOT EXISTS(SELECT 1 FROM held_messages h WHERE h.message_id=m.id) AND NOT EXISTS(SELECT 1 FROM dsn_issuances d WHERE d.message_id=m.id) ORDER BY m.created_at,m.id LIMIT $2",
            )
            .bind(cutoff)
            .bind(BATCH)
            .fetch_all(&mut *tx)
            .await
            .map_err(db_error)?;
            for row in &rows {
                let id: String = row.try_get("id").map_err(db_error)?;
                let key: String = row.try_get("store_key").map_err(db_error)?;
                for sql in [
                    "DELETE FROM message_delivery_bindings WHERE message_id=$1",
                    "DELETE FROM messages WHERE id=$1",
                ] {
                    sqlx::query(sql)
                        .bind(&id)
                        .execute(&mut *tx)
                        .await
                        .map_err(db_error)?;
                }
                sqlx::query("DELETE FROM message_blobs WHERE store_key=$1 AND NOT EXISTS(SELECT 1 FROM messages WHERE store_key=$1)")
                    .bind(&key)
                    .execute(&mut *tx)
                    .await
                    .map_err(db_error)?;
            }
            let deleted = rows.len() as u64;
            if deleted > 0 {
                audit(&mut tx, "messages", deleted).await?;
            }
            tx.commit().await.map_err(db_error)?;
            total += deleted;
            if i64::try_from(deleted).is_ok_and(|n| n < BATCH) {
                break;
            }
        }
        Ok(total)
    }

    /// A score whose last bounce is older than the list's
    /// `bounce_info_stale_after` is forgotten, as the next bounce would
    /// have started it over anyway.
    async fn reset_stale_bounces(&self, now_ms: i64) -> Result<u64> {
        let Some(now) = chrono::DateTime::from_timestamp_millis(now_ms) else {
            return Err(listmngr_core::Error::Validation("invalid time".into()));
        };
        let mut total = 0;
        let mut after = String::new();
        for _ in 0..MAX_BATCHES {
            let rows = sqlx::query(
                "SELECT m.id,m.last_bounce_received,l.bounce_info_stale_after FROM members m JOIN mailing_lists l ON l.list_id=m.list_id WHERE m.role='member' AND m.bounce_score>0 AND m.last_bounce_received IS NOT NULL AND m.id>$1 ORDER BY m.id LIMIT $2",
            )
            .bind(&after)
            .bind(BATCH)
            .fetch_all(self.db.pool())
            .await
            .map_err(db_error)?;
            let mut stale = Vec::new();
            for row in &rows {
                let id: String = row.try_get("id").map_err(db_error)?;
                let received: String = row.try_get("last_bounce_received").map_err(db_error)?;
                let days: i64 = row.try_get("bounce_info_stale_after").map_err(db_error)?;
                after.clone_from(&id);
                let Ok(at) = chrono::DateTime::parse_from_rfc3339(&received) else {
                    continue;
                };
                if now.signed_duration_since(at) >= chrono::Duration::days(days) {
                    stale.push((id, received));
                }
            }
            if !stale.is_empty() {
                let mut tx = self.db.write_tx().await?;
                for (id, received) in &stale {
                    // Fenced on the receipt read above: a bounce scored in
                    // between is a fresh score, not a stale one.
                    let reset = sqlx::query("UPDATE members SET bounce_score=0,last_bounce_received=NULL WHERE id=$1 AND last_bounce_received=$2")
                        .bind(id)
                        .bind(received)
                        .execute(&mut *tx)
                        .await
                        .map_err(db_error)?
                        .rows_affected();
                    if reset > 0 {
                        Database::record_tx_with_context(
                            &mut tx,
                            &AuditContext::system(),
                            "bounce.stale_reset",
                            "member",
                            id,
                            serde_json::json!({"last_bounce_received": received}),
                        )
                        .await?;
                        total += reset;
                    }
                }
                tx.commit().await.map_err(db_error)?;
            }
            if i64::try_from(rows.len()).is_ok_and(|n| n < BATCH) {
                break;
            }
        }
        Ok(total)
    }

    /// What `list`'s moderators still owe.
    /// # Errors
    /// Returns a database error.
    pub async fn pending(&self, list: &ListId) -> Result<PendingSummary> {
        let row = sqlx::query(
            "SELECT (SELECT COUNT(*) FROM held_messages WHERE list_id=$1 AND disposition IS NULL) AS held, (SELECT COUNT(*) FROM subscription_workflows WHERE list_id=$1 AND state='pending_moderation' AND action='join') AS subs, (SELECT COUNT(*) FROM subscription_workflows WHERE list_id=$1 AND state='pending_moderation' AND action='leave') AS unsubs",
        )
        .bind(list.as_str())
        .fetch_one(self.db.pool())
        .await
        .map_err(db_error)?;
        let count = |name: &str| -> Result<u64> {
            let value: i64 = row.try_get(name).map_err(db_error)?;
            u64::try_from(value).map_err(|_| listmngr_core::Error::Database("count".into()))
        };
        Ok(PendingSummary {
            held_messages: count("held")?,
            subscriptions: count("subs")?,
            unsubscriptions: count("unsubs")?,
        })
    }

    /// Mailman's `notify`: every list with something waiting reminds its
    /// owners and moderators. Returns how many lists were notified.
    /// # Errors
    /// Returns a database error; lists already notified stay notified.
    pub async fn notify(&self, now_ms: i64) -> Result<usize> {
        let lists: Vec<String> =
            sqlx::query_scalar("SELECT list_id FROM mailing_lists ORDER BY list_id")
                .fetch_all(self.db.pool())
                .await
                .map_err(db_error)?;
        let mut notified = 0;
        for list in lists {
            if self.notify_list(&list.parse()?, now_ms).await? {
                notified += 1;
            }
        }
        Ok(notified)
    }

    /// [`Self::notify`] for one list; `false` when nothing is waiting.
    /// # Errors
    /// Returns a database error.
    pub async fn notify_list(&self, list: &ListId, now_ms: i64) -> Result<bool> {
        let pending = self.pending(list).await?;
        if pending.total() == 0 {
            return Ok(false);
        }
        let mut tx = self.db.write_tx().await?;
        let recipients: Vec<String> = sqlx::query_scalar("SELECT a.original_email FROM addresses a WHERE EXISTS (SELECT 1 FROM members m WHERE m.address_id=a.id AND m.list_id=$1 AND m.role IN ('owner','moderator')) ORDER BY a.email")
            .bind(list.as_str()).fetch_all(&mut *tx).await.map_err(db_error)?;
        let snapshot = crate::notices::list_snapshot(&mut tx, list).await?;
        let posting_address = list.posting_address();
        let count = pending.total().to_string();
        let mut sent = 0_usize;
        for email in &recipients {
            if listmngr_mail::owner::points_to_list(email, list) {
                continue;
            }
            let language = crate::notices::recipient_language(
                &mut tx,
                &snapshot,
                email,
                self.db.default_language(),
            )
            .await?;
            let data = pending_data(&mut tx, list, &pending, &language).await?;
            workflows::enqueue_templated_notice(
                &mut tx,
                self.db,
                list,
                workflows::Notice {
                    to: email,
                    reply_to: None,
                    subject: "notice-pending-subject",
                    subject_args: &[("listname", &posting_address), ("count", &count)],
                    template: "list:admin:notice:pending",
                },
                |values| values.set("count", count.clone()).set("data", data),
                now_ms,
            )
            .await?;
            sent += 1;
        }
        Database::record_tx_with_context(
            &mut tx,
            &AuditContext::system(),
            "list.notify",
            "list",
            list.as_str(),
            serde_json::json!({
                "held_messages": pending.held_messages,
                "subscriptions": pending.subscriptions,
                "unsubscriptions": pending.unsubscriptions,
                "recipients": sent,
            }),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(true)
    }
}

/// The `$data` block: each non-empty section with its first entries.
async fn pending_data(
    tx: &mut Transaction<'_, Any>,
    list: &ListId,
    pending: &PendingSummary,
    language: &str,
) -> Result<String> {
    let limit = i64::try_from(LISTED).unwrap_or(i64::MAX);
    let mut data = String::new();
    if pending.held_messages > 0 {
        let rows = sqlx::query("SELECT sender,subject FROM held_messages WHERE list_id=$1 AND disposition IS NULL ORDER BY hold_date,id LIMIT $2")
            .bind(list.as_str()).bind(limit).fetch_all(&mut **tx).await.map_err(db_error)?;
        let entries = rows
            .iter()
            .map(|row| {
                let sender: String = row.try_get("sender").map_err(db_error)?;
                let subject: String = row.try_get("subject").map_err(db_error)?;
                let subject = if subject.trim().is_empty() {
                    listmngr_i18n::message(language, "notice-no-subject", &[])
                } else {
                    subject
                };
                Ok(format!("{sender}: {subject}"))
            })
            .collect::<Result<Vec<_>>>()?;
        section(
            &mut data,
            language,
            "notify-held-messages",
            &entries,
            pending.held_messages,
        );
    }
    for (action, key, count) in [
        ("join", "notify-held-subscriptions", pending.subscriptions),
        (
            "leave",
            "notify-held-unsubscriptions",
            pending.unsubscriptions,
        ),
    ] {
        if count == 0 {
            continue;
        }
        let entries: Vec<String> = sqlx::query_scalar("SELECT original_email FROM subscription_workflows WHERE list_id=$1 AND state='pending_moderation' AND action=$2 ORDER BY created_at,id LIMIT $3")
            .bind(list.as_str()).bind(action).bind(limit).fetch_all(&mut **tx).await.map_err(db_error)?;
        section(&mut data, language, key, &entries, count);
    }
    Ok(data)
}

fn section(data: &mut String, language: &str, heading: &str, entries: &[String], count: u64) {
    if !data.is_empty() {
        data.push('\n');
    }
    data.push_str(&listmngr_i18n::message(language, heading, &[]));
    data.push('\n');
    for entry in entries {
        let _ = writeln!(data, "    {}", entry.replace(['\r', '\n'], " "));
    }
    let more = count.saturating_sub(entries.len() as u64);
    if more > 0 {
        let _ = writeln!(
            data,
            "    {}",
            listmngr_i18n::message(language, "notify-more", &[("count", &more.to_string())])
        );
    }
}

async fn audit(tx: &mut Transaction<'_, Any>, step: &str, deleted: u64) -> Result<()> {
    Database::record_tx_with_context(
        tx,
        &AuditContext::system(),
        "task.sweep",
        "task",
        step,
        serde_json::json!({"deleted": deleted}),
    )
    .await
}
