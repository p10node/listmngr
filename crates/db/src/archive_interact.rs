//! Readers' interactions with the archive: votes on posts, tags and a
//! category on threads, favourite threads.
//!
//! Every write takes the archive's policy for the live session inside a
//! `browser_write_tx`, checks the post or thread exists, and commits the
//! change with its audit event (votes, tags, categories) in that one
//! transaction. Favourites are a reader's own bookmarks and, like the
//! last-view marks, carry no audit event.
use super::{ArchiveRepo, browse::ThreadSummary};
use crate::{Database, db_error, web_sessions::WebSession};
use listmngr_core::{Error, ListId, Result, UserId};
use sqlx::Row as _;

/// One post's votes as a page shows them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoteSummary {
    pub hash: String,
    /// Up votes minus down votes.
    pub score: i64,
    /// The reader's own vote: 1, -1 or 0.
    pub own: i32,
}

/// One tag on a thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub tag: String,
    pub user_id: UserId,
}

/// What a thread page shows besides its posts.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ThreadMeta {
    pub tags: Vec<Tag>,
    pub category: Option<String>,
    /// The list's categories, for the owner's form.
    pub categories: Vec<String>,
    /// Whether the reader keeps this thread as a favourite.
    pub favorite: bool,
}

/// A label as stored: lowercase letters, digits and hyphens, runs of
/// anything else collapsed to one hyphen, at most `max` characters. Tags
/// and categories both take this shape, so neither needs escaping in a
/// URL path.
/// # Errors
/// Returns `Validation` named after `what` for an empty or over-long
/// result.
pub(crate) fn normalize_label(value: &str, max: usize, what: &str) -> Result<String> {
    let mut out = String::with_capacity(value.len());
    for c in value.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').to_owned();
    if out.is_empty() || out.len() > max {
        return Err(Error::Validation(what.to_owned()));
    }
    Ok(out)
}

/// A tag as stored: `normalize_label` at forty characters.
/// # Errors
/// Returns validation for an empty or over-long result.
pub fn normalize_tag(tag: &str) -> Result<String> {
    normalize_label(tag, 40, "archive tag")
}

impl ArchiveRepo<'_> {
    /// The scores of the given posts and the reader's own votes.
    /// # Errors
    /// Database failures.
    pub async fn browser_votes(
        &self,
        list: &ListId,
        reader: Option<UserId>,
        hashes: &[String],
    ) -> Result<Vec<VoteSummary>> {
        let mut out = Vec::with_capacity(hashes.len());
        let reader = reader.map(|u| u.to_string()).unwrap_or_default();
        for hash in hashes.iter().take(500) {
            let row = sqlx::query("SELECT COALESCE(SUM(value), 0) AS score, COALESCE(MAX(CASE WHEN user_id=$3 THEN value END), 0) AS own FROM archive_votes WHERE list_id=$1 AND hash=$2")
                .bind(list.as_str())
                .bind(hash)
                .bind(&reader)
                .fetch_one(self.db.pool())
                .await
                .map_err(db_error)?;
            out.push(VoteSummary {
                hash: hash.clone(),
                score: row.try_get("score").map_err(db_error)?,
                own: row.try_get::<i32, _>("own").map_err(db_error)?,
            });
        }
        Ok(out)
    }

    /// The reader's vote on a post: 1, -1, or 0 to take it back.
    /// # Errors
    /// `Validation` for another value, `NotFound` for an absent post,
    /// `Forbidden` when the archive's policy refuses the reader.
    pub async fn browser_vote(
        &self,
        session: &WebSession,
        list: &ListId,
        hash: &str,
        value: i32,
        now_ms: i64,
    ) -> Result<()> {
        if !(-1..=1).contains(&value) {
            return Err(Error::Validation("archive vote".into()));
        }
        if hash.is_empty() || hash.len() > 200 {
            return Err(Error::Validation("archive message hash bounds".into()));
        }
        let mut tx = self.db.browser_write_tx().await?;
        let user = self.reader_tx(&mut tx, session, list).await?;
        let exists: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM archive_messages WHERE list_id=$1 AND hash=$2 AND hidden_at IS NULL",
        )
        .bind(list.as_str())
        .bind(hash)
        .fetch_one(&mut *tx)
        .await
        .map_err(db_error)?;
        if exists == 0 {
            return Err(Error::NotFound("archive message".into()));
        }
        sqlx::query("DELETE FROM archive_votes WHERE user_id=$1 AND list_id=$2 AND hash=$3")
            .bind(user.to_string())
            .bind(list.as_str())
            .bind(hash)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        if value != 0 {
            sqlx::query("INSERT INTO archive_votes(user_id, list_id, hash, value, voted_at) VALUES($1,$2,$3,$4,$5)")
                .bind(user.to_string())
                .bind(list.as_str())
                .bind(hash)
                .bind(value)
                .bind(now_ms)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
        }
        Database::record_tx_with_context(
            &mut tx,
            &crate::AuditContext::new(Some(user), None, None),
            "archive.vote",
            "list",
            list.as_str(),
            serde_json::json!({"hash": hash, "value": value}),
        )
        .await?;
        Database::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)
    }

    /// What a thread page shows besides its posts.
    /// # Errors
    /// Database failures.
    pub async fn browser_thread_meta(
        &self,
        list: &ListId,
        reader: Option<UserId>,
        thread: &str,
    ) -> Result<ThreadMeta> {
        let rows = sqlx::query("SELECT tag, user_id FROM archive_tags WHERE list_id=$1 AND thread=$2 ORDER BY tag LIMIT 200")
            .bind(list.as_str())
            .bind(thread)
            .fetch_all(self.db.pool())
            .await
            .map_err(db_error)?;
        let mut tags = Vec::with_capacity(rows.len());
        for row in &rows {
            let user: String = row.try_get("user_id").map_err(db_error)?;
            tags.push(Tag {
                tag: row.try_get("tag").map_err(db_error)?,
                user_id: user
                    .parse()
                    .map_err(|_| Error::Validation("archive tag user".into()))?,
            });
        }
        let category: Option<String> = sqlx::query_scalar(
            "SELECT category FROM archive_thread_categories WHERE list_id=$1 AND thread=$2",
        )
        .bind(list.as_str())
        .bind(thread)
        .fetch_optional(self.db.pool())
        .await
        .map_err(db_error)?;
        let categories = self.categories(list).await?;
        let favorite = match reader {
            Some(user) => {
                let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM archive_favorites WHERE user_id=$1 AND list_id=$2 AND thread=$3")
                    .bind(user.to_string())
                    .bind(list.as_str())
                    .bind(thread)
                    .fetch_one(self.db.pool())
                    .await
                    .map_err(db_error)?;
                count > 0
            }
            None => false,
        };
        Ok(ThreadMeta {
            tags,
            category,
            categories,
            favorite,
        })
    }

    /// The list's categories, sorted.
    /// # Errors
    /// Database failures.
    pub async fn categories(&self, list: &ListId) -> Result<Vec<String>> {
        sqlx::query_scalar(
            "SELECT name FROM archive_categories WHERE list_id=$1 ORDER BY name LIMIT 200",
        )
        .bind(list.as_str())
        .fetch_all(self.db.pool())
        .await
        .map_err(db_error)
    }

    /// The tags and the category of each summarised thread.
    /// # Errors
    /// Database failures.
    pub async fn label_threads(&self, list: &ListId, threads: &mut [ThreadSummary]) -> Result<()> {
        for summary in threads.iter_mut() {
            summary.tags = sqlx::query_scalar(
                "SELECT tag FROM archive_tags WHERE list_id=$1 AND thread=$2 ORDER BY tag LIMIT 20",
            )
            .bind(list.as_str())
            .bind(&summary.thread)
            .fetch_all(self.db.pool())
            .await
            .map_err(db_error)?;
            summary.category = sqlx::query_scalar(
                "SELECT category FROM archive_thread_categories WHERE list_id=$1 AND thread=$2",
            )
            .bind(list.as_str())
            .bind(&summary.thread)
            .fetch_optional(self.db.pool())
            .await
            .map_err(db_error)?;
        }
        Ok(())
    }

    /// Add a tag to a thread, or remove one: the tagger or an owner may
    /// remove.
    /// # Errors
    /// `Validation` for a tag that normalises to nothing, `NotFound` for an
    /// absent thread, `Forbidden` for a reader the policy refuses or who may
    /// not remove this tag.
    pub async fn browser_tag(
        &self,
        session: &WebSession,
        list: &ListId,
        thread: &str,
        tag: &str,
        add: bool,
        now_ms: i64,
    ) -> Result<String> {
        let tag = normalize_tag(tag)?;
        let mut tx = self.db.browser_write_tx().await?;
        let user = self.reader_tx(&mut tx, session, list).await?;
        thread_exists_tx(&mut tx, list, thread).await?;
        if add {
            sqlx::query("DELETE FROM archive_tags WHERE list_id=$1 AND thread=$2 AND tag=$3")
                .bind(list.as_str())
                .bind(thread)
                .bind(&tag)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
            sqlx::query("INSERT INTO archive_tags(list_id, thread, tag, user_id, tagged_at) VALUES($1,$2,$3,$4,$5)")
                .bind(list.as_str())
                .bind(thread)
                .bind(&tag)
                .bind(user.to_string())
                .bind(now_ms)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
        } else {
            let tagger: Option<String> = sqlx::query_scalar(
                "SELECT user_id FROM archive_tags WHERE list_id=$1 AND thread=$2 AND tag=$3",
            )
            .bind(list.as_str())
            .bind(thread)
            .bind(&tag)
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_error)?;
            let tagger = tagger.ok_or_else(|| Error::NotFound("archive tag".into()))?;
            if tagger != user.to_string()
                && Database::browser_owner_tx(&mut tx, session, list)
                    .await
                    .is_err()
            {
                return Err(Error::Forbidden(
                    "only the tagger or an owner removes a tag".into(),
                ));
            }
            sqlx::query("DELETE FROM archive_tags WHERE list_id=$1 AND thread=$2 AND tag=$3")
                .bind(list.as_str())
                .bind(thread)
                .bind(&tag)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
        }
        Database::record_tx_with_context(
            &mut tx,
            &crate::AuditContext::new(Some(user), None, None),
            "archive.tag",
            "list",
            list.as_str(),
            serde_json::json!({"thread": thread, "tag": tag, "added": add}),
        )
        .await?;
        Database::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(tag)
    }

    /// An owner files a thread under one of the list's categories, or under
    /// none.
    /// # Errors
    /// `Validation` for a category the list does not have, `NotFound` for an
    /// absent thread, `Forbidden` for anyone but an owner.
    pub async fn browser_set_category(
        &self,
        session: &WebSession,
        list: &ListId,
        thread: &str,
        category: Option<&str>,
    ) -> Result<()> {
        let mut tx = self.db.browser_write_tx().await?;
        let user = Database::browser_owner_tx(&mut tx, session, list).await?;
        thread_exists_tx(&mut tx, list, thread).await?;
        if let Some(name) = category {
            let known: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM archive_categories WHERE list_id=$1 AND name=$2",
            )
            .bind(list.as_str())
            .bind(name)
            .fetch_one(&mut *tx)
            .await
            .map_err(db_error)?;
            if known == 0 {
                return Err(Error::Validation("archive category".into()));
            }
        }
        sqlx::query("DELETE FROM archive_thread_categories WHERE list_id=$1 AND thread=$2")
            .bind(list.as_str())
            .bind(thread)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        if let Some(name) = category {
            sqlx::query(
                "INSERT INTO archive_thread_categories(list_id, thread, category) VALUES($1,$2,$3)",
            )
            .bind(list.as_str())
            .bind(thread)
            .bind(name)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        }
        Database::record_tx_with_context(
            &mut tx,
            &crate::AuditContext::new(Some(user), None, None),
            "archive.category",
            "list",
            list.as_str(),
            serde_json::json!({"thread": thread, "category": category}),
        )
        .await?;
        Database::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)
    }

    /// Keep a thread among the reader's favourites, or drop it.
    /// # Errors
    /// `NotFound` for an absent thread, `Forbidden` for a reader the
    /// policy refuses.
    pub async fn browser_favorite(
        &self,
        session: &WebSession,
        list: &ListId,
        thread: &str,
        on: bool,
        now_ms: i64,
    ) -> Result<()> {
        let mut tx = self.db.browser_write_tx().await?;
        let user = self.reader_tx(&mut tx, session, list).await?;
        thread_exists_tx(&mut tx, list, thread).await?;
        sqlx::query("DELETE FROM archive_favorites WHERE user_id=$1 AND list_id=$2 AND thread=$3")
            .bind(user.to_string())
            .bind(list.as_str())
            .bind(thread)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        if on {
            sqlx::query("INSERT INTO archive_favorites(user_id, list_id, thread, marked_at) VALUES($1,$2,$3,$4)")
                .bind(user.to_string())
                .bind(list.as_str())
                .bind(thread)
                .bind(now_ms)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
        }
        Database::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)
    }

    /// The live user behind the session, admitted by the archive's policy
    /// (any signed-in reader of a public archive; a verified member of a
    /// private one).
    async fn reader_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Any>,
        session: &WebSession,
        list: &ListId,
    ) -> Result<UserId> {
        let user = Database::browser_user_tx(tx, session).await?;
        let policy: Option<String> =
            sqlx::query_scalar("SELECT archive_policy FROM mailing_lists WHERE list_id=$1")
                .bind(list.as_str())
                .fetch_optional(&mut **tx)
                .await
                .map_err(db_error)?;
        match policy.as_deref() {
            Some("public") => Ok(user),
            Some("private") => {
                let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id=$1 AND m.role='member' AND a.user_id=$2 AND a.verified_on IS NOT NULL AND (m.subscription_mode='as_address' OR m.user_id=$2)")
                    .bind(list.as_str()).bind(user.to_string()).fetch_one(&mut **tx).await.map_err(db_error)?;
                if count == 0 {
                    return Err(Error::Forbidden(
                        "verified archive membership required".into(),
                    ));
                }
                Ok(user)
            }
            _ => Err(Error::NotFound("archive".into())),
        }
    }
}

async fn thread_exists_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    list: &ListId,
    thread: &str,
) -> Result<()> {
    if thread.is_empty() || thread.len() > 200 {
        return Err(Error::Validation("archive thread bounds".into()));
    }
    let count: i64 =
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM archive_messages WHERE list_id=$1 AND thread=$2 AND hidden_at IS NULL",
        )
            .bind(list.as_str())
            .bind(thread)
            .fetch_one(&mut **tx)
            .await
            .map_err(db_error)?;
    if count == 0 {
        return Err(Error::NotFound("archive thread".into()));
    }
    Ok(())
}
