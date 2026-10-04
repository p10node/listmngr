//! The `posting-rate` rule's ledger: which sender had how many posts
//! accepted on a list, and when.
//!
//! The `in` runner writes a row in the
//! transaction that accepts a post and reads the count before deciding
//! the next one; the task sweep forgets rows older than the longest
//! window the limit can name.
use crate::{Database, db_error};
use listmngr_core::{ListId, Result};
use sqlx::{Any, Transaction};

/// Reads of the posting-rate ledger.
#[derive(Debug)]
pub struct PostingRateRepo<'a> {
    db: &'a Database,
}

impl Database {
    #[must_use]
    pub const fn posting_rate(&self) -> PostingRateRepo<'_> {
        PostingRateRepo { db: self }
    }
}

impl PostingRateRepo<'_> {
    /// Posts by `email` (matched as stored, lower-cased) accepted on the
    /// list at or after `since_ms`.
    /// # Errors
    /// Returns a database error.
    pub async fn count_since(&self, list: &ListId, email: &str, since_ms: i64) -> Result<u32> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM posting_rate WHERE list_id=$1 AND email=$2 AND posted_at>=$3",
        )
        .bind(list.as_str())
        .bind(email.to_ascii_lowercase())
        .bind(since_ms)
        .fetch_one(self.db.pool())
        .await
        .map_err(db_error)?;
        Ok(u32::try_from(count).unwrap_or(u32::MAX))
    }
}

/// Record one accepted post inside the transaction that accepts it.
pub(crate) async fn record_tx(
    tx: &mut Transaction<'_, Any>,
    list: &ListId,
    email: &str,
    now_ms: i64,
) -> Result<()> {
    sqlx::query("INSERT INTO posting_rate(list_id,email,posted_at) VALUES($1,$2,$3)")
        .bind(list.as_str())
        .bind(email.to_ascii_lowercase())
        .bind(now_ms)
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    Ok(())
}
