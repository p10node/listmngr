//! Sender bans: per list, and site-wide rows that every list matches.
//!
//! The `bans` unique index ignores NULL list ids, so site-wide duplicates
//! are refused by the repository under the global reservation.

use listmngr_core::{Address, ListId, Result};
use uuid::Uuid;

use crate::{AuditContext, Database, db_error, lock_list_for_patch};

pub(crate) async fn is_banned<'e, E>(executor: E, list: &ListId, sender: &str) -> Result<bool>
where
    E: sqlx::Executor<'e, Database = sqlx::Any>,
{
    let patterns: Vec<String> =
        sqlx::query_scalar("SELECT email_or_regex FROM bans WHERE list_id=$1 OR list_id IS NULL")
            .bind(list.as_str())
            .fetch_all(executor)
            .await
            .map_err(db_error)?;
    let identity = |email: &str| {
        Address::new(email, String::new())
            .map_or_else(|_| email.to_ascii_lowercase(), |address| address.email)
    };
    let sender_identity = identity(sender);
    Ok(patterns.iter().any(|pattern| {
        if pattern.starts_with('^') {
            regex::Regex::new(pattern).is_ok_and(|expression| expression.is_match(sender))
        } else {
            identity(pattern) == sender_identity
        }
    }))
}

fn normalize(value: &str) -> Result<String> {
    if value.is_empty() || value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(listmngr_core::Error::Validation("invalid ban".into()));
    }
    if !value.starts_with('^') {
        return Ok(Address::new(value, String::new())?.email);
    }
    if value.len() > 1024 {
        return Err(listmngr_core::Error::Validation(
            "ban regex exceeds 1024 bytes".into(),
        ));
    }
    regex::RegexBuilder::new(value)
        .size_limit(1 << 20)
        .nest_limit(64)
        .build()
        .map_err(|_| listmngr_core::Error::Validation("invalid or oversized ban regex".into()))?;
    Ok(value.to_owned())
}

#[derive(Debug, Clone, Copy)]
pub struct BanRepo<'a> {
    db: &'a Database,
}

impl Database {
    #[must_use]
    pub const fn bans(&self) -> BanRepo<'_> {
        BanRepo { db: self }
    }
}

impl BanRepo<'_> {
    /// Retrieves one stored list-local ban, not an address's effective ban status.
    ///
    /// # Errors
    /// Returns validation, missing-list/ban or database errors.
    pub async fn get(&self, id: &ListId, value: &str) -> Result<String> {
        let value = normalize(value)?;
        self.db.lists().get(id).await?;
        sqlx::query_scalar("SELECT email_or_regex FROM bans WHERE list_id=$1 AND email_or_regex=$2")
            .bind(id.as_str())
            .bind(&value)
            .fetch_optional(self.db.pool())
            .await
            .map_err(db_error)?
            .ok_or(listmngr_core::Error::NotFound(value))
    }

    /// Matches list and global bans using canonical exact identity or original-case regex input.
    ///
    /// # Errors
    /// Returns database errors.
    pub async fn is_banned(&self, id: &ListId, sender: &str) -> Result<bool> {
        is_banned(self.db.pool(), id, sender).await
    }

    /// Lists this list's bans in stable lexical order.
    ///
    /// # Errors
    /// Returns missing-list, invalid pagination or database errors.
    pub async fn list(&self, id: &ListId, limit: i64, offset: i64) -> Result<Vec<String>> {
        if limit < 0 || offset < 0 {
            return Err(listmngr_core::Error::Validation(
                "negative pagination".into(),
            ));
        }
        self.db.lists().get(id).await?;
        sqlx::query_scalar("SELECT email_or_regex FROM bans WHERE list_id=$1 ORDER BY email_or_regex LIMIT $2 OFFSET $3")
            .bind(id.as_str()).bind(limit).bind(offset)
            .fetch_all(&self.db.pool).await.map_err(db_error)
    }

    /// Counts only this list's bans, excluding global bans.
    ///
    /// # Errors
    /// Returns missing-list or database errors.
    pub async fn count(&self, id: &ListId) -> Result<i64> {
        self.db.lists().get(id).await?;
        sqlx::query_scalar("SELECT COUNT(*) FROM bans WHERE list_id=$1")
            .bind(id.as_str())
            .fetch_one(&self.db.pool)
            .await
            .map_err(db_error)
    }

    /// Deletes a canonical ban and records its edge attribution atomically.
    ///
    /// # Errors
    /// Returns validation, missing-list/ban, database or audit errors.
    pub async fn delete(&self, id: &ListId, value: &str, context: &AuditContext) -> Result<()> {
        let value = normalize(value)?;
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        crate::workflows::lock(&mut tx).await?;
        lock_list_for_patch(&mut tx, id).await?;
        let changed = sqlx::query("DELETE FROM bans WHERE list_id=$1 AND email_or_regex=$2")
            .bind(id.as_str())
            .bind(&value)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?
            .rows_affected();
        if changed == 0 {
            return Err(listmngr_core::Error::NotFound(value));
        }
        Database::record_tx_with_context(
            &mut tx,
            context,
            "ban.delete",
            "list",
            id.as_str(),
            serde_json::json!({"email_or_regex": value}),
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }

    /// One stored site-wide ban by its canonical value.
    ///
    /// # Errors
    /// Returns validation, missing-ban or database errors.
    pub async fn site_get(&self, value: &str) -> Result<String> {
        let value = normalize(value)?;
        sqlx::query_scalar(
            "SELECT email_or_regex FROM bans WHERE list_id IS NULL AND email_or_regex=$1",
        )
        .bind(&value)
        .fetch_optional(self.db.pool())
        .await
        .map_err(db_error)?
        .ok_or(listmngr_core::Error::NotFound(value))
    }

    /// Site-wide bans in stable lexical order.
    ///
    /// # Errors
    /// Returns invalid pagination or database errors.
    pub async fn site_list(&self, limit: i64, offset: i64) -> Result<Vec<String>> {
        if limit < 0 || offset < 0 {
            return Err(listmngr_core::Error::Validation(
                "negative pagination".into(),
            ));
        }
        sqlx::query_scalar("SELECT email_or_regex FROM bans WHERE list_id IS NULL ORDER BY email_or_regex LIMIT $1 OFFSET $2")
            .bind(limit).bind(offset)
            .fetch_all(&self.db.pool).await.map_err(db_error)
    }

    /// Counts the site-wide bans only.
    ///
    /// # Errors
    /// Returns database errors.
    pub async fn site_count(&self) -> Result<i64> {
        sqlx::query_scalar("SELECT COUNT(*) FROM bans WHERE list_id IS NULL")
            .fetch_one(&self.db.pool)
            .await
            .map_err(db_error)
    }

    /// Creates a site-wide ban under the global reservation, refusing a
    /// value already banned site-wide.
    ///
    /// # Errors
    /// Returns validation, conflict, database or audit errors.
    pub async fn site_create(&self, value: &str, context: &AuditContext) -> Result<String> {
        let value = normalize(value)?;
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        crate::workflows::lock(&mut tx).await?;
        let existing: Option<String> = sqlx::query_scalar(
            "SELECT email_or_regex FROM bans WHERE list_id IS NULL AND email_or_regex=$1",
        )
        .bind(&value)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?;
        if existing.is_some() {
            return Err(listmngr_core::Error::Conflict(value));
        }
        sqlx::query("INSERT INTO bans(id,list_id,email_or_regex) VALUES($1,NULL,$2)")
            .bind(Uuid::now_v7().to_string())
            .bind(&value)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        Database::record_tx_with_context(
            &mut tx,
            context,
            "ban.create",
            "site",
            "bans",
            serde_json::json!({"email_or_regex": value}),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(value)
    }

    /// Deletes a site-wide ban and records it atomically.
    ///
    /// # Errors
    /// Returns validation, missing-ban, database or audit errors.
    pub async fn site_delete(&self, value: &str, context: &AuditContext) -> Result<()> {
        let value = normalize(value)?;
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        crate::workflows::lock(&mut tx).await?;
        let changed = sqlx::query("DELETE FROM bans WHERE list_id IS NULL AND email_or_regex=$1")
            .bind(&value)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?
            .rows_affected();
        if changed == 0 {
            return Err(listmngr_core::Error::NotFound(value));
        }
        Database::record_tx_with_context(
            &mut tx,
            context,
            "ban.delete",
            "site",
            "bans",
            serde_json::json!({"email_or_regex": value}),
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }

    /// Creates a canonical mailbox ban attributed to the immutable edge context.
    ///
    /// # Errors
    /// Returns validation, missing-list, conflict, database or audit errors.
    pub async fn create(&self, id: &ListId, value: &str, context: &AuditContext) -> Result<String> {
        let value = normalize(value)?;
        let mut tx = self.db.pool.begin().await.map_err(db_error)?;
        crate::workflows::lock(&mut tx).await?;
        lock_list_for_patch(&mut tx, id).await?;
        sqlx::query("INSERT INTO bans(id,list_id,email_or_regex) VALUES($1,$2,$3)")
            .bind(Uuid::now_v7().to_string())
            .bind(id.as_str())
            .bind(&value)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        Database::record_tx_with_context(
            &mut tx,
            context,
            "ban.create",
            "list",
            id.as_str(),
            serde_json::json!({"email_or_regex": value}),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(value)
    }
}
