//! Typed producer-selected authority. No caller JSON or mailbox lookup can construct it.
use super::{Any, Error, JobId, Result, Row, Transaction, db_error};
use listmngr_core::ListId;

#[derive(Clone, Debug)]
pub struct RecipientPlan {
    list_id: String,
    recipients: Vec<(String, String, String)>,
}
impl RecipientPlan {
    /// Narrow an authorized plan; never adds new recipients.
    #[must_use]
    pub fn restrict_to(mut self, emails: &[String]) -> Self {
        self.recipients.retain(|r| emails.contains(&r.0));
        self
    }
    #[must_use]
    pub fn emails(&self) -> Vec<String> {
        self.recipients.iter().map(|r| r.0.clone()).collect()
    }
}

pub(super) const SNAPSHOT_SELECT: &str = "SELECT m.id AS member_id,m.list_id,l.delivery_incarnation,a.email,a.original_email,m.address_id,COALESCE(m.user_id,'') AS user_id,COALESCE(a.user_id,'') AS address_user,mp.id AS mp,mp.delivery_generation AS mg,COALESCE(ap.id,'') AS ap,COALESCE(ap.delivery_generation,0) AS ag,COALESCE(up.id,'') AS up,COALESCE(up.delivery_generation,0) AS ug,COALESCE(mp.delivery_status,ap.delivery_status,up.delivery_status,'enabled') AS status,COALESCE(mp.delivery_mode,ap.delivery_mode,up.delivery_mode,'regular') AS mode,COALESCE(mp.receive_own_postings,ap.receive_own_postings,up.receive_own_postings,1) AS own,COALESCE(mp.receive_list_copy,ap.receive_list_copy,up.receive_list_copy,1) AS copy FROM members m JOIN mailing_lists l ON l.list_id=m.list_id JOIN addresses a ON a.id=m.address_id JOIN preferences mp ON mp.id=m.preferences_id LEFT JOIN preferences ap ON ap.id=a.preferences_id LEFT JOIN users u ON u.id=m.user_id LEFT JOIN preferences up ON up.id=u.preferences_id";

pub(super) fn snapshot(row: &sqlx::any::AnyRow) -> Result<String> {
    let mut values = Vec::new();
    for column in [
        "member_id",
        "list_id",
        "delivery_incarnation",
        "email",
        "original_email",
        "address_id",
        "user_id",
        "address_user",
        "mp",
        "ap",
        "up",
        "status",
        "mode",
    ] {
        values.push(row.try_get::<String, _>(column).map_err(db_error)?);
    }
    for column in ["mg", "ag", "ug", "own", "copy"] {
        values.push(row.try_get::<i64, _>(column).map_err(db_error)?.to_string());
    }
    serde_json::to_string(&values).map_err(db_error)
}

impl super::MailQueueRepo<'_> {
    /// Select ordinary recipients and their identity/preference authority in one SQL snapshot.
    /// # Errors
    /// Returns database or malformed persisted preference errors.
    pub async fn plan_recipients(
        &self,
        list: &ListId,
        sender: &str,
        raw: &[u8],
    ) -> Result<RecipientPlan> {
        let mut tx = self.db.pool().begin().await.map_err(db_error)?;
        let plan = select(&mut tx, list, sender, raw).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(plan)
    }
}

pub async fn select(
    tx: &mut Transaction<'_, Any>,
    list: &ListId,
    sender: &str,
    raw: &[u8],
) -> Result<RecipientPlan> {
    let direct = listmngr_mail::visible_recipients::mailboxes(raw).unwrap_or_default();
    let rows = sqlx::query(&format!(
        "{SNAPSHOT_SELECT} WHERE m.list_id=$1 AND m.role='member' ORDER BY a.email,m.id"
    ))
    .bind(list.as_str())
    .fetch_all(&mut **tx)
    .await
    .map_err(db_error)?;
    let mut recipients = Vec::new();
    for row in rows {
        let email: String = row.try_get("email").map_err(db_error)?;
        if row.try_get::<i64, _>("copy").map_err(db_error)? == 0 && direct.contains(&email) {
            continue;
        }
        let candidate = listmngr_pipeline::CandidateRecipient {
            email,
            delivery_status: row
                .try_get::<String, _>("status")
                .map_err(db_error)?
                .parse()?,
            delivery_mode: row
                .try_get::<String, _>("mode")
                .map_err(db_error)?
                .parse()?,
            receive_own_postings: row.try_get::<i64, _>("own").map_err(db_error)? != 0,
        };
        if !listmngr_pipeline::select_recipients(&[candidate], sender).is_empty() {
            recipients.push((
                row.try_get("original_email").map_err(db_error)?,
                row.try_get("member_id").map_err(db_error)?,
                snapshot(&row)?,
            ));
        }
    }
    Ok(RecipientPlan {
        list_id: list.to_string(),
        recipients,
    })
}

pub async fn bind(tx: &mut Transaction<'_, Any>, job: JobId, plan: &RecipientPlan) -> Result<()> {
    let bound: Option<String> = sqlx::query_scalar("SELECT b.list_id FROM message_delivery_bindings b JOIN queue_jobs q ON q.message_id=b.message_id WHERE q.id=$1 AND q.queue='out'")
        .bind(job.0.to_string()).fetch_optional(&mut **tx).await.map_err(db_error)?;
    if bound.is_none() {
        return Ok(());
    } // Historical messages stay unbound, including moderation.
    if bound.as_deref() != Some(plan.list_id.as_str()) {
        return Err(Error::Validation(
            "recipient plan belongs to a different producer list".into(),
        ));
    }
    for (email, member, snapshot) in &plan.recipients {
        let changed = sqlx::query("UPDATE delivery_recipients SET member_incarnation=$1,authority_snapshot=$2 WHERE job_id=$3 AND email=$4")
            .bind(member).bind(snapshot).bind(job.0.to_string()).bind(email).execute(&mut **tx).await.map_err(db_error)?.rows_affected();
        if changed != 1 {
            return Err(Error::Validation(
                "recipient plan does not match publication".into(),
            ));
        }
    }
    Ok(())
}
