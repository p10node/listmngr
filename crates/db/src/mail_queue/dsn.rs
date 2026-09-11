//! Immutable producer binding and pre-SMTP issuance. No incoming consumer.
use super::{Any, Error, MessageId, Queue, QueueJob, Result, Row, Transaction, Uuid, db_error};
use listmngr_core::dsn_issuance::Issuer;

pub(super) async fn bind_message(
    tx: &mut Transaction<'_, Any>,
    message: MessageId,
    context: &str,
) -> Result<()> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(context) else {
        return Ok(());
    };
    let Some(list) = value["list_id"].as_str() else {
        return Ok(());
    };
    let incarnation:Option<String>=sqlx::query_scalar("UPDATE mailing_lists SET delivery_incarnation=delivery_incarnation WHERE list_id=$1 RETURNING delivery_incarnation")
        .bind(list).fetch_optional(&mut **tx).await.map_err(db_error)?;
    if let Some(incarnation) = incarnation {
        sqlx::query("INSERT INTO message_delivery_bindings(message_id,list_id,list_incarnation) VALUES($1,$2,$3)")
            .bind(message.0.to_string()).bind(list).bind(incarnation).execute(&mut **tx).await.map_err(db_error)?;
    }
    Ok(())
}
fn invalid() -> Error {
    Error::Validation("unbound or stale DSN delivery authority".into())
}

pub(super) async fn issue(
    tx: &mut Transaction<'_, Any>,
    job: &QueueJob,
    attempt: &str,
    recipients: &[String],
    now: i64,
    issuer: &Issuer,
) -> Result<Vec<String>> {
    // Public Lease.job/message/context is never authority. This job was decoded under its lock.
    let row=sqlx::query("SELECT b.list_id,b.list_incarnation FROM message_delivery_bindings b WHERE b.message_id=$1 AND NOT EXISTS(SELECT 1 FROM workflow_notices WHERE job_id=$2) AND NOT EXISTS(SELECT 1 FROM owner_deliveries WHERE job_id=$2) AND NOT EXISTS(SELECT 1 FROM digest_deliveries WHERE job_id=$2)")
        .bind(job.message_id.0.to_string()).bind(job.id.0.to_string()).fetch_optional(&mut **tx).await.map_err(db_error)?.ok_or_else(invalid)?;
    let list: String = row.try_get("list_id").map_err(db_error)?;
    let incarnation: String = row.try_get("list_incarnation").map_err(db_error)?;
    let matched=sqlx::query("UPDATE mailing_lists SET delivery_incarnation=delivery_incarnation WHERE list_id=$1 AND delivery_incarnation=$2")
        .bind(&list).bind(&incarnation).execute(&mut **tx).await.map_err(db_error)?.rows_affected();
    if matched != 1 || job.queue != Queue::Out {
        return Err(invalid());
    }
    let expires = issuer.expires_at(now)?;
    let mut result = Vec::new();
    for recipient in recipients {
        let canonical = listmngr_core::Address::new(recipient, String::new())?.email;
        let member:Option<String>=sqlx::query_scalar("UPDATE members SET bounce_score=bounce_score WHERE list_id=$1 AND role='member' AND address_id IN(SELECT id FROM addresses WHERE email=$2 AND original_email=$3) AND id=(SELECT member_incarnation FROM delivery_recipients WHERE job_id=$4 AND email=$3) RETURNING id")
            .bind(&list).bind(&canonical).bind(recipient).bind(job.id.0.to_string()).fetch_optional(&mut **tx).await.map_err(db_error)?;
        let member = member.ok_or_else(invalid)?;
        crate::smtp_bounces::lock_preferences(tx, &member).await?;
        let row = sqlx::query(&format!("{} WHERE m.id=$1", super::plan::SNAPSHOT_SELECT))
            .bind(&member)
            .fetch_one(&mut **tx)
            .await
            .map_err(db_error)?;
        let planned: Option<String> = sqlx::query_scalar(
            "SELECT authority_snapshot FROM delivery_recipients WHERE job_id=$1 AND email=$2",
        )
        .bind(job.id.0.to_string())
        .bind(recipient)
        .fetch_one(&mut **tx)
        .await
        .map_err(db_error)?;
        if planned.as_deref() != Some(super::plan::snapshot(&row)?.as_str()) {
            return Err(invalid());
        }
        if row.try_get::<String, _>("status").map_err(db_error)? != "enabled"
            || row.try_get::<String, _>("mode").map_err(db_error)? != "regular"
        {
            return Err(invalid());
        }
        let id = Uuid::from_bytes(rand::random()).simple().to_string();
        let claims=serde_json::json!({"version":1,"key_id":issuer.key_id(),"nonce":id,"job_id":job.id.0.to_string(),"message_id":job.message_id.0.to_string(),"list_id":list,"list_incarnation":incarnation,"recipient":recipient,"canonical_recipient":canonical,"member_id":member,"address_id":row.try_get::<String,_>("address_id").map_err(db_error)?,"user_id":row.try_get::<String,_>("user_id").map_err(db_error)?,"preferences":[[row.try_get::<String,_>("mp").map_err(db_error)?,row.try_get::<i64,_>("mg").map_err(db_error)?.to_string()],[row.try_get::<String,_>("ap").map_err(db_error)?,row.try_get::<i64,_>("ag").map_err(db_error)?.to_string()],[row.try_get::<String,_>("up").map_err(db_error)?,row.try_get::<i64,_>("ug").map_err(db_error)?.to_string()]],"issued_at":now,"expires_at":expires,"attempt":attempt}).to_string();
        let envid = issuer.issue(&claims, &id);
        sqlx::query("INSERT INTO dsn_issuances(id,job_id,message_id,recipient,attempt_token,claims,envid,issued_at,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)")
            .bind(&id).bind(job.id.0.to_string()).bind(job.message_id.0.to_string()).bind(recipient).bind(attempt).bind(claims).bind(&envid).bind(now).bind(expires).execute(&mut **tx).await.map_err(db_error)?;
        let at = chrono::DateTime::from_timestamp_millis(now).ok_or_else(invalid)?;
        sqlx::query("INSERT INTO audit_log(id,at,action,target_type,target_id,diff) VALUES($1,$2,'dsn.issue','dsn_issuance',$3,$4)")
            .bind(Uuid::now_v7().to_string()).bind(at.to_rfc3339()).bind(&id)
            .bind(serde_json::json!({"version":1,"key_id":issuer.key_id(),"job_id":job.id.0.to_string()}).to_string())
            .execute(&mut **tx).await.map_err(db_error)?;
        result.push(envid);
    }
    Ok(result)
}
