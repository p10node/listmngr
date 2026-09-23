//! What readers left on another site's archive, written onto this one:
//! votes, tags, a category per thread, and favourites.
//!
//! An import places what it can and counts the rest: a post or thread
//! this archive does not have (its messages come from the mbox, which
//! is imported separately) and a reader with no account here are
//! skipped, never invented. Nothing is mailed, and running it again
//! writes what it wrote before, not more.
use crate::archive::ArchiveRepo;
use crate::archive::interact::{normalize_category, normalize_tag};
use crate::{AuditContext, Database, db_error};
use listmngr_core::{Address, ListId, Result, UserId};
use sqlx::{Any, Row, Transaction};

/// What another site's archive holds for one list.
#[derive(Debug, Clone)]
pub struct ImportedInteractions<'a> {
    pub list: &'a ListId,
    /// `(message hash, the reader's address, 1 or -1)`.
    pub votes: Vec<(String, String, i32)>,
    /// `(thread, tag, the tagger's address)`.
    pub tags: Vec<(String, String, String)>,
    /// `(thread, category)`.
    pub categories: Vec<(String, String)>,
    /// `(thread, the reader's address)`.
    pub favorites: Vec<(String, String)>,
    /// When the import runs, in milliseconds.
    pub at: i64,
}

/// What an import of them did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InteractionReport {
    pub votes: usize,
    pub tags: usize,
    pub categories: usize,
    pub favorites: usize,
    /// Rows this archive has no post, thread or account for.
    pub skipped: usize,
}

impl InteractionReport {
    fn to_json(self) -> serde_json::Value {
        serde_json::json!({
            "votes": self.votes,
            "tags": self.tags,
            "categories": self.categories,
            "favorites": self.favorites,
            "skipped": self.skipped,
        })
    }
}

/// The account behind an address, when this site has one.
async fn account(tx: &mut Transaction<'_, Any>, email: &str) -> Result<Option<UserId>> {
    let Ok(address) = Address::new(email, String::new()) else {
        return Ok(None);
    };
    let owner: Option<Option<String>> = sqlx::query("SELECT user_id FROM addresses WHERE email=$1")
        .bind(&address.email)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_error)?
        .map(|row| row.try_get("user_id").ok().flatten());
    let Some(Some(owner)) = owner else {
        return Ok(None);
    };
    Ok(crate::parse_uuid(&owner).ok().map(UserId))
}

async fn has_message(tx: &mut Transaction<'_, Any>, list: &ListId, hash: &str) -> Result<bool> {
    let found: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM archive_messages WHERE list_id=$1 AND hash=$2 AND hidden_at IS NULL",
    )
    .bind(list.as_str())
    .bind(hash)
    .fetch_one(&mut **tx)
    .await
    .map_err(db_error)?;
    Ok(found > 0)
}

async fn has_thread(tx: &mut Transaction<'_, Any>, list: &ListId, thread: &str) -> Result<bool> {
    let found: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM archive_messages WHERE list_id=$1 AND thread=$2 AND hidden_at IS NULL",
    )
    .bind(list.as_str())
    .bind(thread)
    .fetch_one(&mut **tx)
    .await
    .map_err(db_error)?;
    Ok(found > 0)
}

type Tx<'t> = Transaction<'t, Any>;

async fn votes(
    tx: &mut Tx<'_>,
    imported: &ImportedInteractions<'_>,
    report: &mut InteractionReport,
) -> Result<()> {
    let list = imported.list.as_str();
    for (hash, email, value) in &imported.votes {
        let voter = account(tx, email).await?;
        let (Some(voter), true, true) = (
            voter,
            *value == 1 || *value == -1,
            has_message(tx, imported.list, hash).await?,
        ) else {
            report.skipped += 1;
            continue;
        };
        sqlx::query("DELETE FROM archive_votes WHERE user_id=$1 AND list_id=$2 AND hash=$3")
            .bind(voter.to_string())
            .bind(list)
            .bind(hash)
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
        sqlx::query("INSERT INTO archive_votes(user_id, list_id, hash, value, voted_at) VALUES($1,$2,$3,$4,$5)")
            .bind(voter.to_string())
            .bind(list)
            .bind(hash)
            .bind(*value)
            .bind(imported.at)
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
        report.votes += 1;
    }
    Ok(())
}

async fn tags(
    tx: &mut Tx<'_>,
    imported: &ImportedInteractions<'_>,
    report: &mut InteractionReport,
) -> Result<()> {
    let list = imported.list.as_str();
    for (thread, tag, email) in &imported.tags {
        let tagger = account(tx, email).await?;
        let (Some(tagger), Ok(tag), true) = (
            tagger,
            normalize_tag(tag),
            has_thread(tx, imported.list, thread).await?,
        ) else {
            report.skipped += 1;
            continue;
        };
        sqlx::query("DELETE FROM archive_tags WHERE list_id=$1 AND thread=$2 AND tag=$3")
            .bind(list)
            .bind(thread)
            .bind(&tag)
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
        sqlx::query("INSERT INTO archive_tags(list_id, thread, tag, user_id, tagged_at) VALUES($1,$2,$3,$4,$5)")
            .bind(list)
            .bind(thread)
            .bind(&tag)
            .bind(tagger.to_string())
            .bind(imported.at)
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
        report.tags += 1;
    }
    Ok(())
}

async fn categories(
    tx: &mut Tx<'_>,
    imported: &ImportedInteractions<'_>,
    report: &mut InteractionReport,
) -> Result<()> {
    let list = imported.list.as_str();
    for (thread, category) in &imported.categories {
        let (Ok(category), true) = (
            normalize_category(category),
            has_thread(tx, imported.list, thread).await?,
        ) else {
            report.skipped += 1;
            continue;
        };
        sqlx::query(
            "INSERT INTO archive_categories(list_id, name) VALUES($1,$2) ON CONFLICT(list_id, name) DO NOTHING",
        )
        .bind(list)
        .bind(&category)
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
        sqlx::query("DELETE FROM archive_thread_categories WHERE list_id=$1 AND thread=$2")
            .bind(list)
            .bind(thread)
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
        sqlx::query(
            "INSERT INTO archive_thread_categories(list_id, thread, category) VALUES($1,$2,$3)",
        )
        .bind(list)
        .bind(thread)
        .bind(&category)
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
        report.categories += 1;
    }
    Ok(())
}

async fn favorites(
    tx: &mut Tx<'_>,
    imported: &ImportedInteractions<'_>,
    report: &mut InteractionReport,
) -> Result<()> {
    let list = imported.list.as_str();
    for (thread, email) in &imported.favorites {
        let reader = account(tx, email).await?;
        let (Some(reader), true) = (reader, has_thread(tx, imported.list, thread).await?) else {
            report.skipped += 1;
            continue;
        };
        sqlx::query("DELETE FROM archive_favorites WHERE user_id=$1 AND list_id=$2 AND thread=$3")
            .bind(reader.to_string())
            .bind(list)
            .bind(thread)
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
        sqlx::query("INSERT INTO archive_favorites(user_id, list_id, thread, marked_at) VALUES($1,$2,$3,$4)")
            .bind(reader.to_string())
            .bind(list)
            .bind(thread)
            .bind(imported.at)
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
        report.favorites += 1;
    }
    Ok(())
}

impl ArchiveRepo<'_> {
    /// Write another site's votes, tags, categories and favourites onto
    /// this archive, in one transaction with one audit event.
    ///
    /// # Errors
    /// `NotFound` for an unknown list, and the database's own errors.
    pub async fn import_interactions(
        &self,
        imported: &ImportedInteractions<'_>,
        context: &AuditContext,
    ) -> Result<InteractionReport> {
        let db = self.database();
        db.lists().get(imported.list).await?;
        let mut report = InteractionReport::default();
        let mut tx = db.write_tx().await?;
        votes(&mut tx, imported, &mut report).await?;
        tags(&mut tx, imported, &mut report).await?;
        categories(&mut tx, imported, &mut report).await?;
        favorites(&mut tx, imported, &mut report).await?;
        Database::record_tx_with_context(
            &mut tx,
            context,
            "archive.import_interactions",
            "list",
            imported.list.as_str(),
            report.to_json(),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(report)
    }
}
