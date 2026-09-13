//! Direct SMTP failure metadata and opt-in RCPT signal scoring; not inbound DSN processing.
use crate::{db_error, mail_queue::Lease};
use listmngr_core::{ListId, Result};
use sqlx::{Any, Row, Transaction};
use uuid::Uuid;

// Called before resolving the attempt. Only the stored, currently reserved
// outgoing recipient is authority, never caller-editable lease job metadata.
pub async fn record(
    tx: &mut Transaction<'_, Any>,
    db: &crate::Database,
    lease: &Lease,
    recipient: &str,
    now_ms: i64,
    failure: Option<listmngr_core::SmtpFailure>,
) -> Result<()> {
    let row = sqlx::query("SELECT m.context,m.id AS message_id FROM queue_jobs q JOIN messages m ON m.id=q.message_id JOIN delivery_recipients r ON r.job_id=q.id WHERE q.id=$1 AND q.queue='out' AND q.state='leased' AND q.lease_token=$2 AND q.lease_until>$3 AND r.email=$4 AND r.status='ambiguous' AND r.attempt_token=$2 AND NOT EXISTS(SELECT 1 FROM workflow_notices n WHERE n.job_id=q.id) AND NOT EXISTS(SELECT 1 FROM owner_deliveries o WHERE o.job_id=q.id) AND NOT EXISTS(SELECT 1 FROM digest_deliveries d WHERE d.job_id=q.id)")
        .bind(lease.job.id.0.to_string()).bind(&lease.token).bind(now_ms).bind(recipient)
        .fetch_optional(&mut **tx).await.map_err(db_error)?;
    let Some(row) = row else { return Ok(()) };
    let context: String = row.try_get("context").map_err(db_error)?;
    let Ok(context) = serde_json::from_str::<serde_json::Value>(&context) else {
        return Ok(());
    };
    let Some(list) = context["list_id"]
        .as_str()
        .and_then(|s| s.parse::<ListId>().ok())
    else {
        return Ok(());
    };
    let message: String = row.try_get("message_id").map_err(db_error)?;
    let id = Uuid::now_v7().to_string();
    let inserted = sqlx::query("INSERT INTO bounce_events(id,list_id,recipient,job_id,message_id,created_at,source,context,processed,smtp_stage,smtp_code) SELECT $1,list_id,$2,$3,$4,$5,'smtp_permanent_failure','normal',0,$7,$8 FROM mailing_lists WHERE list_id=$6 ON CONFLICT(job_id,recipient) DO NOTHING")
        .bind(&id).bind(recipient).bind(lease.job.id.0.to_string()).bind(message).bind(now_ms).bind(list.as_str())
        .bind(failure.map(|f| f.stage.as_str())).bind(failure.map(|f| i64::from(f.code)))
        .execute(&mut **tx).await.map_err(db_error)?.rows_affected();
    if inserted > 0 {
        let at = chrono::DateTime::from_timestamp_millis(now_ms)
            .ok_or_else(|| listmngr_core::Error::Validation("timestamp out of range".into()))?;
        sqlx::query("INSERT INTO audit_log(id,at,action,target_type,target_id,diff) VALUES($1,$2,'bounce.record','bounce_event',$3,'{}')")
            .bind(Uuid::now_v7().to_string()).bind(at.to_rfc3339()).bind(&id)
            .execute(&mut **tx).await.map_err(db_error)?;
        if failure.is_some_and(|f| f.stage.as_str() == "rcpt" && (500..600).contains(&f.code)) {
            score(tx, db, &list, recipient, &id, at).await?;
        }
    }
    Ok(())
}

// Member -> address -> preference IDs in sorted order. Preference UPDATE writers
// share the row locks; the address lock also fences first-time preference creation.
// Read effective status in a NEW statement after all waits (READ COMMITTED).
pub async fn lock_preferences(tx: &mut Transaction<'_, Any>, member: &str) -> Result<()> {
    sqlx::query("UPDATE addresses SET preferences_id=preferences_id WHERE id=(SELECT address_id FROM members WHERE id=$1)")
        .bind(member).execute(&mut **tx).await.map_err(db_error)?;
    let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM preferences WHERE id IN (SELECT preferences_id FROM members WHERE id=$1 UNION SELECT a.preferences_id FROM addresses a JOIN members m ON m.address_id=a.id WHERE m.id=$1 UNION SELECT u.preferences_id FROM users u JOIN members m ON m.user_id=u.id WHERE m.id=$1) ORDER BY id")
        .bind(member).fetch_all(&mut **tx).await.map_err(db_error)?;
    for id in ids {
        sqlx::query("UPDATE preferences SET id=id WHERE id=$1")
            .bind(id)
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
    }
    Ok(())
}

pub async fn score(
    tx: &mut Transaction<'_, Any>,
    db: &crate::Database,
    list: &ListId,
    recipient: &str,
    id: &str,
    at: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    let email = listmngr_core::Address::new(recipient, String::new())?.email;
    // Reserve the member row before reading its score, including on PostgreSQL.
    let row = sqlx::query("UPDATE members SET bounce_score=bounce_score WHERE list_id=$1 AND role='member' AND address_id IN (SELECT id FROM addresses WHERE email=$2) AND EXISTS(SELECT 1 FROM mailing_lists WHERE list_id=$1 AND process_bounces=1) RETURNING id,bounce_score,last_bounce_received")
        .bind(list.as_str()).bind(email).fetch_optional(&mut **tx).await.map_err(db_error)?;
    if let Some(row) = row {
        let member: String = row.try_get("id").map_err(db_error)?;
        lock_preferences(tx, &member).await?;
        // Same precedence as PreferencesRepo::resolve_member: member, address,
        // member's user, then enabled system default. In-flight old recipients
        // must not modify the score/reset or receipt of an already-disabled member.
        let status: String = sqlx::query_scalar("SELECT COALESCE(mp.delivery_status,ap.delivery_status,up.delivery_status,'enabled') FROM members m JOIN preferences mp ON mp.id=m.preferences_id JOIN addresses a ON a.id=m.address_id LEFT JOIN preferences ap ON ap.id=a.preferences_id LEFT JOIN users u ON u.id=m.user_id LEFT JOIN preferences up ON up.id=u.preferences_id WHERE m.id=$1")
            .bind(&member).fetch_one(&mut **tx).await.map_err(db_error)?;
        if status != "enabled" {
            return Ok(());
        }
        let previous: Option<String> = row.try_get("last_bounce_received").map_err(db_error)?;
        let received_at = previous
            .map(|v| {
                chrono::DateTime::parse_from_rfc3339(&v).map(|t| t.with_timezone(&chrono::Utc))
            })
            .transpose()
            .map_err(|_| listmngr_core::Error::Validation("invalid last_bounce_received".into()))?;
        let days: i64 = sqlx::query_scalar(
            "SELECT bounce_info_stale_after FROM mailing_lists WHERE list_id=$1",
        )
        .bind(list.as_str())
        .fetch_one(&mut **tx)
        .await
        .map_err(db_error)?;
        let threshold: f64 =
            sqlx::query_scalar("SELECT bounce_score_threshold FROM mailing_lists WHERE list_id=$1")
                .bind(list.as_str())
                .fetch_one(&mut **tx)
                .await
                .map_err(db_error)?;
        if received_at.is_none_or(|prior| at > prior) {
            let old: f64 = row.try_get("bounce_score").map_err(db_error)?;
            let next = if received_at.is_some_and(|prior| at.date_naive() == prior.date_naive()) {
                old
            } else if received_at
                .is_none_or(|prior| at.signed_duration_since(prior) >= chrono::Duration::days(days))
            {
                1.0
            } else {
                old + 1.0
            };
            let fresh = received_at.is_none_or(|prior| at.date_naive() != prior.date_naive());
            let disable = fresh && next >= threshold;
            sqlx::query("UPDATE members SET bounce_score=$1,last_bounce_received=$2 WHERE id=$3")
                .bind(if disable { 0.0 } else { next })
                .bind(at.to_rfc3339())
                .bind(&member)
                .execute(&mut **tx)
                .await
                .map_err(db_error)?;
            if fresh {
                crate::workflows::enqueue_increment_notice(
                    tx,
                    db,
                    list,
                    recipient,
                    at.timestamp_millis(),
                    next,
                )
                .await?;
            }
            if disable {
                sqlx::query(
                    "UPDATE members SET total_warnings_sent=0,last_warning_sent=NULL WHERE id=$1",
                )
                .bind(&member)
                .execute(&mut **tx)
                .await
                .map_err(db_error)?;
                crate::workflows::enqueue_disable_notice(
                    tx,
                    db,
                    list,
                    recipient,
                    at.timestamp_millis(),
                )
                .await?;
                sqlx::query("UPDATE preferences SET delivery_status='by_bounces' WHERE id=(SELECT preferences_id FROM members WHERE id=$1)")
                    .bind(&member).execute(&mut **tx).await.map_err(db_error)?;
                sqlx::query("INSERT INTO audit_log(id,at,action,target_type,target_id,diff) VALUES($1,$2,'bounce.disable','member',$3,'{\"delivery_status\":\"by_bounces\",\"bounce_score\":0}')")
                    .bind(Uuid::now_v7().to_string()).bind(at.to_rfc3339()).bind(&member).execute(&mut **tx).await.map_err(db_error)?;
            }
        }
        sqlx::query("UPDATE bounce_events SET processed=1 WHERE id=$1")
            .bind(id)
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
        sqlx::query("INSERT INTO audit_log(id,at,action,target_type,target_id,diff) VALUES($1,$2,'bounce.score','member',$3,'{}')")
            .bind(Uuid::now_v7().to_string()).bind(at.to_rfc3339()).bind(member).execute(&mut **tx).await.map_err(db_error)?;
    }
    Ok(())
}
