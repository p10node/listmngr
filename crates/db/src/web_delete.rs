//! Self-service account deletion.
//!
//! The reader proves the password again, then one transaction removes what
//! is theirs — memberships, addresses and their pending tokens, API tokens,
//! credential, sessions, domain ownerships, the user row and its preferences —
//! and audits it. Moderation history keeps its rows but loses the link to the
//! deleted user; the audit log keeps the actor id as text. The last server
//! owner cannot delete themselves.
use crate::web_sessions::WebSession;
use crate::{AuditContext, Database, db_error};
use argon2::{PasswordHash, PasswordVerifier};
use listmngr_core::{Error, Result, UserId};
use sqlx::Row;

/// What one deletion removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Deleted {
    /// Memberships ended.
    pub memberships: u64,
    /// Address rows removed.
    pub addresses: u64,
    /// API tokens removed.
    pub tokens: u64,
}

impl Database {
    /// Delete the signed-in reader's account after re-checking `password`.
    /// # Errors
    /// A wrong password (authentication); the last server owner (validation);
    /// a stale session or failed CSRF binding; or a database or audit failure —
    /// nothing is written on error.
    pub async fn browser_delete_account(
        &self,
        session: &WebSession,
        password: &str,
    ) -> Result<Deleted> {
        let user = session.user_id.ok_or(Error::Authentication)?;
        if password.len() > 1024 {
            return Err(Error::Authentication);
        }
        let hash: Option<String> =
            sqlx::query_scalar("SELECT password_hash FROM user_credentials WHERE user_id=$1")
                .bind(user.to_string())
                .fetch_optional(self.pool())
                .await
                .map_err(db_error)?;
        let hash = hash.ok_or(Error::Authentication)?;
        let parsed = PasswordHash::new(&hash).map_err(|_| Error::Authentication)?;
        self.users()
            .password_hasher()?
            .verify_password(password.as_bytes(), &parsed)
            .map_err(|_| Error::Authentication)?;
        // Argon2 has finished; reserve the writer and rebind authority.
        let mut tx = self.browser_write_tx().await?;
        if Self::browser_user_tx(&mut tx, session).await? != user {
            return Err(Error::Authentication);
        }
        let owner: i64 = sqlx::query_scalar("SELECT is_server_owner FROM users WHERE id=$1")
            .bind(user.to_string())
            .fetch_one(&mut *tx)
            .await
            .map_err(db_error)?;
        if owner == 1 {
            let others: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users u WHERE u.is_server_owner=1 AND u.id<>$1 AND EXISTS (SELECT 1 FROM addresses a WHERE a.user_id=u.id AND a.verified_on IS NOT NULL)")
                .bind(user.to_string()).fetch_one(&mut *tx).await.map_err(db_error)?;
            if others == 0 {
                return Err(Error::Validation(
                    "the last server owner cannot delete their account".into(),
                ));
            }
        }
        let deleted = remove_everything(&mut tx, user).await?;
        Self::record_tx_with_context(
            &mut tx,
            &AuditContext::new(Some(user), None, None),
            "user.delete",
            "user",
            &user.to_string(),
            serde_json::json!({
                "by": "self",
                "memberships": deleted.memberships,
                "addresses": deleted.addresses,
                "tokens": deleted.tokens,
            }),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(deleted)
    }
}

pub(crate) async fn remove_everything(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    user: UserId,
) -> Result<Deleted> {
    let id = user.to_string();
    let memberships = remove_memberships(tx, user).await?;
    let addresses = remove_addresses(tx, &id).await?;
    let tokens = sqlx::query("DELETE FROM api_tokens WHERE user_id=$1")
        .bind(&id)
        .execute(&mut **tx)
        .await
        .map_err(db_error)?
        .rows_affected();
    let deleted = Deleted {
        memberships,
        addresses,
        tokens,
    };
    for sql in [
        "DELETE FROM user_credentials WHERE user_id=$1",
        "DELETE FROM web_sessions WHERE user_id=$1",
        "DELETE FROM domain_owners WHERE user_id=$1",
        "UPDATE held_messages SET moderator_id=NULL WHERE moderator_id=$1",
        "UPDATE moderation_log SET moderator_id=NULL WHERE moderator_id=$1",
    ] {
        sqlx::query(sql)
            .bind(&id)
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
    }
    let preferences: Option<String> =
        sqlx::query_scalar("SELECT preferences_id FROM users WHERE id=$1")
            .bind(&id)
            .fetch_one(&mut **tx)
            .await
            .map_err(db_error)?;
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(&id)
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    if let Some(preferences) = preferences {
        sqlx::query("DELETE FROM preferences WHERE id=$1")
            .bind(preferences)
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
    }
    Ok(deleted)
}

/// Memberships held through the user or any of their addresses, each with
/// its own audit event so a list owner can see why a member vanished.
async fn remove_memberships(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    user: UserId,
) -> Result<u64> {
    let context = AuditContext::new(Some(user), None, None);
    let memberships = sqlx::query(
        "SELECT m.id, m.list_id, m.role, m.preferences_id, a.email FROM members m
         JOIN addresses a ON a.id=m.address_id
         WHERE m.user_id=$1 OR a.user_id=$1",
    )
    .bind(user.to_string())
    .fetch_all(&mut **tx)
    .await
    .map_err(db_error)?;
    let mut removed = 0;
    for row in memberships {
        let member: String = row.try_get("id").map_err(db_error)?;
        let list: String = row.try_get("list_id").map_err(db_error)?;
        let role: String = row.try_get("role").map_err(db_error)?;
        let preferences: String = row.try_get("preferences_id").map_err(db_error)?;
        let email: String = row.try_get("email").map_err(db_error)?;
        sqlx::query("DELETE FROM members WHERE id=$1")
            .bind(&member)
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
        sqlx::query("DELETE FROM preferences WHERE id=$1")
            .bind(&preferences)
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
        Database::record_tx_with_context(
            tx,
            &context,
            "member.delete",
            "member",
            &member,
            serde_json::json!({"list_id": list, "role": role, "email": email, "reason": "account deleted"}),
        )
        .await?;
        removed += 1;
    }
    Ok(removed)
}

/// The user's address rows and their preferences; pending verification and
/// reset tokens cascade with the rows.
async fn remove_addresses(tx: &mut sqlx::Transaction<'_, sqlx::Any>, id: &str) -> Result<u64> {
    let addresses: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT id, preferences_id FROM addresses WHERE user_id=$1")
            .bind(id)
            .fetch_all(&mut **tx)
            .await
            .map_err(db_error)?;
    sqlx::query("UPDATE users SET preferred_address_id=NULL WHERE id=$1")
        .bind(id)
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    let mut removed = 0;
    for (address, preferences) in addresses {
        sqlx::query("DELETE FROM addresses WHERE id=$1")
            .bind(&address)
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
        if let Some(preferences) = preferences {
            sqlx::query("DELETE FROM preferences WHERE id=$1")
                .bind(preferences)
                .execute(&mut **tx)
                .await
                .map_err(db_error)?;
        }
        removed += 1;
    }
    Ok(removed)
}
