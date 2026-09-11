//! List-owned direct SMTP event reads. No SMTP diagnostics or message bodies.
use crate::{Database, db_error};
use listmngr_core::{Error, ListId, Result};
use sqlx::Row;

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct BounceEvent {
    pub id: String,
    pub list_id: ListId,
    pub recipient: String,
    pub job_id: String,
    pub message_id: String,
    /// Unix milliseconds sampled in the fenced completion transaction.
    pub created_at: i64,
    pub source: String,
    pub context: String,
    pub processed: bool,
    /// Exact remote command stage, unknown for historical or local failures.
    pub smtp_stage: Option<listmngr_core::SmtpFailureStage>,
    /// Exact remote 5xx reply, not an enhanced status or mailbox validity verdict.
    pub smtp_code: Option<i64>,
}

#[derive(Debug)]
pub struct BounceRepo<'a> {
    db: &'a Database,
}
impl Database {
    #[must_use]
    pub const fn bounces(&self) -> BounceRepo<'_> {
        BounceRepo { db: self }
    }
}
impl BounceRepo<'_> {
    /// Count metadata for an existing list; missing parents are not empty pages.
    /// # Errors
    /// Returns missing-list or database errors.
    pub async fn count(&self, list: &ListId) -> Result<i64> {
        self.db.lists().get(list).await?;
        sqlx::query_scalar("SELECT COUNT(*) FROM bounce_events WHERE list_id=$1")
            .bind(list.as_str())
            .fetch_one(self.db.pool())
            .await
            .map_err(db_error)
    }

    /// Read at most 100 event records, ordered by timestamp and durable ID.
    /// # Errors
    /// Returns invalid-window, missing-list or database errors.
    pub async fn list(&self, list: &ListId, limit: i64, offset: i64) -> Result<Vec<BounceEvent>> {
        if !(1..=100).contains(&limit) || offset < 0 {
            return Err(Error::Validation("invalid bounce page window".into()));
        }
        self.db.lists().get(list).await?;
        let rows = sqlx::query("SELECT id,recipient,job_id,message_id,created_at,source,context,processed,smtp_stage,smtp_code FROM bounce_events WHERE list_id=$1 ORDER BY created_at,id LIMIT $2 OFFSET $3")
            .bind(list.as_str()).bind(limit).bind(offset).fetch_all(self.db.pool()).await.map_err(db_error)?;
        rows.iter()
            .map(|row| {
                Ok(BounceEvent {
                    smtp_stage: row
                        .try_get::<Option<String>, _>("smtp_stage")
                        .map_err(db_error)?
                        .map(|stage| match stage.as_str() {
                            "ehlo" => Ok(listmngr_core::SmtpFailureStage::Ehlo),
                            "mail_from" => Ok(listmngr_core::SmtpFailureStage::MailFrom),
                            "rcpt" => Ok(listmngr_core::SmtpFailureStage::Rcpt),
                            "data_start" => Ok(listmngr_core::SmtpFailureStage::DataStart),
                            "data_final" => Ok(listmngr_core::SmtpFailureStage::DataFinal),
                            _ => Err(Error::Database("invalid stored SMTP stage".into())),
                        })
                        .transpose()?,
                    smtp_code: row.try_get("smtp_code").map_err(db_error)?,
                    id: row.try_get("id").map_err(db_error)?,
                    list_id: list.clone(),
                    recipient: row.try_get("recipient").map_err(db_error)?,
                    job_id: row.try_get("job_id").map_err(db_error)?,
                    message_id: row.try_get("message_id").map_err(db_error)?,
                    created_at: row.try_get("created_at").map_err(db_error)?,
                    source: row.try_get("source").map_err(db_error)?,
                    context: row.try_get("context").map_err(db_error)?,
                    processed: row.try_get::<i64, _>("processed").map_err(db_error)? != 0,
                })
            })
            .collect()
    }
}
