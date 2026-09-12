//! Policy-gated durable archive. SQL substring search is bounded, not full-text ranking.
use crate::mail_queue::{Lease, Queue, ack_leased_job};
use crate::{Database, TokenAuth, db_error};
use base64::Engine;
use listmngr_core::{Error, ListId, Result};
use serde::Serialize;
use sqlx::Row;
#[path = "browser_archive.rs"]
mod browser;
enum Selection<'a> {
    Thread(Option<&'a str>),
    Message(&'a str),
}
#[cfg(test)]
#[path = "archive_tests.rs"]
mod tests;
#[derive(Debug, Serialize)]
pub struct ArchiveMessage {
    pub hash: String,
    pub thread: String,
    pub subject: String,
    pub body: String,
    #[serde(skip)]
    pub raw: Vec<u8>,
}
#[derive(Debug)]
pub struct ArchiveRepo<'a> {
    db: &'a Database,
    clock: Option<&'a dyn crate::mail_queue::LeaseClock>,
}
impl Database {
    #[must_use]
    pub const fn archive(&self) -> ArchiveRepo<'_> {
        ArchiveRepo {
            db: self,
            clock: None,
        }
    }
}
fn render_rows(
    mut settings: listmngr_core::MailingList,
    rows: &[sqlx::any::AnyRow],
    base_url: Option<&str>,
) -> Result<Vec<ArchiveMessage>> {
    rows.iter()
        .map(|r| {
            // Publication policy is selected with the authorized bytes.
            settings.anonymous_list = r.try_get::<i64, _>("anonymous_list").map_err(db_error)? != 0;
            settings.subject_prefix = r.try_get("subject_prefix").map_err(db_error)?;
            let hash: String = r.try_get("hash").map_err(db_error)?;
            let stored = base64::engine::general_purpose::STANDARD
                .decode(r.try_get::<String, _>("raw_b64").map_err(db_error)?)
                .map_err(db_error)?;
            let raw = publication(&stored, &settings, &hash, base_url)?;
            let parsed = mail_parser::MessageParser::default()
                .parse(&raw)
                .ok_or_else(|| Error::Validation("invalid archive MIME".into()))?;
            Ok(ArchiveMessage {
                hash,
                thread: r.try_get("thread").map_err(db_error)?,
                subject: parsed.subject().unwrap_or("").into(),
                body: parsed.body_text(0).unwrap_or_default().into_owned(),
                raw,
            })
        })
        .collect()
}

/// The archive copy: the posting pipeline up to `to-archive`.
fn publication(
    raw: &[u8],
    list: &listmngr_core::MailingList,
    identity: &str,
    base_url: Option<&str>,
) -> Result<Vec<u8>> {
    listmngr_mail::handlers::cook_for_site(
        listmngr_mail::handlers::Target::Archive,
        raw,
        list,
        identity,
        base_url,
    )
    .map_err(|e| Error::Validation(e.to_string()))
}
/// Schedule an accepted held post inside its existing disposition transaction.
pub(crate) async fn schedule_accepted(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    list: &ListId,
    message: crate::mail_queue::MessageId,
    now_ms: i64,
) -> Result<()> {
    let policy: String = sqlx::query_scalar("UPDATE mailing_lists SET archive_policy=archive_policy WHERE list_id=$1 RETURNING archive_policy").bind(list.as_str()).fetch_one(&mut **tx).await.map_err(db_error)?;
    if policy == "public" || policy == "private" {
        crate::mail_queue::insert_child_job(
            tx,
            message,
            &crate::mail_queue::ChildJob {
                queue: Queue::Archive,
                max_attempts: 5,
                recipients: vec![],
            },
            now_ms,
        )
        .await?;
    }
    Ok(())
}
impl<'a> ArchiveRepo<'a> {
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
    /// Check current policy before touching archive rows. Private access requires owned, verified membership.
    /// # Errors
    /// Returns forbidden, not-found or database errors.
    pub async fn authorize(&self, list: &ListId, auth: Option<&TokenAuth>) -> Result<()> {
        let policy = self.db.lists().get(list).await?.archive_policy;
        if policy == listmngr_core::ArchivePolicy::Public {
            return Ok(());
        }
        if policy == listmngr_core::ArchivePolicy::Never {
            return Err(Error::NotFound("archive".into()));
        }
        let auth = auth.ok_or_else(|| Error::Forbidden("private archive".into()))?;
        let domain = self.db.domains().get(list.mail_host()).await?;
        if !auth.allows_list(list, domain.id) {
            return Err(Error::Forbidden("archive boundary".into()));
        }
        if auth.scopes.contains("admin") && auth.list_id.is_none() && auth.domain_id.is_none() {
            return Ok(());
        }
        if !auth.has_scope("members:read") {
            return Err(Error::Forbidden("members:read".into()));
        }
        let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id=$1 AND m.role='member' AND a.user_id=$2 AND a.verified_on IS NOT NULL")
   .bind(list.as_str()).bind(auth.user_id.to_string()).fetch_one(self.db.pool()).await.map_err(db_error)?;
        if exists == 0 {
            return Err(Error::Forbidden(
                "verified archive membership required".into(),
            ));
        }
        Ok(())
    }
    /// Read at most 100 messages, scoped to a list and optionally thread and literal substring.
    /// # Errors
    /// Returns access, validation or database errors.
    pub async fn read(
        &self,
        list: &ListId,
        auth: Option<&TokenAuth>,
        thread: Option<&str>,
        query: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ArchiveMessage>> {
        self.authorize(list, auth).await?;
        self.read_snapshot(list, auth, thread, query, limit, offset)
            .await
    }
    async fn read_snapshot(
        &self,
        list: &ListId,
        auth: Option<&TokenAuth>,
        thread: Option<&str>,
        query: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ArchiveMessage>> {
        self.read_selection(list, auth, Selection::Thread(thread), query, limit, offset)
            .await
    }
    /// Read exactly one list-scoped message through the current publication policy.
    /// # Errors
    /// Returns validation, access, missing-message or database errors.
    pub async fn read_message(
        &self,
        list: &ListId,
        auth: Option<&TokenAuth>,
        hash: &str,
    ) -> Result<ArchiveMessage> {
        if hash.is_empty() || hash.len() > 200 {
            return Err(Error::Validation("archive message hash bounds".into()));
        }
        self.authorize(list, auth).await?;
        self.read_selection(list, auth, Selection::Message(hash), "", 1, 0)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| Error::NotFound("archive message".into()))
    }
    async fn read_selection(
        &self,
        list: &ListId,
        auth: Option<&TokenAuth>,
        selection: Selection<'_>,
        query: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ArchiveMessage>> {
        let (thread, hash) = match selection {
            Selection::Thread(thread) => (thread.unwrap_or(""), ""),
            Selection::Message(hash) => ("", hash),
        };
        if !(1..=100).contains(&limit) || !(0..=100_000).contains(&offset) || query.len() > 200 {
            return Err(Error::Validation("archive query bounds".into()));
        }
        let pattern = format!(
            "%{}%",
            query
                .replace('!', "!!")
                .replace('%', "!%")
                .replace('_', "!_")
        );
        let settings = self.db.lists().get(list).await?;
        let rows = sqlx::query("SELECT hash,thread,subject,body,raw_b64,(SELECT anonymous_list FROM mailing_lists WHERE list_id=$1) AS anonymous_list,(SELECT subject_prefix FROM mailing_lists WHERE list_id=$1) AS subject_prefix FROM archive_messages WHERE list_id=$1 AND ($2='' OR thread=$2) AND ($11='' OR hash=$11) AND (LOWER(subject) LIKE LOWER($3) ESCAPE '!' OR LOWER(body) LIKE LOWER($3) ESCAPE '!') AND EXISTS(SELECT 1 FROM mailing_lists l JOIN domains d ON d.mail_host=l.mail_host WHERE l.list_id=$1 AND (l.archive_policy='public' OR (l.archive_policy='private' AND ($6=1 OR ($7=1 AND ($8='' OR $8=l.list_id) AND ($9='' OR $9=d.id) AND EXISTS(SELECT 1 FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id=l.list_id AND m.role='member' AND a.user_id=$10 AND a.verified_on IS NOT NULL)))))) ORDER BY created_at,hash LIMIT $4 OFFSET $5")
   .bind(list.as_str()).bind(thread).bind(pattern).bind(limit).bind(offset)
   .bind(i64::from(auth.is_some_and(|a| a.scopes.contains("admin") && a.list_id.is_none() && a.domain_id.is_none())))
   .bind(i64::from(auth.is_some_and(|a| a.has_scope("members:read"))))
   .bind(auth.and_then(|a| a.list_id.as_ref()).map_or_else(String::new, ToString::to_string))
   .bind(auth.and_then(|a| a.domain_id).map_or_else(String::new, |id| id.to_string()))
   .bind(auth.map_or_else(String::new, |a| a.user_id.to_string()))
   .bind(hash)
   .fetch_all(self.db.pool()).await.map_err(db_error)?;
        render_rows(settings, &rows, self.db.base_url())
    }
    /// Atomically index and ack an archive lease; replay is idempotent per list/hash.
    /// # Errors
    /// Returns invalid lease/context, stale lease or database errors.
    pub async fn complete(&self, lease: &Lease, item: &ArchiveMessage, now_ms: i64) -> Result<()> {
        if lease.job.queue != Queue::Archive {
            return Err(Error::Validation("archive lease required".into()));
        }
        let message = self.db.mail_queue().message(lease.job.message_id).await?;
        let context: serde_json::Value =
            serde_json::from_str(&message.context).map_err(db_error)?;
        let list: ListId = context["list_id"]
            .as_str()
            .ok_or_else(|| Error::Validation("missing list".into()))?
            .parse()?;
        let mut tx = self.db.pool().begin().await.map_err(db_error)?;
        let settings = crate::lock_list_for_patch(&mut tx, &list).await?;
        let raw = publication(&message.raw, &settings, &item.hash, self.db.base_url())?;
        let parsed = mail_parser::MessageParser::default()
            .parse(&raw)
            .ok_or_else(|| Error::Validation("invalid archive MIME".into()))?;
        let queue = self.db.mail_queue();
        let queue = self.clock.map_or(queue, |clock| queue.with_clock(clock));
        let now_ms = queue.lock_time(&mut tx, lease, now_ms).await?;
        // Preserve renewed authority before ACK clears the locked queue row.
        let deadline = crate::mail_queue::MailQueueRepo::locked_deadline(&mut tx, lease).await?;
        // UPDATE locks the policy row against a concurrent privacy change before storage.
        let policy: String = sqlx::query_scalar("UPDATE mailing_lists SET archive_policy=archive_policy WHERE list_id=$1 RETURNING archive_policy").bind(list.as_str()).fetch_one(&mut *tx).await.map_err(db_error)?;
        if policy == "public" || policy == "private" {
            // Resolve an indexed parent's root in the same list and transaction.
            let thread: Option<String> = sqlx::query_scalar(
                "SELECT thread FROM archive_messages WHERE list_id=$1 AND hash=$2",
            )
            .bind(list.as_str())
            .bind(&item.thread)
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_error)?;
            let thread = thread.as_deref().unwrap_or(&item.thread);
            sqlx::query("INSERT INTO archive_messages(list_id,hash,thread,subject,body,raw_b64,created_at) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(list_id,hash) DO NOTHING")
    .bind(list.as_str()).bind(&item.hash).bind(thread).bind(parsed.subject().unwrap_or("")).bind(parsed.body_text(0).unwrap_or_default().as_ref()).bind(base64::engine::general_purpose::STANDARD.encode(&raw)).bind(message.created_at).execute(&mut *tx).await.map_err(db_error)?;
            // A parent arriving after its replies unifies their provisional root.
            sqlx::query("UPDATE archive_messages SET thread=$1 WHERE list_id=$2 AND thread=$3")
                .bind(thread)
                .bind(list.as_str())
                .bind(&item.hash)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
            Database::record_tx_with_context(
                &mut tx,
                &crate::AuditContext::system(),
                "archive.index",
                "list",
                list.as_str(),
                serde_json::json!({"hash": item.hash}),
            )
            .await?;
        }
        // Index/audit writes can wait too. Fence last, with the queue lock held.
        ack_leased_job(&mut tx, lease, queue.time(now_ms)).await?;
        // ACK's own audit insert can wait; no awaited business write follows this check.
        queue.check_final_deadline(Some(deadline), now_ms)?;
        tx.commit().await.map_err(db_error)
    }
}
