//! Per-list `header_matches` rows: the list owner's own header rules.
//!
//! The posting chain evaluates them in position order via the `header-match`
//! detour. Rows are replaced as a whole so position numbering never gaps.

use listmngr_core::{ListId, Result};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::{AuditContext, Database, db_error, lock_list_for_patch};

/// Chains a header rule may jump to. Terminal chains only: a rule that jumped
/// into the moderation chain would carry no recorded action.
const TARGET_CHAINS: [&str; 4] = ["accept", "hold", "reject", "discard"];

/// Longest header name, pattern, chain or tag accepted from an owner.
const MAX_FIELD_BYTES: usize = 1024;

/// One header rule, in the shape the REST layer and the pipeline both use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct HeaderMatchRow {
    pub header: String,
    pub pattern: String,
    /// Terminal chain to jump to; `None` means the site default (`hold`).
    #[serde(default)]
    pub chain: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
}

fn validate(row: &HeaderMatchRow) -> Result<()> {
    let invalid = |what: &str| listmngr_core::Error::Validation(format!("header match {what}"));
    let header = row.header.trim();
    if header.is_empty()
        || header.len() > MAX_FIELD_BYTES
        || header.contains([':', '\r', '\n'])
        || !header.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(invalid("header must be a single ASCII field name"));
    }
    if row.pattern.is_empty() || row.pattern.len() > MAX_FIELD_BYTES {
        return Err(invalid("pattern must be 1..=1024 bytes"));
    }
    // Validate exactly the way the pipeline will compile it.
    listmngr_pipeline::compile_header_pattern(&row.pattern)
        .map_err(|_| invalid("pattern is not a valid regular expression"))?;
    if let Some(chain) = &row.chain
        && !TARGET_CHAINS.contains(&chain.as_str())
    {
        return Err(invalid(
            "chain must be one of accept, hold, reject, discard",
        ));
    }
    if let Some(tag) = &row.tag
        && (tag.trim().is_empty() || tag.len() > MAX_FIELD_BYTES || tag.contains(['\r', '\n']))
    {
        return Err(invalid("tag must be a single non-empty line"));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub struct HeaderMatchRepo<'a> {
    pub(crate) db: &'a Database,
}

impl HeaderMatchRepo<'_> {
    /// Rows in position order.
    /// # Errors
    /// Returns a database error.
    pub async fn list(&self, list: &ListId) -> Result<Vec<HeaderMatchRow>> {
        let rows = sqlx::query(
            "SELECT header, pattern, chain, tag FROM header_matches WHERE list_id=$1 ORDER BY position",
        )
        .bind(list.as_str())
        .fetch_all(self.db.pool())
        .await
        .map_err(db_error)?;
        rows.iter()
            .map(|row| {
                Ok(HeaderMatchRow {
                    header: row.try_get("header").map_err(db_error)?,
                    pattern: row.try_get("pattern").map_err(db_error)?,
                    chain: row.try_get("chain").map_err(db_error)?,
                    tag: row.try_get("tag").map_err(db_error)?,
                })
            })
            .collect()
    }

    /// Replace every row for the list in one audited transaction. Every row is
    /// validated before anything is written, so a bad row leaves the previous
    /// set untouched.
    /// # Errors
    /// Returns validation errors for any row, not-found for the list, or a
    /// database/audit failure.
    pub async fn replace(&self, list: &ListId, rows: &[HeaderMatchRow]) -> Result<()> {
        self.replace_with_context(list, rows, &AuditContext::system())
            .await
    }

    /// See [`Self::replace`].
    /// # Errors
    /// Returns validation errors for any row, not-found for the list, or a
    /// database/audit failure.
    pub async fn replace_with_context(
        &self,
        list: &ListId,
        rows: &[HeaderMatchRow],
        context: &AuditContext,
    ) -> Result<()> {
        for row in rows {
            validate(row)?;
        }
        let mut tx = self.db.pool().begin().await.map_err(db_error)?;
        lock_list_for_patch(&mut tx, list).await?;
        sqlx::query("DELETE FROM header_matches WHERE list_id=$1")
            .bind(list.as_str())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        for (position, row) in rows.iter().enumerate() {
            sqlx::query(
                "INSERT INTO header_matches(id, list_id, position, header, pattern, action, tag, chain) VALUES($1,$2,$3,$4,$5,NULL,$6,$7)",
            )
            .bind(Uuid::now_v7().to_string())
            .bind(list.as_str())
            .bind(i64::try_from(position).map_err(|_| listmngr_core::Error::Validation("too many header matches".into()))?)
            .bind(row.header.trim())
            .bind(&row.pattern)
            .bind(&row.tag)
            .bind(&row.chain)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        }
        Database::record_tx_with_context(
            &mut tx,
            context,
            "list.header_matches",
            "list",
            list.as_str(),
            serde_json::json!({ "count": rows.len() }),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(())
    }
}
