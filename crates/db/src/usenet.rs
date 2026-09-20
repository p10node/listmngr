//! The Usenet gateway's own state on a list: the watermark, which the
//! settings API reads but only the gateway writes.
use crate::{AuditContext, Database, db_error};
use listmngr_core::{Error, ListId, Result};

/// The gateway's writes.
#[derive(Debug)]
pub struct UsenetRepo<'a> {
    db: &'a Database,
}

impl Database {
    #[must_use]
    pub const fn usenet(&self) -> UsenetRepo<'_> {
        UsenetRepo { db: self }
    }
}

impl UsenetRepo<'_> {
    /// Record the last article number gated from the list's newsgroup, with
    /// its audit event (`usenet.watermark`) in the same transaction.
    /// # Errors
    /// Returns `NotFound` for an unknown list, a validation error for a
    /// negative article number, and database errors.
    pub async fn set_watermark(&self, list: &ListId, watermark: i64) -> Result<()> {
        if watermark < 0 {
            return Err(Error::Validation("usenet_watermark".into()));
        }
        let mut tx = self.db.write_tx().await?;
        let updated = sqlx::query("UPDATE mailing_lists SET usenet_watermark=$1 WHERE list_id=$2")
            .bind(watermark)
            .bind(list.as_str())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        if updated.rows_affected() == 0 {
            return Err(Error::NotFound(list.to_string()));
        }
        Database::record_tx_with_context(
            &mut tx,
            &AuditContext::system(),
            "usenet.watermark",
            "list",
            list.as_str(),
            serde_json::json!({ "usenet_watermark": watermark }),
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }
}
