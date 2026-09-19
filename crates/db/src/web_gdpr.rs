//! Data portability and erasure for one account.
//!
//! `export_user` is a read of everything stored about an account — never
//! a password hash, a token secret or a session token. `erase_user` removes
//! the account the way self-service deletion does (`web_delete`), for an
//! administrator or the command line, with the same last-server-owner
//! guard and one audit event in the same transaction.
use crate::{
    AuditContext, Database, db_error,
    web_delete::{Deleted, remove_everything},
    web_sessions::WebSession,
};
use listmngr_core::{Error, Result, UserId};
use serde_json::{Value, json};
use sqlx::Row as _;

/// Audit events about the account included in an export, at most.
const EXPORTED_AUDIT_EVENTS: i64 = 1000;

/// The preferences row an owner table row points at, if any.
async fn preferences_of(db: &Database, table: &str, id: &str) -> Result<Value> {
    let preferences: Option<String> =
        sqlx::query_scalar(&format!("SELECT preferences_id FROM {table} WHERE id=$1"))
            .bind(id)
            .fetch_optional(db.pool())
            .await
            .map_err(db_error)?
            .flatten();
    match preferences {
        Some(preferences) => preferences_row(db, &preferences).await,
        None => Ok(Value::Null),
    }
}

async fn preferences_row(db: &Database, id: &str) -> Result<Value> {
    let row = sqlx::query("SELECT acknowledge_posts,hide_address,preferred_language,receive_list_copy,receive_own_postings,delivery_mode,delivery_status FROM preferences WHERE id=$1")
        .bind(id)
        .fetch_optional(db.pool())
        .await
        .map_err(db_error)?;
    let Some(row) = row else {
        return Ok(Value::Null);
    };
    let flag = |name: &str| -> Result<Value> {
        Ok(row
            .try_get::<Option<i64>, _>(name)
            .map_err(db_error)?
            .map_or(Value::Null, |value| Value::Bool(value != 0)))
    };
    let text = |name: &str| -> Result<Value> {
        Ok(row
            .try_get::<Option<String>, _>(name)
            .map_err(db_error)?
            .map_or(Value::Null, Value::String))
    };
    Ok(json!({
        "acknowledge_posts": flag("acknowledge_posts")?,
        "hide_address": flag("hide_address")?,
        "preferred_language": text("preferred_language")?,
        "receive_list_copy": flag("receive_list_copy")?,
        "receive_own_postings": flag("receive_own_postings")?,
        "delivery_mode": text("delivery_mode")?,
        "delivery_status": text("delivery_status")?,
    }))
}

async fn exported_memberships(db: &Database, id: &str) -> Result<Vec<Value>> {
    let rows = sqlx::query("SELECT m.id, m.list_id, m.role, m.subscription_mode, m.display_name, m.moderation_action, m.bounce_score, m.created_at, m.preferences_id, a.email FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.user_id=$1 OR a.user_id=$1 ORDER BY m.list_id, m.role, a.email")
            .bind(id)
            .fetch_all(db.pool())
            .await
            .map_err(db_error)?;
    let mut memberships = Vec::new();
    for row in rows {
        let preferences_id: String = row.try_get("preferences_id").map_err(db_error)?;
        memberships.push(json!({
                "id": row.try_get::<String, _>("id").map_err(db_error)?,
                "list_id": row.try_get::<String, _>("list_id").map_err(db_error)?,
                "role": row.try_get::<String, _>("role").map_err(db_error)?,
                "subscription_mode": row.try_get::<String, _>("subscription_mode").map_err(db_error)?,
                "display_name": row.try_get::<String, _>("display_name").map_err(db_error)?,
                "moderation_action": row.try_get::<Option<String>, _>("moderation_action").map_err(db_error)?,
                "bounce_score": row.try_get::<f64, _>("bounce_score").map_err(db_error)?,
                "created_at": row.try_get::<String, _>("created_at").map_err(db_error)?,
                "email": row.try_get::<String, _>("email").map_err(db_error)?,
                "preferences": preferences_row(db, &preferences_id).await?,
            }));
    }
    Ok(memberships)
}

async fn exported_tokens(db: &Database, id: &str) -> Result<Vec<Value>> {
    let rows = sqlx::query("SELECT id, name, scopes, list_id, domain_id, expires_at, last_used_at, revoked_at, created_at FROM api_tokens WHERE user_id=$1 ORDER BY created_at")
            .bind(id)
            .fetch_all(db.pool())
            .await
            .map_err(db_error)?;
    let tokens = rows
        .iter()
        .map(|row| {
            Ok(json!({
                "id": row.try_get::<String, _>("id").map_err(db_error)?,
                "name": row.try_get::<String, _>("name").map_err(db_error)?,
                "scopes": row.try_get::<String, _>("scopes").map_err(db_error)?,
                "list_id": row.try_get::<Option<String>, _>("list_id").map_err(db_error)?,
                "domain_id": row.try_get::<Option<String>, _>("domain_id").map_err(db_error)?,
                "expires_at": row.try_get::<Option<String>, _>("expires_at").map_err(db_error)?,
                "last_used_at": row.try_get::<Option<String>, _>("last_used_at").map_err(db_error)?,
                "revoked_at": row.try_get::<Option<String>, _>("revoked_at").map_err(db_error)?,
                "created_at": row.try_get::<String, _>("created_at").map_err(db_error)?,
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(tokens)
}

async fn exported_audit(db: &Database, id: &str) -> Result<Vec<Value>> {
    let rows = sqlx::query("SELECT at, action, target_type, target_id, diff FROM audit_log WHERE actor_user_id=$1 OR (target_type='user' AND target_id=$1) OR (target_type='member' AND target_id IN (SELECT m.id FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.user_id=$1 OR a.user_id=$1)) OR (target_type='address' AND target_id IN (SELECT a.email FROM addresses a WHERE a.user_id=$1)) ORDER BY at DESC, id DESC LIMIT $2")
            .bind(id)
            .bind(EXPORTED_AUDIT_EVENTS)
            .fetch_all(db.pool())
            .await
            .map_err(db_error)?;
    let audit = rows
            .iter()
            .map(|row| {
                Ok(json!({
                    "at": row.try_get::<String, _>("at").map_err(db_error)?,
                    "action": row.try_get::<String, _>("action").map_err(db_error)?,
                    "target_type": row.try_get::<String, _>("target_type").map_err(db_error)?,
                    "target_id": row.try_get::<String, _>("target_id").map_err(db_error)?,
                    "diff": serde_json::from_str::<Value>(&row.try_get::<String, _>("diff").map_err(db_error)?).unwrap_or(Value::Null),
                }))
            })
            .collect::<Result<Vec<_>>>()?;
    Ok(audit)
}

impl Database {
    /// Everything stored about `user`, as JSON: the account, its
    /// preferences, addresses, memberships with their preferences, API
    /// token metadata, domain ownerships, browser session count, and the
    /// audit events it appears in (bounded). Secrets are never included.
    /// # Errors
    /// `NotFound` for an unknown account; database failures.
    pub async fn export_user(&self, user: UserId) -> Result<Value> {
        let account = self.users().get(user).await?;
        let id = user.to_string();
        let mut addresses = Vec::new();
        for address in self.addresses().by_user(user).await? {
            addresses.push(json!({
                "email": address.email,
                "original_email": address.original_email,
                "display_name": address.display_name,
                "verified_on": address.verified_on,
                "registered_on": address.registered_on,
                "preferences": preferences_of(self, "addresses", &address.id.to_string()).await?,
            }));
        }
        let memberships = exported_memberships(self, &id).await?;
        let tokens = exported_tokens(self, &id).await?;
        let domains: Vec<String> = sqlx::query_scalar("SELECT d.mail_host FROM domains d JOIN domain_owners o ON o.domain_id=d.id WHERE o.user_id=$1 ORDER BY d.mail_host")
            .bind(&id)
            .fetch_all(self.pool())
            .await
            .map_err(db_error)?;
        let sessions: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM web_sessions WHERE user_id=$1")
                .bind(&id)
                .fetch_one(self.pool())
                .await
                .map_err(db_error)?;
        let audit = exported_audit(self, &id).await?;
        Ok(json!({
            "format": "listmngr-account-export/1",
            "exported_at": chrono::Utc::now(),
            "account": {
                "id": account.id,
                "display_name": account.display_name,
                "is_server_owner": account.is_server_owner,
                "locale": account.locale,
                "timezone": account.timezone,
                "created_at": account.created_at,
                "preferences": preferences_of(self, "users", &id).await?,
            },
            "addresses": addresses,
            "memberships": memberships,
            "api_tokens": tokens,
            "domains_owned": domains,
            "browser_sessions": sessions,
            "audit_events": audit,
        }))
    }

    /// The signed-in reader's own export, under live session authority.
    /// # Errors
    /// A stale session; database failures.
    pub async fn browser_export_own(&self, session: &WebSession) -> Result<Value> {
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)?;
        self.export_user(user).await
    }

    /// Another account's export, for a server owner.
    /// # Errors
    /// Authority, `NotFound`, database failures.
    pub async fn browser_export_user(&self, session: &WebSession, user: UserId) -> Result<Value> {
        let mut tx = self.browser_write_tx().await?;
        Self::server_owner_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)?;
        self.export_user(user).await
    }

    /// Erases an account for an administrator or the command line: what
    /// self-service deletion removes, in one transaction with a `user.delete`
    /// event naming `context`. The last server owner with a verified
    /// address cannot be erased.
    /// # Errors
    /// `NotFound`, `Validation` for the last owner, database or audit
    /// failures.
    pub async fn erase_user(&self, user: UserId, context: &AuditContext) -> Result<Deleted> {
        let mut tx = self.write_tx().await?;
        let deleted = erase_tx(&mut tx, user, context, "administrator").await?;
        tx.commit().await.map_err(db_error)?;
        Ok(deleted)
    }

    /// Erases an account from the browser, for a server owner.
    /// # Errors
    /// Authority, `NotFound`, `Validation` for the last owner, database or
    /// audit failures.
    pub async fn browser_erase_user(&self, session: &WebSession, user: UserId) -> Result<Deleted> {
        let mut tx = self.browser_write_tx().await?;
        let actor = Self::server_owner_tx(&mut tx, session).await?;
        let deleted = erase_tx(
            &mut tx,
            user,
            &AuditContext::new(Some(actor), None, None),
            "administrator",
        )
        .await?;
        if actor != user {
            Self::browser_user_tx(&mut tx, session).await?;
        }
        tx.commit().await.map_err(db_error)?;
        Ok(deleted)
    }
}

async fn erase_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    user: UserId,
    context: &AuditContext,
    by: &str,
) -> Result<Deleted> {
    let owner: Option<i64> = sqlx::query_scalar("SELECT is_server_owner FROM users WHERE id=$1")
        .bind(user.to_string())
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_error)?;
    let Some(owner) = owner else {
        return Err(Error::NotFound(user.to_string()));
    };
    if owner == 1 {
        let others: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users u WHERE u.is_server_owner=1 AND u.id<>$1 AND EXISTS (SELECT 1 FROM addresses a WHERE a.user_id=u.id AND a.verified_on IS NOT NULL)")
            .bind(user.to_string()).fetch_one(&mut **tx).await.map_err(db_error)?;
        if others == 0 {
            return Err(Error::Validation(
                "the last server owner cannot be erased".into(),
            ));
        }
    }
    let deleted = remove_everything(tx, user).await?;
    Database::record_tx_with_context(
        tx,
        context,
        "user.delete",
        "user",
        &user.to_string(),
        json!({
            "by": by,
            "memberships": deleted.memberships,
            "addresses": deleted.addresses,
            "tokens": deleted.tokens,
        }),
    )
    .await?;
    Ok(deleted)
}
