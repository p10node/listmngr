//! The server owner's system page and audit log.
//!
//! Read-only projections of the runner queues and the audit log, each under
//! live browser session authority.
use crate::{AuditEntry, Database, db_error, mail_queue::QueueStats, web_sessions::WebSession};
use listmngr_core::Result;
use sqlx::Row as _;

/// The queues and the runners currently holding leases.
#[derive(Debug)]
pub struct RunnerStatus {
    pub stats: QueueStats,
    /// `locked_by` of leased jobs with how many each holds.
    pub runners: Vec<(String, i64)>,
}

/// What the audit viewer is asked for.
#[derive(Debug, Clone, Copy, Default)]
pub struct AuditFilter<'a> {
    /// A prefix of the action, such as `list.` or `member.create`.
    pub action: &'a str,
    /// A substring of the target id.
    pub target: &'a str,
    pub offset: i64,
}

fn escape_like(query: &str) -> String {
    query
        .replace('!', "!!")
        .replace('%', "!%")
        .replace('_', "!_")
}

impl Database {
    /// Queue depths per state, shunted jobs, the oldest ready job's age and
    /// the runners holding leases, for a server owner.
    /// # Errors
    /// Authority and database failures.
    pub async fn browser_runner_status(&self, session: &WebSession) -> Result<RunnerStatus> {
        let mut tx = self.browser_write_tx().await?;
        Self::server_owner_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)?;
        let stats = self
            .mail_queue()
            .stats(chrono::Utc::now().timestamp_millis())
            .await?;
        let rows = sqlx::query("SELECT locked_by, COUNT(*) AS jobs FROM queue_jobs WHERE state='leased' AND locked_by IS NOT NULL GROUP BY locked_by ORDER BY locked_by LIMIT 200")
            .fetch_all(self.pool())
            .await
            .map_err(db_error)?;
        let runners = rows
            .iter()
            .map(|row| {
                Ok((
                    row.try_get("locked_by").map_err(db_error)?,
                    row.try_get("jobs").map_err(db_error)?,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(RunnerStatus { stats, runners })
    }

    /// A page of audit events (21 rows at most), newest first, for a server
    /// owner.
    /// # Errors
    /// Authority, an invalid offset and database failures.
    pub async fn browser_audit(
        &self,
        session: &WebSession,
        filter: &AuditFilter<'_>,
    ) -> Result<Vec<AuditEntry>> {
        crate::web_admin::valid_offset(filter.offset)?;
        let mut tx = self.browser_write_tx().await?;
        Self::server_owner_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)?;
        let rows = sqlx::query("SELECT id,at,actor_user_id,actor_token_id,ip,action,target_type,target_id,diff FROM audit_log WHERE action LIKE $1 ESCAPE '!' AND target_id LIKE $2 ESCAPE '!' ORDER BY at DESC, id DESC LIMIT 21 OFFSET $3")
            .bind(format!("{}%", escape_like(filter.action.trim())))
            .bind(format!("%{}%", escape_like(filter.target.trim())))
            .bind(filter.offset)
            .fetch_all(self.pool())
            .await
            .map_err(db_error)?;
        rows.iter()
            .map(|row| {
                Ok(AuditEntry {
                    id: row.try_get("id").map_err(db_error)?,
                    at: crate::parse_time(&row.try_get::<String, _>("at").map_err(db_error)?)?,
                    actor_user_id: row
                        .try_get::<Option<String>, _>("actor_user_id")
                        .map_err(db_error)?
                        .map(|value| crate::parse_uuid(&value))
                        .transpose()?,
                    actor_token_id: row
                        .try_get::<Option<String>, _>("actor_token_id")
                        .map_err(db_error)?
                        .map(|value| crate::parse_uuid(&value))
                        .transpose()?,
                    ip: row
                        .try_get::<Option<String>, _>("ip")
                        .map_err(db_error)?
                        .map(|value| value.parse().map_err(db_error))
                        .transpose()?,
                    action: row.try_get("action").map_err(db_error)?,
                    target_type: row.try_get("target_type").map_err(db_error)?,
                    target_id: row.try_get("target_id").map_err(db_error)?,
                    diff: serde_json::from_str(
                        &row.try_get::<String, _>("diff").map_err(db_error)?,
                    )
                    .map_err(db_error)?,
                })
            })
            .collect()
    }
}
