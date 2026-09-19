//! Experimental owner forwarding: producer-owned provenance, never raw headers.
use crate::mail_queue::JobId;
use crate::mail_queue::{
    ChildJob, Lease, LeaseClock, Queue, SystemLeaseClock, ack_leased_job, insert_child_job,
};
use crate::{Database, db_error};
use listmngr_core::{Error, ListId, Result};

#[derive(Debug)]
pub struct OwnerMailRepo<'a> {
    db: &'a Database,
    clock: Option<&'a dyn LeaseClock>,
}
impl Database {
    #[must_use]
    pub fn owner_mail(&self) -> OwnerMailRepo<'_> {
        OwnerMailRepo {
            db: self,
            clock: None,
        }
    }
}
impl<'a> OwnerMailRepo<'a> {
    #[must_use]
    pub fn with_clock(mut self, clock: &'a dyn LeaseClock) -> Self {
        self.clock = Some(clock);
        self
    }
    #[must_use]
    pub fn live(self) -> Self {
        self.with_clock(&SystemLeaseClock)
    }

    /// Job-bound authority produced only by the atomic owner handoff.
    /// # Errors
    /// Returns database failure.
    pub async fn is_delivery(&self, job: JobId) -> Result<bool> {
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM owner_deliveries o JOIN queue_jobs q ON q.id=o.job_id WHERE o.job_id=$1 AND q.queue='out'")
            .bind(job.0.to_string()).fetch_one(self.db.pool()).await.map_err(db_error)?;
        Ok(n == 1)
    }

    /// Snapshot owner/moderator addresses once each, without post fanout.
    /// # Errors
    /// Invalid route, missing owners, stale lease, or database/audit failure.
    pub async fn forward(&self, lease: &Lease, attempts: i64, fixture_ms: i64) -> Result<()> {
        if lease.job.queue != Queue::In {
            return Err(Error::Validation("inbound owner lease required".into()));
        }
        let message = self.db.mail_queue().message(lease.job.message_id).await?;
        let context: serde_json::Value =
            serde_json::from_str(&message.context).map_err(db_error)?;
        if context["owner_route"] != true {
            return Err(Error::Validation("owner route required".into()));
        }
        let list: ListId = context["list_id"]
            .as_str()
            .ok_or_else(|| Error::Validation("missing owner list".into()))?
            .parse()?;
        if !listmngr_mail::owner::allows_forward(
            &message.raw,
            context["envelope_sender"].as_str(),
            &list,
        ) {
            return Err(Error::Validation("unsafe owner forwarding request".into()));
        }
        let mut tx = self.db.write_tx().await?;
        let locked = sqlx::query("UPDATE mailing_lists SET list_id=list_id WHERE list_id=$1")
            .bind(list.as_str())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?
            .rows_affected();
        if locked != 1 {
            return Err(Error::NotFound(list.to_string()));
        }
        let queue = self.clock.map_or_else(
            || self.db.mail_queue(),
            |clock| self.db.mail_queue().with_clock(clock),
        );
        let now = queue.lock_time(&mut tx, lease, fixture_ms).await?;
        let deadline: i64 = sqlx::query_scalar(
            "SELECT lease_until FROM queue_jobs WHERE id=$1 AND state='leased' AND lease_token=$2",
        )
        .bind(lease.job.id.0.to_string())
        .bind(&lease.token)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?
        .ok_or_else(|| Error::Conflict("stale owner lease".into()))?;
        let recipients: Vec<String> = sqlx::query_scalar("SELECT a.original_email FROM addresses a WHERE EXISTS (SELECT 1 FROM members m WHERE m.address_id=a.id AND m.list_id=$1 AND m.role IN ('owner','moderator')) ORDER BY a.email")
            .bind(list.as_str()).fetch_all(&mut *tx).await.map_err(db_error)?;
        if recipients.is_empty() {
            return Err(Error::Validation("list has no owner recipients".into()));
        }
        if recipients.iter().any(|email| {
            !listmngr_mail::owner::safe_mailbox(email)
                || listmngr_mail::owner::points_to_list(email, &list)
        }) {
            return Err(Error::Validation("unsafe owner recipient roster".into()));
        }
        let child = insert_child_job(
            &mut tx,
            message.id,
            &ChildJob {
                queue: Queue::Out,
                max_attempts: attempts,
                recipients,
            },
            now,
        )
        .await?;
        sqlx::query("INSERT INTO owner_deliveries(job_id) VALUES($1)")
            .bind(child.id.0.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        crate::mail_queue::audit(&mut tx, &child, "owner.forward", now).await?;
        ack_leased_job(&mut tx, lease, queue.time(fixture_ms)).await?;
        // ACK itself writes an audit row which may wait. Recheck after that
        // final write, using the locked live deadline (not the claim snapshot).
        if queue.time(fixture_ms) >= deadline {
            return Err(Error::Conflict(
                "expired owner lease after publication".into(),
            ));
        }
        tx.commit().await.map_err(db_error)
    }
}
