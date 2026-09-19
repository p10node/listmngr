//! Explicit bounded operator maintenance, never a serve scheduler.
use crate::{AuditContext, Database, db_error};
use chrono::{DateTime, Duration, Utc};
use listmngr_core::{Error, ListId, Result};
use serde::Serialize;
use sqlx::{Any, Row, Transaction};
use uuid::Uuid;

#[derive(Debug, Default, Serialize)]
pub struct BounceSweepSummary {
    pub scanned: u32,
    pub warned: u32,
    pub removed: u32,
    pub failed: u32,
    /// Last scanned ID, including non-due and failed items. Empty page returns null.
    pub next_cursor: Option<Uuid>,
}
#[derive(Debug)]
pub struct BounceMaintenanceRepository<'a> {
    db: &'a Database,
}
impl Database {
    #[must_use]
    pub const fn bounce_maintenance(&self) -> BounceMaintenanceRepository<'_> {
        BounceMaintenanceRepository { db: self }
    }
}
impl BounceMaintenanceRepository<'_> {
    /// Real clock is sampled inside each reserved member transaction.
    /// # Errors
    /// Returns database or invalid page errors; individual failures are counted.
    pub async fn sweep(&self, limit: u32, after: Option<Uuid>) -> Result<BounceSweepSummary> {
        self.sweep_with_clock(limit, after, Utc::now).await
    }
    /// Deterministic database boundary; not exposed as a CLI clock override.
    /// # Errors
    /// Returns database or invalid page errors; individual failures are counted.
    pub async fn sweep_at(
        &self,
        limit: u32,
        after: Option<Uuid>,
        now: DateTime<Utc>,
    ) -> Result<BounceSweepSummary> {
        self.sweep_with_clock(limit, after, || now).await
    }
    async fn sweep_with_clock(
        &self,
        limit: u32,
        after: Option<Uuid>,
        clock: impl Fn() -> DateTime<Utc> + Send + Sync,
    ) -> Result<BounceSweepSummary> {
        if !(1..=1000).contains(&limit) {
            return Err(Error::Validation(
                "bounce sweep limit must be 1..1000".into(),
            ));
        }
        // SQL bounds enumeration, not a full disabled roster followed by truncation.
        let ids: Vec<String> = sqlx::query_scalar("SELECT m.id FROM members m JOIN preferences p ON p.id=m.preferences_id JOIN mailing_lists l ON l.list_id=m.list_id WHERE m.role='member' AND p.delivery_status='by_bounces' AND l.process_bounces=1 AND m.id>$1 ORDER BY m.id LIMIT $2")
            .bind(after.map_or_else(String::new, |id| id.to_string())).bind(i64::from(limit)).fetch_all(self.db.pool()).await.map_err(db_error)?;
        let mut summary = BounceSweepSummary::default();
        for id in ids {
            summary.scanned += 1;
            summary.next_cursor = Some(id.parse().map_err(db_error)?);
            match self.member(&id, &clock).await {
                Ok(Action::Warn) => summary.warned += 1,
                Ok(Action::Remove) => summary.removed += 1,
                Ok(Action::None) => {}
                Err(_) => summary.failed += 1,
            }
        }
        Ok(summary)
    }
    async fn member(
        &self,
        id: &str,
        clock: &(impl Fn() -> DateTime<Utc> + Sync),
    ) -> Result<Action> {
        let mut tx = self.db.write_tx().await?;
        match maintain(&mut tx, self.db, id, clock).await {
            Ok(action) => {
                tx.commit().await.map_err(db_error)?;
                Ok(action)
            }
            Err(error) => {
                tx.rollback().await.map_err(db_error)?;
                Err(error)
            }
        }
    }
}
#[derive(Debug)]
enum Action {
    None,
    Warn,
    Remove,
}

async fn maintain(
    tx: &mut Transaction<'_, Any>,
    db: &Database,
    id: &str,
    clock: &(impl Fn() -> DateTime<Utc> + Sync),
) -> Result<Action> {
    // First statement is a writer reservation: no SQLite deferred read/upgrade race.
    let list: Option<String> = sqlx::query_scalar(
        "UPDATE members SET id=id WHERE id=$1 AND role='member' RETURNING list_id",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_error)?;
    let Some(list) = list else {
        return Ok(Action::None);
    };
    crate::smtp_bounces::lock_preferences(tx, id).await?;
    sqlx::query("UPDATE mailing_lists SET list_id=list_id WHERE list_id=$1")
        .bind(&list)
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    // Reread after waits, coordinating with explicit preference/config updates.
    let row = sqlx::query("SELECT m.total_warnings_sent,m.last_warning_sent,a.original_email,l.bounce_you_are_disabled_warnings,l.bounce_you_are_disabled_warnings_interval FROM members m JOIN preferences mp ON mp.id=m.preferences_id JOIN addresses a ON a.id=m.address_id LEFT JOIN preferences ap ON ap.id=a.preferences_id LEFT JOIN users u ON u.id=m.user_id LEFT JOIN preferences up ON up.id=u.preferences_id JOIN mailing_lists l ON l.list_id=m.list_id WHERE m.id=$1 AND m.role='member' AND mp.delivery_status='by_bounces' AND COALESCE(mp.delivery_status,ap.delivery_status,up.delivery_status,'enabled')='by_bounces' AND l.process_bounces=1")
        .bind(id).fetch_optional(&mut **tx).await.map_err(db_error)?;
    let Some(row) = row else {
        return Ok(Action::None);
    };
    let total: i64 = row.try_get("total_warnings_sent").map_err(db_error)?;
    let count: i64 = row
        .try_get("bounce_you_are_disabled_warnings")
        .map_err(db_error)?;
    let interval: i64 = row
        .try_get("bounce_you_are_disabled_warnings_interval")
        .map_err(db_error)?;
    let receipt: Option<String> = row.try_get("last_warning_sent").map_err(db_error)?;
    let receipt = receipt.as_deref().map(crate::parse_time).transpose()?;
    let now = clock();
    let due = receipt.is_none_or(|at| now.signed_duration_since(at) >= Duration::days(interval));
    if !due && count != 0 {
        return Ok(Action::None);
    }
    let email: String = row.try_get("original_email").map_err(db_error)?;
    let list: ListId = list.parse()?;
    if total >= count {
        if !crate::workflows::delete_member_with_goodbye(tx, db, id).await? {
            return Ok(Action::None);
        }
        crate::workflows::enqueue_removal_notice(tx, db, &list, &email, now.timestamp_millis())
            .await?;
        Database::record_tx_with_context(tx, &AuditContext::system(), "bounce.remove", "member", id,
            serde_json::json!({"member_id":id,"list_id":list.as_str(),"warning_number":total,"warning_count":count})).await?;
        return Ok(Action::Remove);
    }
    sqlx::query("UPDATE members SET total_warnings_sent=total_warnings_sent+1,last_warning_sent=$1 WHERE id=$2")
        .bind(now.to_rfc3339()).bind(id).execute(&mut **tx).await.map_err(db_error)?;
    crate::workflows::enqueue_warning_notice(tx, db, &list, &email, now.timestamp_millis()).await?;
    Database::record_tx_with_context(
        tx,
        &AuditContext::system(),
        "bounce.warning",
        "member",
        id,
        serde_json::json!({"member_id":id,"warning_number":total+1,"warning_count":count}),
    )
    .await?;
    Ok(Action::Warn)
}
