//! Explicit operator resolution of quarantined SMTP outcomes. Never auto-replays.
use crate::{AuditContext, Database, db_error, mail_queue::JobId};
use listmngr_core::{Error, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ResolutionOutcome {
    Sent,
    Failed,
    Retry,
}

#[derive(Debug)]
pub struct DeliveryResolution<'a> {
    pub job: JobId,
    pub email: &'a str,
    pub outcome: ResolutionOutcome,
    pub reason: &'a str,
    pub acknowledge_duplicate_risk: bool,
}

impl Database {
    /// Acknowledge an untrusted inbox report, retaining its job and exact message.
    /// This is operator bookkeeping, not a trusted delivery-failure observation.
    /// # Errors
    /// Rejects anything other than a ready bounce job. Audit failure rolls back.
    pub async fn acknowledge_bounce(
        &self,
        job: JobId,
        reason: &str,
        context: &AuditContext,
    ) -> Result<()> {
        let reason = reason.trim();
        if reason.is_empty() || reason.len() > 2048 || reason.chars().any(char::is_control) {
            return Err(Error::Validation(
                "acknowledgement requires a bounded, single-line reason".into(),
            ));
        }
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        // First statement acquires the writer/row lock and rechecks authority.
        // No lease is stolen, even if a worker claims while this statement waits.
        let changed = sqlx::query(
            "UPDATE queue_jobs SET state='done' WHERE id=$1 AND queue='bounces' AND state='ready'",
        )
        .bind(job.0.to_string())
        .execute(&mut *tx)
        .await
        .map_err(db_error)?
        .rows_affected();
        if changed != 1 {
            return Err(Error::Conflict(
                "acknowledgement requires a ready bounce job".into(),
            ));
        }
        Self::record_tx_with_context(
            &mut tx,
            context,
            "queue.acknowledge_bounce",
            "queue_job",
            &job.0.to_string(),
            serde_json::json!({"previous_state": "ready", "state": "done", "reason": reason}),
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }

    /// Resolve one ambiguous recipient after the operator checks the relay.
    /// Retry requires explicit acknowledgement: the original might have arrived.
    /// Other recipients, including known successes, are never reset.
    /// # Errors
    /// Rejects active jobs, non-ambiguous recipients, missing reasons, and
    /// unacknowledged retries. Database/audit failure rolls the whole write back.
    pub async fn resolve_delivery(
        &self,
        resolution: DeliveryResolution<'_>,
        context: &AuditContext,
        now_ms: i64,
    ) -> Result<()> {
        let reason = resolution.reason.trim();
        if reason.is_empty() || reason.len() > 2048 || reason.chars().any(char::is_control) {
            return Err(Error::Validation(
                "resolution requires a bounded, single-line reason".into(),
            ));
        }
        if resolution.outcome == ResolutionOutcome::Retry && !resolution.acknowledge_duplicate_risk
        {
            return Err(Error::Validation(
                "retry requires explicit duplicate-risk acknowledgement".into(),
            ));
        }
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        // First statement is a conditional write: acquires the row/writer lock
        // before inspecting recipients on both PostgreSQL and SQLite.
        let locked = sqlx::query("UPDATE queue_jobs SET last_error=last_error WHERE id=$1 AND ((queue='out' AND state='done') OR (queue='shunt' AND state='shunted'))")
            .bind(resolution.job.0.to_string()).execute(&mut *tx).await.map_err(db_error)?.rows_affected();
        if locked != 1 {
            return Err(Error::Conflict(
                "resolution requires an inactive outgoing job".into(),
            ));
        }
        let status = match resolution.outcome {
            ResolutionOutcome::Sent => "sent",
            ResolutionOutcome::Failed => "failed",
            ResolutionOutcome::Retry => "pending",
        };
        let changed = sqlx::query("UPDATE delivery_recipients SET status=$1,detail=$2,attempt_token=NULL WHERE job_id=$3 AND email=$4 AND status='ambiguous'")
            .bind(status).bind(reason).bind(resolution.job.0.to_string()).bind(resolution.email)
            .execute(&mut *tx).await.map_err(db_error)?.rows_affected();
        if changed != 1 {
            return Err(Error::Conflict(
                "recipient is not quarantined as ambiguous".into(),
            ));
        }
        if resolution.outcome == ResolutionOutcome::Retry {
            sqlx::query("UPDATE queue_jobs SET queue='out',state='ready',attempts=0,run_after=$1,locked_by=NULL,lease_token=NULL,lease_until=NULL,last_error='' WHERE id=$2")
                .bind(now_ms).bind(resolution.job.0.to_string()).execute(&mut *tx).await.map_err(db_error)?;
        }
        Self::record_tx_with_context(&mut tx, context, "queue.resolve", "queue_job", &resolution.job.0.to_string(),
            serde_json::json!({"recipient": resolution.email, "previous_status": "ambiguous", "outcome": resolution.outcome,
                "reason": reason, "duplicate_risk_acknowledged": resolution.acknowledge_duplicate_risk})).await?;
        tx.commit().await.map_err(db_error)
    }
}
