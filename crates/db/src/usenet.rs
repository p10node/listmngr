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
    /// Move the list's watermark from `from` to `to` and, when the article
    /// at `to` is gated, queue it — in one transaction, so the article can
    /// neither be queued without the watermark passing it nor passed
    /// without being queued. `Ok(false)` means the watermark was no longer
    /// `from`: another poller moved it, and this one must stop.
    /// # Errors
    /// Validation for a negative watermark, `NotFound` for an unknown
    /// list, and database errors.
    pub async fn advance_watermark(
        &self,
        list: &ListId,
        from: Option<i64>,
        to: i64,
        gated: Option<crate::mail_queue::NewMessage>,
        now_ms: i64,
    ) -> Result<bool> {
        if to < 0 {
            return Err(Error::Validation("usenet_watermark".into()));
        }
        let mut tx = self.db.write_tx().await?;
        let updated = match from {
            Some(from) => {
                sqlx::query("UPDATE mailing_lists SET usenet_watermark=$1 WHERE list_id=$2 AND usenet_watermark=$3")
                    .bind(to)
                    .bind(list.as_str())
                    .bind(from)
                    .execute(&mut *tx)
                    .await
            }
            None => {
                sqlx::query("UPDATE mailing_lists SET usenet_watermark=$1 WHERE list_id=$2 AND usenet_watermark IS NULL")
                    .bind(to)
                    .bind(list.as_str())
                    .execute(&mut *tx)
                    .await
            }
        }
        .map_err(db_error)?;
        if updated.rows_affected() == 0 {
            let known: Option<i64> =
                sqlx::query_scalar("SELECT 1 FROM mailing_lists WHERE list_id=$1")
                    .bind(list.as_str())
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(db_error)?;
            return if known.is_some() {
                Ok(false)
            } else {
                Err(Error::NotFound(list.to_string()))
            };
        }
        let queued = gated.is_some();
        if let Some(message) = gated {
            crate::mail_queue::enqueue_tx(self.db.blobs(), &mut tx, &message, now_ms).await?;
        }
        Database::record_tx_with_context(
            &mut tx,
            &AuditContext::system(),
            "usenet.watermark",
            "list",
            list.as_str(),
            serde_json::json!({ "usenet_watermark": to, "from": from, "gated": queued }),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(true)
    }

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
