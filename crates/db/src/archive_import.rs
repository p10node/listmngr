//! Bulk import into an archive and paged export out of it.
//!
//! The importer stores messages the archive crate has already parsed, a
//! batch per transaction, with the same thread resolution as queue
//! processing (`ArchiveRepo::complete`) and one audit event per batch.
//! The exporter pages raw messages by `(created_at, hash)` so a whole
//! archive streams out without being held in memory.
use super::ArchiveRepo;
use crate::{Database, db_error, web_sessions::WebSession};
use base64::Engine as _;
use listmngr_core::{Error, ListId, Result};
use listmngr_mail::attachments::Stored;
use sqlx::Row as _;

/// One message ready to store.
#[derive(Debug, Clone)]
pub struct ImportItem {
    pub hash: String,
    /// The provisional thread root, resolved against stored roots.
    pub thread: String,
    pub parent: Option<String>,
    pub subject: String,
    pub body: String,
    pub sender_name: String,
    pub sender_email: String,
    pub date_ms: Option<i64>,
    /// The arrival stamp the archive orders by: the post's date, else now.
    pub created_at: i64,
    pub attachments: Vec<Stored>,
    pub raw: Vec<u8>,
}

/// What an import did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Outcome {
    pub imported: u64,
    /// Already archived, or not a message.
    pub skipped: u64,
}

impl std::ops::AddAssign for Outcome {
    fn add_assign(&mut self, other: Self) {
        self.imported += other.imported;
        self.skipped += other.skipped;
    }
}

/// Which messages an export takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportSelection {
    All,
    Thread(String),
    /// Posts dated within `[from_ms, until_ms)`.
    Between {
        from_ms: i64,
        until_ms: i64,
    },
}

/// One exported message.
#[derive(Debug, Clone)]
pub struct ExportRow {
    pub created_at: i64,
    pub hash: String,
    pub raw: Vec<u8>,
}

/// A stored archive policy an import may write into.
fn importable(policy: Option<&str>) -> Result<()> {
    match policy {
        Some("public" | "private") => Ok(()),
        Some(_) => Err(Error::Validation("the list keeps no archive".into())),
        None => Err(Error::NotFound("list".into())),
    }
}

impl ArchiveRepo<'_> {
    /// Store a batch in one transaction; returns how many were new.
    /// # Errors
    /// `Validation` for a list whose archive is `never`, `NotFound` for
    /// an unknown list, database failures.
    pub async fn import_batch(
        &self,
        list: &ListId,
        items: &[ImportItem],
        now_ms: i64,
    ) -> Result<Outcome> {
        let mut tx = self.db.pool().begin().await.map_err(db_error)?;
        let policy: Option<String> =
            sqlx::query_scalar("SELECT archive_policy FROM mailing_lists WHERE list_id=$1")
                .bind(list.as_str())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_error)?;
        importable(policy.as_deref())?;
        let mut outcome = Outcome::default();
        for item in items {
            let thread = super::resolve_thread(
                &mut tx,
                list,
                &item.hash,
                &item.thread,
                item.parent.as_deref(),
            )
            .await?;
            let thread = thread.as_str();
            let inserted = sqlx::query("INSERT INTO archive_messages(list_id,hash,thread,subject,body,raw_b64,created_at,sender_name,sender_email,message_date,parent_hash) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) ON CONFLICT(list_id,hash) DO NOTHING")
                .bind(list.as_str()).bind(&item.hash).bind(thread).bind(&item.subject).bind(&item.body)
                .bind(base64::engine::general_purpose::STANDARD.encode(&item.raw)).bind(item.created_at)
                .bind(&item.sender_name).bind(&item.sender_email).bind(item.date_ms).bind(item.parent.as_deref())
                .execute(&mut *tx).await.map_err(db_error)?.rows_affected();
            if inserted == 0 {
                outcome.skipped += 1;
                continue;
            }
            outcome.imported += 1;
            for (position, stored) in item.attachments.iter().enumerate() {
                sqlx::query("INSERT INTO archive_attachments(list_id,hash,position,filename,content_type,size,content) VALUES($1,$2,$3,$4,$5,$6,$7)")
                    .bind(list.as_str()).bind(&item.hash).bind(i64::try_from(position).unwrap_or(i64::MAX))
                    .bind(&stored.filename).bind(&stored.content_type).bind(i64::try_from(stored.content.len()).unwrap_or(i64::MAX)).bind(&stored.content)
                    .execute(&mut *tx).await.map_err(db_error)?;
            }
            super::adopt_orphans(&mut tx, list, &item.hash, thread).await?;
        }
        Database::record_tx_with_context(
            &mut tx,
            &crate::AuditContext::system(),
            "archive.import",
            "list",
            list.as_str(),
            serde_json::json!({"imported": outcome.imported, "skipped": outcome.skipped, "at": now_ms}),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(outcome)
    }

    /// The list must keep an archive before an import reads a byte of
    /// the file: an mbox holding nothing storable would otherwise report
    /// success on a list whose archive is `never`.
    /// # Errors
    /// `Validation` for `never`, `NotFound` for an unknown list, database
    /// failures.
    pub async fn ensure_importable(&self, list: &ListId) -> Result<()> {
        let policy: Option<String> =
            sqlx::query_scalar("SELECT archive_policy FROM mailing_lists WHERE list_id=$1")
                .bind(list.as_str())
                .fetch_optional(self.db.pool())
                .await
                .map_err(db_error)?;
        importable(policy.as_deref())
    }

    /// A page of messages after `(created_at, hash)`, oldest first,
    /// without the archive policy (the command line, and the browser
    /// export which authorizes first). Each message is projected for
    /// publication exactly as an archive page projects it, so an
    /// anonymous list's authors are hidden here too.
    /// # Errors
    /// `NotFound` for an unknown list, database failures, a stored
    /// message that will not parse.
    pub async fn export_rows(
        &self,
        list: &ListId,
        selection: &ExportSelection,
        after: Option<&(i64, String)>,
        limit: i64,
    ) -> Result<Vec<ExportRow>> {
        let settings = self.db.lists().get(list).await?;
        let (thread, from, until) = match selection {
            ExportSelection::All => ("", i64::MIN, i64::MAX),
            ExportSelection::Thread(thread) => (thread.as_str(), i64::MIN, i64::MAX),
            ExportSelection::Between { from_ms, until_ms } => ("", *from_ms, *until_ms),
        };
        let (created, hash) = after.map_or((i64::MIN, ""), |(c, h)| (*c, h.as_str()));
        let rows = sqlx::query("SELECT hash, raw_b64, created_at FROM archive_messages WHERE list_id=$1 AND hidden_at IS NULL AND ($2='' OR thread=$2) AND COALESCE(message_date, created_at)>=$3 AND COALESCE(message_date, created_at)<$4 AND (created_at>$5 OR (created_at=$5 AND hash>$6)) ORDER BY created_at, hash LIMIT $7")
            .bind(list.as_str())
            .bind(thread)
            .bind(from)
            .bind(until)
            .bind(created)
            .bind(hash)
            .bind(limit.clamp(1, 1000))
            .fetch_all(self.db.pool())
            .await
            .map_err(db_error)?;
        rows.iter()
            .map(|row| {
                let hash: String = row.try_get("hash").map_err(db_error)?;
                let stored = base64::engine::general_purpose::STANDARD
                    .decode(row.try_get::<String, _>("raw_b64").map_err(db_error)?)
                    .map_err(|_| Error::Validation("stored message".into()))?;
                let raw = super::publication(&stored, &settings, &hash, self.db.base_url(), None)?;
                Ok(ExportRow {
                    created_at: row.try_get("created_at").map_err(db_error)?,
                    hash,
                    raw,
                })
            })
            .collect()
    }

    /// The archived copy of one post, without policy: the bytes the
    /// remote archivers forward, exactly as the archive pages publish
    /// them. `None` when the post is not archived or is hidden.
    /// # Errors
    /// Database failures, or a stored message that will not decode.
    pub async fn archived_copy(&self, list: &ListId, hash: &str) -> Result<Option<Vec<u8>>> {
        let stored: Option<String> = sqlx::query_scalar(
            "SELECT raw_b64 FROM archive_messages WHERE list_id=$1 AND hash=$2 AND hidden_at IS NULL",
        )
        .bind(list.as_str())
        .bind(hash)
        .fetch_optional(self.db.pool())
        .await
        .map_err(db_error)?;
        stored
            .map(|raw| {
                base64::engine::general_purpose::STANDARD
                    .decode(raw)
                    .map_err(|_| Error::Validation("stored message".into()))
            })
            .transpose()
    }

    /// The browser's export: the archive's policy for the reader, once,
    /// before any page is read.
    /// # Errors
    /// `NotFound`, `Forbidden`, a stale session or database failures.
    pub async fn browser_export_authorize(
        &self,
        list: &ListId,
        session: Option<&WebSession>,
    ) -> Result<()> {
        self.browser_authorize(list, session).await
    }
}
