//! Per-list `header_matches` rows: the list owner's own header rules.
//!
//! The posting chain evaluates them in position order via the `header-match`
//! detour. Every edit rewrites the set inside one locked transaction so
//! position numbering never gaps and concurrent edits never lose each other.

use listmngr_core::{Error, ListId, Result};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::{AuditContext, Database, db_error, lock_list_for_patch};

/// Chains a header rule may jump to. Terminal chains only: a rule that jumped
/// into the moderation chain would carry no recorded action.
pub const TARGET_CHAINS: [&str; 4] = ["accept", "hold", "reject", "discard"];

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

/// A partial edit of one row, as Mailman's `PATCH`/`PUT` on a header match.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeaderMatchPatch {
    pub header: Option<String>,
    pub pattern: Option<String>,
    pub chain: FieldEdit,
    pub tag: FieldEdit,
    /// Move the row to this position, shifting the rows in between.
    pub position: Option<usize>,
}

/// One optional field of a patch: left alone, cleared, or set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum FieldEdit {
    #[default]
    Keep,
    Clear,
    Set(String),
}

impl FieldEdit {
    fn apply(self, field: &mut Option<String>) {
        match self {
            Self::Keep => {}
            Self::Clear => *field = None,
            Self::Set(value) => *field = Some(value),
        }
    }
}

fn validate(row: &HeaderMatchRow) -> Result<()> {
    let invalid = |what: &str| Error::Validation(format!("header match {what}"));
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

/// Validate every row and refuse the same header and pattern twice, as
/// Mailman's `IHeaderMatchList.append` does ("Pattern already exists").
fn validate_set(rows: &[HeaderMatchRow]) -> Result<()> {
    for (index, row) in rows.iter().enumerate() {
        validate(row)?;
        let duplicate = rows[..index].iter().any(|earlier| {
            earlier
                .header
                .trim()
                .eq_ignore_ascii_case(row.header.trim())
                && earlier.pattern == row.pattern
        });
        if duplicate {
            return Err(Error::Validation("header match already exists".into()));
        }
    }
    Ok(())
}

fn out_of_range(position: usize) -> Error {
    Error::NotFound(format!("header match {position}"))
}

#[derive(Debug, Clone, Copy)]
pub struct HeaderMatchRepo<'a> {
    pub(crate) db: &'a Database,
}

impl HeaderMatchRepo<'_> {
    /// Rows in position order.
    /// # Errors
    /// Returns not-found for the list or a database error.
    pub async fn list(&self, list: &ListId) -> Result<Vec<HeaderMatchRow>> {
        self.db.lists().get(list).await?;
        let rows = sqlx::query(
            "SELECT header, pattern, chain, tag FROM header_matches WHERE list_id=$1 ORDER BY position",
        )
        .bind(list.as_str())
        .fetch_all(self.db.pool())
        .await
        .map_err(db_error)?;
        rows.iter().map(row_from).collect()
    }

    /// The row at `position`.
    /// # Errors
    /// Returns not-found when there is no such row, or a database error.
    pub async fn get(&self, list: &ListId, position: usize) -> Result<HeaderMatchRow> {
        self.list(list)
            .await?
            .into_iter()
            .nth(position)
            .ok_or_else(|| out_of_range(position))
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
        self.edit(list, context, "replace", None, |set| {
            *set = rows.to_vec();
            Ok(())
        })
        .await
        .map(|_| ())
    }

    /// Append one row at the end; returns its position.
    /// # Errors
    /// Returns a validation error for the row or a duplicate header and
    /// pattern, not-found for the list, or a database/audit failure.
    pub async fn append(
        &self,
        list: &ListId,
        row: HeaderMatchRow,
        context: &AuditContext,
    ) -> Result<usize> {
        let rows = self
            .edit(list, context, "append", None, |set| {
                set.push(row);
                Ok(())
            })
            .await?;
        Ok(rows.len() - 1)
    }

    /// Apply a patch to the row at `position`, moving it when the patch says
    /// so; returns the row and the position it ended at.
    /// # Errors
    /// Returns not-found when there is no such row, a validation error for
    /// the patched row, a duplicate or a position past the end, or a
    /// database/audit failure.
    pub async fn update(
        &self,
        list: &ListId,
        position: usize,
        patch: HeaderMatchPatch,
        context: &AuditContext,
    ) -> Result<(usize, HeaderMatchRow)> {
        let target = patch.position.unwrap_or(position);
        let rows = self
            .edit(list, context, "update", Some(position), |set| {
                let mut row = set
                    .get(position)
                    .cloned()
                    .ok_or_else(|| out_of_range(position))?;
                if let Some(header) = patch.header {
                    row.header = header;
                }
                if let Some(pattern) = patch.pattern {
                    row.pattern = pattern;
                }
                patch.chain.apply(&mut row.chain);
                patch.tag.apply(&mut row.tag);
                if target >= set.len() {
                    return Err(Error::Validation(format!(
                        "header match position must be below {}",
                        set.len()
                    )));
                }
                set.remove(position);
                set.insert(target, row);
                Ok(())
            })
            .await?;
        Ok((target, rows[target].clone()))
    }

    /// Remove the row at `position`; the rows after it move up.
    /// # Errors
    /// Returns not-found when there is no such row, or a database/audit
    /// failure.
    pub async fn remove(
        &self,
        list: &ListId,
        position: usize,
        context: &AuditContext,
    ) -> Result<()> {
        self.edit(list, context, "remove", Some(position), |set| {
            if position >= set.len() {
                return Err(out_of_range(position));
            }
            set.remove(position);
            Ok(())
        })
        .await
        .map(|_| ())
    }

    /// Remove every row.
    /// # Errors
    /// Returns not-found for the list or a database/audit failure.
    pub async fn clear(&self, list: &ListId, context: &AuditContext) -> Result<()> {
        self.edit(list, context, "clear", None, |set| {
            set.clear();
            Ok(())
        })
        .await
        .map(|_| ())
    }

    /// Lock the list, read the current set, let `edit` change it, validate
    /// the result and write it back with one `list.header_matches` audit row.
    async fn edit(
        &self,
        list: &ListId,
        context: &AuditContext,
        change: &str,
        position: Option<usize>,
        edit: impl FnOnce(&mut Vec<HeaderMatchRow>) -> Result<()>,
    ) -> Result<Vec<HeaderMatchRow>> {
        let mut tx = self.db.write_tx().await?;
        let rows = Self::edit_tx(&mut tx, list, context, change, position, edit).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(rows)
    }

    /// Rewrite the set inside the caller's transaction: the current rows,
    /// the edit, validation, the renumbered rows and one audit event.
    pub(crate) async fn edit_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Any>,
        list: &ListId,
        context: &AuditContext,
        change: &str,
        position: Option<usize>,
        edit: impl FnOnce(&mut Vec<HeaderMatchRow>) -> Result<()>,
    ) -> Result<Vec<HeaderMatchRow>> {
        lock_list_for_patch(tx, list).await?;
        let current = sqlx::query(
            "SELECT header, pattern, chain, tag FROM header_matches WHERE list_id=$1 ORDER BY position",
        )
        .bind(list.as_str())
        .fetch_all(&mut **tx)
        .await
        .map_err(db_error)?;
        let mut rows = current.iter().map(row_from).collect::<Result<Vec<_>>>()?;
        edit(&mut rows)?;
        validate_set(&rows)?;
        sqlx::query("DELETE FROM header_matches WHERE list_id=$1")
            .bind(list.as_str())
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
        for (position, row) in rows.iter().enumerate() {
            sqlx::query(
                "INSERT INTO header_matches(id, list_id, position, header, pattern, action, tag, chain) VALUES($1,$2,$3,$4,$5,NULL,$6,$7)",
            )
            .bind(Uuid::now_v7().to_string())
            .bind(list.as_str())
            .bind(i64::try_from(position).map_err(|_| Error::Validation("too many header matches".into()))?)
            .bind(row.header.trim())
            .bind(&row.pattern)
            .bind(&row.tag)
            .bind(&row.chain)
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
        }
        Database::record_tx_with_context(
            tx,
            context,
            "list.header_matches",
            "list",
            list.as_str(),
            serde_json::json!({ "change": change, "position": position, "count": rows.len() }),
        )
        .await?;
        Ok(rows)
    }
}

fn row_from(row: &sqlx::any::AnyRow) -> Result<HeaderMatchRow> {
    Ok(HeaderMatchRow {
        header: row.try_get("header").map_err(db_error)?,
        pattern: row.try_get("pattern").map_err(db_error)?,
        chain: row.try_get("chain").map_err(db_error)?,
        tag: row.try_get("tag").map_err(db_error)?,
    })
}
