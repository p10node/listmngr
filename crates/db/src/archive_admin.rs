//! The owner's archive administration: hiding, deleting and the list's
//! categories.
//!
//! Hiding sets `archive_messages.hidden_at`, which every reading path
//! filters out, so a hidden post leaves the pages, the feeds, the search
//! index, the REST reads and the mbox export while its row, its bytes and
//! its place in the thread survive; the administration page is the only
//! surface that still names it, and only to put it back. Deleting removes
//! the rows outright, splicing a deleted post's replies onto its parent
//! and re-rooting the thread when the root goes. Every change and its
//! audit event commit in one transaction under the owner's live authority.
use super::ArchiveRepo;
use super::interact::normalize_label;
use crate::{Database, db_error, web_sessions::WebSession};
use listmngr_core::{Error, ListId, Result};
use sqlx::Row as _;

/// Whether an administrative change takes one post or a whole thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Message,
    Thread,
}

impl Scope {
    /// The form value a page submits.
    /// # Errors
    /// Returns validation for anything else.
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "message" => Ok(Self::Message),
            "thread" => Ok(Self::Thread),
            _ => Err(Error::Validation("archive admin scope".into())),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Thread => "thread",
        }
    }

    /// The column the target names.
    const fn column(self) -> &'static str {
        match self {
            Self::Message => "hash",
            Self::Thread => "thread",
        }
    }
}

/// One hidden post, as the owner's page lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HiddenPost {
    pub hash: String,
    pub thread: String,
    pub subject: String,
    pub sender: String,
    pub hidden_at: i64,
}

/// One of the list's categories with the threads filed under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryRow {
    pub name: String,
    pub threads: i64,
}

/// What the owner's archive administration page shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Administration {
    pub categories: Vec<CategoryRow>,
    pub hidden: Vec<HiddenPost>,
}

/// A category as stored: the same shape as a tag, up to sixty characters.
/// # Errors
/// Returns validation for an empty or over-long result.
pub fn normalize_category(name: &str) -> Result<String> {
    normalize_label(name, 60, "archive category")
}

impl ArchiveRepo<'_> {
    /// The owner's view of the archive: the list's categories with their
    /// thread counts, and every hidden post.
    /// # Errors
    /// `Forbidden` for anyone but an owner, a stale session or database
    /// failures.
    pub async fn browser_administration(
        &self,
        session: &WebSession,
        list: &ListId,
    ) -> Result<Administration> {
        let mut tx = self.db.browser_write_tx().await?;
        Database::browser_owner_tx(&mut tx, session, list).await?;
        let category_rows = sqlx::query("SELECT c.name AS name, (SELECT COUNT(*) FROM archive_thread_categories t WHERE t.list_id=c.list_id AND t.category=c.name) AS threads FROM archive_categories c WHERE c.list_id=$1 ORDER BY c.name LIMIT 200")
            .bind(list.as_str())
            .fetch_all(&mut *tx)
            .await
            .map_err(db_error)?;
        let hidden_rows = sqlx::query("SELECT hash, thread, subject, sender_name, sender_email, hidden_at FROM archive_messages WHERE list_id=$1 AND hidden_at IS NOT NULL ORDER BY hidden_at DESC, hash LIMIT 200")
            .bind(list.as_str())
            .fetch_all(&mut *tx)
            .await
            .map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        let categories = category_rows
            .iter()
            .map(|row| {
                Ok(CategoryRow {
                    name: row.try_get("name").map_err(db_error)?,
                    threads: row.try_get("threads").map_err(db_error)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let hidden = hidden_rows
            .iter()
            .map(|row| {
                let name: String = row.try_get("sender_name").map_err(db_error)?;
                let email: String = row.try_get("sender_email").map_err(db_error)?;
                Ok(HiddenPost {
                    hash: row.try_get("hash").map_err(db_error)?,
                    thread: row.try_get("thread").map_err(db_error)?,
                    subject: row.try_get("subject").map_err(db_error)?,
                    sender: if name.is_empty() { email } else { name },
                    hidden_at: row.try_get("hidden_at").map_err(db_error)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Administration { categories, hidden })
    }

    /// Hide one post or a whole thread from every reading surface, or put
    /// it back; returns how many posts changed.
    /// # Errors
    /// `NotFound` for a target the list does not hold, `Forbidden` for
    /// anyone but an owner, a stale session or database failures.
    pub async fn browser_hide(
        &self,
        session: &WebSession,
        list: &ListId,
        scope: Scope,
        target: &str,
        hidden: bool,
        now_ms: i64,
    ) -> Result<u64> {
        if target.is_empty() || target.len() > 200 {
            return Err(Error::Validation("archive message hash bounds".into()));
        }
        let mut tx = self.db.browser_write_tx().await?;
        let user = Database::browser_owner_tx(&mut tx, session, list).await?;
        let sql = format!(
            "UPDATE archive_messages SET hidden_at=$1 WHERE list_id=$2 AND {}=$3",
            scope.column()
        );
        let posts = sqlx::query(&sql)
            .bind(hidden.then_some(now_ms))
            .bind(list.as_str())
            .bind(target)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?
            .rows_affected();
        if posts == 0 {
            return Err(Error::NotFound("archive message".into()));
        }
        Database::record_tx_with_context(
            &mut tx,
            &crate::AuditContext::new(Some(user), None, None),
            if hidden {
                "archive.hide"
            } else {
                "archive.unhide"
            },
            "list",
            list.as_str(),
            serde_json::json!({"scope": scope.as_str(), "target": target, "posts": posts}),
        )
        .await?;
        Database::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(posts)
    }

    /// Delete one post or a whole thread with everything attached to it;
    /// returns how many posts went.
    ///
    /// A deleted post's replies are spliced onto its parent, and when the
    /// thread's root goes its oldest surviving post becomes the new root,
    /// carrying the thread's tags, category, favourites and last-view
    /// marks with it.
    /// # Errors
    /// `NotFound` for a target the list does not hold, `Forbidden` for
    /// anyone but an owner, a stale session or database failures.
    pub async fn browser_delete(
        &self,
        session: &WebSession,
        list: &ListId,
        scope: Scope,
        target: &str,
    ) -> Result<u64> {
        if target.is_empty() || target.len() > 200 {
            return Err(Error::Validation("archive message hash bounds".into()));
        }
        let mut tx = self.db.browser_write_tx().await?;
        let user = Database::browser_owner_tx(&mut tx, session, list).await?;
        let posts = match scope {
            Scope::Message => delete_message(&mut tx, list, target).await?,
            Scope::Thread => delete_thread(&mut tx, list, target).await?,
        };
        if posts == 0 {
            return Err(Error::NotFound("archive message".into()));
        }
        Database::record_tx_with_context(
            &mut tx,
            &crate::AuditContext::new(Some(user), None, None),
            "archive.delete",
            "list",
            list.as_str(),
            serde_json::json!({"scope": scope.as_str(), "target": target, "posts": posts}),
        )
        .await?;
        Database::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(posts)
    }

    /// Add a category to the list, rename one (its threads follow), or
    /// remove one (its threads are unfiled).
    /// # Errors
    /// `Validation` for a name that normalises to nothing, a name the list
    /// already has, or a rename or removal of one it lacks; `Forbidden`
    /// for anyone but an owner; a stale session or database failures.
    pub async fn browser_categories(
        &self,
        session: &WebSession,
        list: &ListId,
        operation: CategoryChange<'_>,
    ) -> Result<()> {
        let mut tx = self.db.browser_write_tx().await?;
        let user = Database::browser_owner_tx(&mut tx, session, list).await?;
        let event = match operation {
            CategoryChange::Add(name) => {
                let name = normalize_category(name)?;
                if known(&mut tx, list, &name).await? {
                    return Err(Error::Validation("archive category exists".into()));
                }
                sqlx::query("INSERT INTO archive_categories(list_id, name) VALUES($1,$2)")
                    .bind(list.as_str())
                    .bind(&name)
                    .execute(&mut *tx)
                    .await
                    .map_err(db_error)?;
                serde_json::json!({"op": "add", "name": name})
            }
            CategoryChange::Rename { from, to } => {
                let from = normalize_category(from)?;
                let to = normalize_category(to)?;
                if !known(&mut tx, list, &from).await? || known(&mut tx, list, &to).await? {
                    return Err(Error::Validation("archive category".into()));
                }
                sqlx::query("UPDATE archive_categories SET name=$1 WHERE list_id=$2 AND name=$3")
                    .bind(&to)
                    .bind(list.as_str())
                    .bind(&from)
                    .execute(&mut *tx)
                    .await
                    .map_err(db_error)?;
                sqlx::query("UPDATE archive_thread_categories SET category=$1 WHERE list_id=$2 AND category=$3")
                    .bind(&to)
                    .bind(list.as_str())
                    .bind(&from)
                    .execute(&mut *tx)
                    .await
                    .map_err(db_error)?;
                serde_json::json!({"op": "rename", "name": from, "to": to})
            }
            CategoryChange::Remove(name) => {
                let name = normalize_category(name)?;
                if !known(&mut tx, list, &name).await? {
                    return Err(Error::Validation("archive category".into()));
                }
                sqlx::query(
                    "DELETE FROM archive_thread_categories WHERE list_id=$1 AND category=$2",
                )
                .bind(list.as_str())
                .bind(&name)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
                sqlx::query("DELETE FROM archive_categories WHERE list_id=$1 AND name=$2")
                    .bind(list.as_str())
                    .bind(&name)
                    .execute(&mut *tx)
                    .await
                    .map_err(db_error)?;
                serde_json::json!({"op": "remove", "name": name})
            }
        };
        Database::record_tx_with_context(
            &mut tx,
            &crate::AuditContext::new(Some(user), None, None),
            "archive.category",
            "list",
            list.as_str(),
            event,
        )
        .await?;
        Database::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)
    }
}

/// What `browser_categories` does to the list's categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CategoryChange<'a> {
    Add(&'a str),
    Rename { from: &'a str, to: &'a str },
    Remove(&'a str),
}

async fn known(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    list: &ListId,
    name: &str,
) -> Result<bool> {
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM archive_categories WHERE list_id=$1 AND name=$2")
            .bind(list.as_str())
            .bind(name)
            .fetch_one(&mut **tx)
            .await
            .map_err(db_error)?;
    Ok(count > 0)
}

/// Everything keyed by a post's hash goes with the post.
async fn forget_posts(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    list: &ListId,
    hashes: &[String],
) -> Result<()> {
    for hash in hashes {
        for sql in [
            "DELETE FROM archive_attachments WHERE list_id=$1 AND hash=$2",
            "DELETE FROM archive_votes WHERE list_id=$1 AND hash=$2",
            "DELETE FROM archive_messages WHERE list_id=$1 AND hash=$2",
        ] {
            sqlx::query(sql)
                .bind(list.as_str())
                .bind(hash)
                .execute(&mut **tx)
                .await
                .map_err(db_error)?;
        }
    }
    Ok(())
}

/// Everything keyed by a thread goes with the thread, or moves to `to`.
async fn move_thread_marks(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    list: &ListId,
    from: &str,
    to: Option<&str>,
) -> Result<()> {
    const TABLES: [&str; 4] = [
        "archive_tags",
        "archive_thread_categories",
        "archive_favorites",
        "archive_thread_views",
    ];
    for table in TABLES {
        // The new thread is bound in both shapes so the two statements
        // take the same parameter list.
        let sql = match to {
            Some(_) => format!("UPDATE {table} SET thread=$2 WHERE list_id=$1 AND thread=$3"),
            None => {
                format!("DELETE FROM {table} WHERE list_id=$1 AND ($2='' OR 1=1) AND thread=$3")
            }
        };
        sqlx::query(&sql)
            .bind(list.as_str())
            .bind(to.unwrap_or(""))
            .bind(from)
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
    }
    Ok(())
}

/// Delete one post: its replies take its parent, and if it was the
/// thread's root the oldest survivor becomes the new root.
async fn delete_message(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    list: &ListId,
    hash: &str,
) -> Result<u64> {
    let row = sqlx::query(
        "SELECT thread, parent_hash FROM archive_messages WHERE list_id=$1 AND hash=$2",
    )
    .bind(list.as_str())
    .bind(hash)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_error)?;
    let Some(row) = row else { return Ok(0) };
    let thread: String = row.try_get("thread").map_err(db_error)?;
    let parent: Option<String> = row.try_get("parent_hash").map_err(db_error)?;
    sqlx::query("UPDATE archive_messages SET parent_hash=$1 WHERE list_id=$2 AND parent_hash=$3")
        .bind(parent.as_deref())
        .bind(list.as_str())
        .bind(hash)
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    forget_posts(tx, list, std::slice::from_ref(&hash.to_owned())).await?;
    if thread == hash {
        let survivor: Option<String> = sqlx::query_scalar("SELECT hash FROM archive_messages WHERE list_id=$1 AND thread=$2 ORDER BY created_at, hash LIMIT 1")
            .bind(list.as_str())
            .bind(&thread)
            .fetch_optional(&mut **tx)
            .await
            .map_err(db_error)?;
        match survivor {
            Some(root) => {
                sqlx::query("UPDATE archive_messages SET thread=$1 WHERE list_id=$2 AND thread=$3")
                    .bind(&root)
                    .bind(list.as_str())
                    .bind(&thread)
                    .execute(&mut **tx)
                    .await
                    .map_err(db_error)?;
                move_thread_marks(tx, list, &thread, Some(&root)).await?;
            }
            None => move_thread_marks(tx, list, &thread, None).await?,
        }
    }
    Ok(1)
}

/// Delete every post of a thread, and every mark on the thread.
async fn delete_thread(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    list: &ListId,
    thread: &str,
) -> Result<u64> {
    let hashes: Vec<String> = sqlx::query_scalar(
        "SELECT hash FROM archive_messages WHERE list_id=$1 AND thread=$2 LIMIT 5000",
    )
    .bind(list.as_str())
    .bind(thread)
    .fetch_all(&mut **tx)
    .await
    .map_err(db_error)?;
    if hashes.is_empty() {
        return Ok(0);
    }
    forget_posts(tx, list, &hashes).await?;
    move_thread_marks(tx, list, thread, None).await?;
    Ok(hashes.len() as u64)
}
