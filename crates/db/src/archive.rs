//! Policy-gated durable archive. SQL substring search is bounded, not full-text ranking.
use crate::mail_queue::{Lease, Queue, ack_leased_job};
use crate::{Database, TokenAuth, db_error};
use base64::Engine;
use listmngr_core::{Error, ListId, Result};
use serde::Serialize;
use sqlx::Row;
#[path = "archive_admin.rs"]
pub mod admin;
#[path = "archive_browse.rs"]
pub mod browse;
#[path = "browser_archive.rs"]
mod browser;
#[path = "archive_import.rs"]
pub mod import;
#[path = "archive_interact.rs"]
pub mod interact;
enum Selection<'a> {
    Thread(Option<&'a str>),
    Message(&'a str),
}
#[cfg(test)]
#[path = "archive_tests.rs"]
mod tests;
#[derive(Debug, Default, Serialize)]
pub struct ArchiveMessage {
    pub hash: String,
    pub thread: String,
    pub subject: String,
    pub body: String,
    /// The sender as the cooked copy shows it (an anonymous list shows none).
    pub sender_name: String,
    pub sender_email: String,
    /// The `Date` header in milliseconds, when it parsed.
    pub date_ms: Option<i64>,
    /// The post replied to, when the headers named one; it may be absent
    /// from the archive.
    pub parent: Option<String>,
    /// Stored attachments, metadata only.
    pub attachments: Vec<StoredAttachment>,
    #[serde(skip)]
    pub raw: Vec<u8>,
}

/// One attachment stored with an archived post.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct StoredAttachment {
    pub position: i64,
    pub filename: String,
    pub content_type: String,
    pub size: i64,
}

/// One archived post as the search index reads it; no policy applies
/// because the index holds nothing a search returns without the archive's
/// own authorization afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexRow {
    pub list: String,
    pub hash: String,
    pub thread: String,
    pub subject: String,
    pub body: String,
    pub sender_name: String,
    pub sender_email: String,
    /// The post's date, else its arrival, milliseconds.
    pub date_ms: i64,
    pub created_at: i64,
}

/// One attachment's bytes, for a download.
#[derive(Debug, Clone)]
pub struct AttachmentContent {
    pub filename: String,
    pub content_type: String,
    pub content: Vec<u8>,
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
            let raw = publication(&stored, &settings, &hash, base_url, None)?;
            let parsed = mail_parser::MessageParser::default()
                .parse(&raw)
                .ok_or_else(|| Error::Validation("invalid archive MIME".into()))?;
            Ok(ArchiveMessage {
                hash,
                thread: r.try_get("thread").map_err(db_error)?,
                subject: parsed.subject().unwrap_or("").into(),
                body: parsed.body_text(0).unwrap_or_default().into_owned(),
                sender_name: r.try_get("sender_name").map_err(db_error)?,
                sender_email: r.try_get("sender_email").map_err(db_error)?,
                date_ms: r.try_get("message_date").map_err(db_error)?,
                parent: r.try_get("parent_hash").map_err(db_error)?,
                attachments: Vec::new(),
                raw,
            })
        })
        .collect()
}

const INDEX_ROW_SQL: &str = "SELECT list_id, hash, thread, substr(subject,1,1000) AS subject, substr(body,1,200000) AS body, sender_name, sender_email, COALESCE(message_date, created_at) AS date_ms, created_at FROM archive_messages WHERE list_id=$1 AND hash=$2 AND created_at>=$3 AND hash<>$4 AND hidden_at IS NULL LIMIT $5";
const INDEX_ROWS_SQL: &str = "SELECT list_id, hash, thread, substr(subject,1,1000) AS subject, substr(body,1,200000) AS body, sender_name, sender_email, COALESCE(message_date, created_at) AS date_ms, created_at FROM archive_messages WHERE (created_at>$1 OR (created_at=$1 AND hash>$2)) AND hidden_at IS NULL ORDER BY created_at, hash LIMIT $3";

fn index_row(row: &sqlx::any::AnyRow) -> Result<IndexRow> {
    Ok(IndexRow {
        list: row.try_get("list_id").map_err(db_error)?,
        hash: row.try_get("hash").map_err(db_error)?,
        thread: row.try_get("thread").map_err(db_error)?,
        subject: row.try_get("subject").map_err(db_error)?,
        body: row.try_get("body").map_err(db_error)?,
        sender_name: row.try_get("sender_name").map_err(db_error)?,
        sender_email: row.try_get("sender_email").map_err(db_error)?,
        date_ms: row.try_get("date_ms").map_err(db_error)?,
        created_at: row.try_get("created_at").map_err(db_error)?,
    })
}

/// The `mail-archive` archiver: a public list's copy goes to the service
/// through the outbound queue, queued with the archived post so the two
/// commit together. Does nothing when the list has the archiver off.
async fn queue_mail_archive(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    list: &ListId,
    address: &str,
    message: crate::mail_queue::MessageId,
    hash: &str,
    now_ms: i64,
) -> Result<()> {
    if !archiver_on(tx, list, "mail-archive").await? {
        return Ok(());
    }
    let child = crate::mail_queue::insert_child_job(
        tx,
        message,
        &crate::mail_queue::ChildJob {
            queue: Queue::Out,
            max_attempts: 8,
            recipients: vec![address.to_owned()],
        },
        now_ms,
    )
    .await?;
    Database::record_tx_with_context(
        tx,
        &crate::AuditContext::system(),
        "archive.archiver",
        "list",
        list.as_str(),
        serde_json::json!({"archiver": "mail-archive", "hash": hash, "job": child.id.0.to_string()}),
    )
    .await
}

/// The thread a post joins, resolved the way `HyperKitty` resolves it: the
/// thread of its provisional root when that root is archived, else the
/// thread of its parent when the parent is, else — for compatibility
/// with posts filed under a root that never arrived — the provisional
/// root when something is already filed there, else the post itself. A
/// reply whose ancestors are all absent therefore starts its own thread
/// rather than one named after a post nobody holds.
pub(crate) async fn resolve_thread(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    list: &ListId,
    hash: &str,
    provisional: &str,
    parent: Option<&str>,
) -> Result<String> {
    for candidate in [Some(provisional), parent].into_iter().flatten() {
        let thread: Option<String> =
            sqlx::query_scalar("SELECT thread FROM archive_messages WHERE list_id=$1 AND hash=$2")
                .bind(list.as_str())
                .bind(candidate)
                .fetch_optional(&mut **tx)
                .await
                .map_err(db_error)?;
        if let Some(thread) = thread {
            return Ok(thread);
        }
    }
    let filed: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM archive_messages WHERE list_id=$1 AND thread=$2 LIMIT 1")
            .bind(list.as_str())
            .bind(provisional)
            .fetch_optional(&mut **tx)
            .await
            .map_err(db_error)?;
    Ok(if filed.is_some() {
        provisional.to_owned()
    } else {
        hash.to_owned()
    })
}

/// A parent arriving after its replies takes them in: replies that
/// started their own thread for want of it, with their whole subtrees,
/// and posts filed under this hash as a provisional root.
pub(crate) async fn adopt_orphans(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    list: &ListId,
    hash: &str,
    thread: &str,
) -> Result<()> {
    // Two statements, each on its own index (`archive_thread`,
    // `archive_parent`): one `OR` between them costs a table scan per
    // archived post, which a hundred-thousand-post import cannot afford.
    sqlx::query("UPDATE archive_messages SET thread=$1 WHERE list_id=$2 AND thread=$3")
        .bind(thread)
        .bind(list.as_str())
        .bind(hash)
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    sqlx::query("UPDATE archive_messages SET thread=$1 WHERE list_id=$2 AND thread IN (SELECT o.hash FROM archive_messages o WHERE o.list_id=$2 AND o.parent_hash=$3 AND o.thread=o.hash AND o.hash<>$1)")
        .bind(thread)
        .bind(list.as_str())
        .bind(hash)
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    Ok(())
}

/// Whether one of a list's archivers is switched on.
async fn archiver_on(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    list: &ListId,
    name: &str,
) -> Result<bool> {
    let enabled: Option<i64> =
        sqlx::query_scalar("SELECT enabled FROM list_archivers WHERE list_id=$1 AND name=$2")
            .bind(list.as_str())
            .bind(name)
            .fetch_optional(&mut **tx)
            .await
            .map_err(db_error)?;
    Ok(enabled.is_some_and(|value| value != 0))
}

/// The first `From` mailbox of a parsed message: name and address, each
/// bounded. The importer parses outside this crate and needs the same
/// projection the archive runner stores.
#[must_use]
pub fn sender_of(parsed: &mail_parser::Message<'_>) -> (String, String) {
    sender(parsed)
}

/// The first `From` mailbox of the cooked copy: name and address.
fn sender(parsed: &mail_parser::Message<'_>) -> (String, String) {
    parsed
        .from()
        .and_then(|from| from.first())
        .map(|addr| {
            (
                addr.name().unwrap_or("").chars().take(256).collect(),
                addr.address().unwrap_or("").chars().take(320).collect(),
            )
        })
        .unwrap_or_default()
}

/// The archive copy: the posting pipeline up to `to-archive`. `context` is
/// the stored message context when the caller has it (queue processing);
/// read-time rendering has only the bytes.
fn publication(
    raw: &[u8],
    list: &listmngr_core::MailingList,
    identity: &str,
    base_url: Option<&str>,
    context: Option<&serde_json::Value>,
) -> Result<Vec<u8>> {
    let authentication_results = context
        .and_then(|context| context["authentication_results"].as_str())
        .map(str::to_owned);
    listmngr_mail::handlers::cook_with(
        listmngr_mail::handlers::Target::Archive,
        raw,
        list,
        identity,
        &listmngr_mail::handlers::Admission {
            base_url,
            dmarc_mitigate: context.is_some_and(|context| context["dmarc_mitigate"] == true),
            authentication_results: authentication_results.as_deref(),
        },
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
        // A caller's page is 100 posts at most; only the thread tree reads more.
        if !(1..=100).contains(&limit) {
            return Err(Error::Validation("archive query bounds".into()));
        }
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
        if !(1..=500).contains(&limit) || !(0..=100_000).contains(&offset) || query.len() > 200 {
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
        let rows = sqlx::query("SELECT hash,thread,subject,body,raw_b64,sender_name,sender_email,message_date,CASE WHEN EXISTS(SELECT 1 FROM archive_messages p WHERE p.list_id=m.list_id AND p.hash=m.parent_hash AND p.hidden_at IS NULL) THEN m.parent_hash ELSE NULL END AS parent_hash,(SELECT anonymous_list FROM mailing_lists WHERE list_id=$1) AS anonymous_list,(SELECT subject_prefix FROM mailing_lists WHERE list_id=$1) AS subject_prefix FROM archive_messages m WHERE m.list_id=$1 AND m.hidden_at IS NULL AND ($2='' OR thread=$2) AND ($11='' OR hash=$11) AND (LOWER(subject) LIKE LOWER($3) ESCAPE '!' OR LOWER(body) LIKE LOWER($3) ESCAPE '!') AND EXISTS(SELECT 1 FROM mailing_lists l JOIN domains d ON d.mail_host=l.mail_host WHERE l.list_id=$1 AND (l.archive_policy='public' OR (l.archive_policy='private' AND ($6=1 OR ($7=1 AND ($8='' OR $8=l.list_id) AND ($9='' OR $9=d.id) AND EXISTS(SELECT 1 FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id=l.list_id AND m.role='member' AND a.user_id=$10 AND a.verified_on IS NOT NULL)))))) ORDER BY created_at,hash LIMIT $4 OFFSET $5")
   .bind(list.as_str()).bind(thread).bind(pattern).bind(limit).bind(offset)
   .bind(i64::from(auth.is_some_and(|a| a.scopes.contains("admin") && a.list_id.is_none() && a.domain_id.is_none())))
   .bind(i64::from(auth.is_some_and(|a| a.has_scope("members:read"))))
   .bind(auth.and_then(|a| a.list_id.as_ref()).map_or_else(String::new, ToString::to_string))
   .bind(auth.and_then(|a| a.domain_id).map_or_else(String::new, |id| id.to_string()))
   .bind(auth.map_or_else(String::new, |a| a.user_id.to_string()))
   .bind(hash)
   .fetch_all(self.db.pool()).await.map_err(db_error)?;
        let mut messages = render_rows(settings, &rows, self.db.base_url())?;
        self.attach_metadata(list, &mut messages).await?;
        Ok(messages)
    }
    /// The archived post a finished archive job stored, for the search
    /// index; `None` when the job archived nothing.
    /// # Errors
    /// Returns database errors and an unreadable message id.
    pub async fn index_row_for_message(
        &self,
        message_id: crate::mail_queue::MessageId,
    ) -> Result<Option<IndexRow>> {
        let message = self.db.mail_queue().message(message_id).await?;
        let context: serde_json::Value =
            serde_json::from_str(&message.context).map_err(db_error)?;
        let Some(list) = context["list_id"].as_str() else {
            return Ok(None);
        };
        let Ok(hash) = listmngr_mail::message_id_hash(&message.external_id) else {
            return Ok(None);
        };
        let rows = sqlx::query(INDEX_ROW_SQL)
            .bind(list)
            .bind(hash)
            .bind(i64::MIN)
            .bind("")
            .bind(1_i64)
            .fetch_all(self.db.pool())
            .await
            .map_err(db_error)?;
        rows.first().map(index_row).transpose()
    }
    /// Archived posts after `(created_at, hash)`, oldest first, for a
    /// rebuild of the search index.
    /// # Errors
    /// Returns database errors.
    pub async fn index_rows_after(
        &self,
        after: Option<&(i64, String)>,
        limit: i64,
    ) -> Result<Vec<IndexRow>> {
        let (created, hash) = after.map_or((i64::MIN, ""), |(c, h)| (*c, h.as_str()));
        let rows = sqlx::query(INDEX_ROWS_SQL)
            .bind(created)
            .bind(hash)
            .bind(limit.clamp(1, 5000))
            .fetch_all(self.db.pool())
            .await
            .map_err(db_error)?;
        rows.iter().map(index_row).collect()
    }
    /// The stored attachments' metadata for each message on a page.
    pub(crate) async fn attach_metadata(
        &self,
        list: &ListId,
        messages: &mut [ArchiveMessage],
    ) -> Result<()> {
        for message in messages.iter_mut() {
            let rows = sqlx::query("SELECT position, filename, content_type, size FROM archive_attachments WHERE list_id=$1 AND hash=$2 ORDER BY position")
                .bind(list.as_str())
                .bind(&message.hash)
                .fetch_all(self.db.pool())
                .await
                .map_err(db_error)?;
            message.attachments = rows
                .iter()
                .map(|row| {
                    Ok(StoredAttachment {
                        position: row.try_get("position").map_err(db_error)?,
                        filename: row.try_get("filename").map_err(db_error)?,
                        content_type: row.try_get("content_type").map_err(db_error)?,
                        size: row.try_get("size").map_err(db_error)?,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
        }
        Ok(())
    }
    /// One whole thread (500 posts at most), for the tree view.
    /// # Errors
    /// Returns access, validation or database errors.
    pub async fn read_thread(
        &self,
        list: &ListId,
        auth: Option<&TokenAuth>,
        thread: &str,
    ) -> Result<Vec<ArchiveMessage>> {
        if thread.is_empty() || thread.len() > 200 {
            return Err(Error::Validation("archive thread bounds".into()));
        }
        self.authorize(list, auth).await?;
        self.read_snapshot(list, auth, Some(thread), "", 500, 0)
            .await
    }
    /// One stored attachment, after the message itself was authorized.
    /// # Errors
    /// Returns access, missing or database errors.
    pub async fn read_attachment(
        &self,
        list: &ListId,
        auth: Option<&TokenAuth>,
        hash: &str,
        position: i64,
    ) -> Result<AttachmentContent> {
        self.read_message(list, auth, hash).await?;
        self.attachment_row(list, hash, position).await
    }
    pub(crate) async fn attachment_row(
        &self,
        list: &ListId,
        hash: &str,
        position: i64,
    ) -> Result<AttachmentContent> {
        let row = sqlx::query("SELECT filename, content_type, content FROM archive_attachments WHERE list_id=$1 AND hash=$2 AND position=$3")
            .bind(list.as_str())
            .bind(hash)
            .bind(position)
            .fetch_optional(self.db.pool())
            .await
            .map_err(db_error)?
            .ok_or_else(|| Error::NotFound("archive attachment".into()))?;
        Ok(AttachmentContent {
            filename: row.try_get("filename").map_err(db_error)?,
            content_type: row.try_get("content_type").map_err(db_error)?,
            content: row.try_get("content").map_err(db_error)?,
        })
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
        let raw = publication(
            &message.raw,
            &settings,
            &item.hash,
            self.db.base_url(),
            Some(&context),
        )?;
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
            let thread = resolve_thread(
                &mut tx,
                &list,
                &item.hash,
                &item.thread,
                item.parent.as_deref(),
            )
            .await?;
            let thread = thread.as_str();
            let (sender_name, sender_email) = sender(&parsed);
            let date_ms = parsed
                .date()
                .map(|date| date.to_timestamp().saturating_mul(1000));
            let inserted = sqlx::query("INSERT INTO archive_messages(list_id,hash,thread,subject,body,raw_b64,created_at,sender_name,sender_email,message_date,parent_hash) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) ON CONFLICT(list_id,hash) DO NOTHING")
    .bind(list.as_str()).bind(&item.hash).bind(thread).bind(parsed.subject().unwrap_or("")).bind(parsed.body_text(0).unwrap_or_default().as_ref()).bind(base64::engine::general_purpose::STANDARD.encode(&raw)).bind(message.created_at)
    .bind(sender_name).bind(sender_email).bind(date_ms).bind(item.parent.as_deref())
    .execute(&mut *tx).await.map_err(db_error)?.rows_affected();
            if inserted > 0 {
                // A malformed or oversized MIME structure stores no attachment
                // and never blocks indexing; the page then says so.
                for (position, stored) in listmngr_mail::attachments::stored(&raw)
                    .unwrap_or_default()
                    .into_iter()
                    .enumerate()
                {
                    sqlx::query("INSERT INTO archive_attachments(list_id,hash,position,filename,content_type,size,content) VALUES($1,$2,$3,$4,$5,$6,$7)")
                        .bind(list.as_str()).bind(&item.hash).bind(i64::try_from(position).unwrap_or(i64::MAX))
                        .bind(&stored.filename).bind(&stored.content_type).bind(i64::try_from(stored.content.len()).unwrap_or(i64::MAX)).bind(&stored.content)
                        .execute(&mut *tx).await.map_err(db_error)?;
                }
            }
            adopt_orphans(&mut tx, &list, &item.hash, thread).await?;
            Database::record_tx_with_context(
                &mut tx,
                &crate::AuditContext::system(),
                "archive.index",
                "list",
                list.as_str(),
                serde_json::json!({"hash": item.hash}),
            )
            .await?;
            // The mail-archive archiver is a durable write, so its child
            // job and its audit event commit with the archived post.
            if policy == "public"
                && let Some(address) = self.db.mail_archive_address()
            {
                queue_mail_archive(
                    &mut tx,
                    &list,
                    address,
                    lease.job.message_id,
                    &item.hash,
                    now_ms,
                )
                .await?;
            }
        }
        // Index/audit writes can wait too. Fence last, with the queue lock held.
        ack_leased_job(&mut tx, lease, queue.time(now_ms)).await?;
        // ACK's own audit insert can wait; no awaited business write follows this check.
        queue.check_final_deadline(Some(deadline), now_ms)?;
        tx.commit().await.map_err(db_error)
    }
}
