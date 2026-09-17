//! Browser-only archive authority: no synthesized bearer identity or admin scope.
use super::{ArchiveMessage, ArchiveRepo, Selection, render_rows};
use crate::{Database, db_error, web_sessions::WebSession};
use listmngr_core::{ArchivePolicy, Error, ListId, Result};

impl ArchiveRepo<'_> {
    /// Read a browser archive selection with current session and membership authority.
    /// # Errors
    /// Rejects missing/revoked authority, disabled archives and invalid bounds.
    pub async fn read_browser(
        &self,
        list: &ListId,
        session: Option<&WebSession>,
        thread: Option<&str>,
        query: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ArchiveMessage>> {
        self.read_browser_selection(
            list,
            session,
            Selection::Thread(thread),
            query,
            limit,
            offset,
        )
        .await
    }

    /// Read one browser message with the same current authorization as page/export.
    /// # Errors
    /// Rejects invalid hashes, inaccessible archives and missing messages.
    pub async fn read_browser_message(
        &self,
        list: &ListId,
        session: Option<&WebSession>,
        hash: &str,
    ) -> Result<ArchiveMessage> {
        if hash.is_empty() || hash.len() > 200 {
            return Err(Error::Validation("archive message hash bounds".into()));
        }
        self.read_browser_selection(list, session, Selection::Message(hash), "", 1, 0)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| Error::NotFound("archive message".into()))
    }

    async fn read_browser_selection(
        &self,
        list: &ListId,
        session: Option<&WebSession>,
        selection: Selection<'_>,
        query: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ArchiveMessage>> {
        let settings = self.db.lists().get(list).await?;
        // Anonymous/public requests retain the existing authorization-qualified SELECT.
        if settings.archive_policy != ArchivePolicy::Private {
            self.authorize(list, None).await?;
            return self
                .read_selection(list, None, selection, query, limit, offset)
                .await;
        }
        let session = session.ok_or_else(|| Error::Forbidden("private archive".into()))?;
        let (thread, hash) = match selection {
            Selection::Thread(thread) => (thread.unwrap_or(""), ""),
            Selection::Message(hash) => ("", hash),
        };
        if !(1..=500).contains(&limit)
            || !(0..=100_000).contains(&offset)
            || query.len() > 200
            || thread.len() > 200
        {
            return Err(Error::Validation("archive query bounds".into()));
        }
        // Conflicts with ordinary session/credential/address/membership/list DML.
        // Authority is not sampled until acquisition and all retries have completed.
        let mut tx = self.db.browser_write_tx().await?;
        let policy: String =
            sqlx::query_scalar("SELECT archive_policy FROM mailing_lists WHERE list_id=$1")
                .bind(list.as_str())
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?;
        let private = match policy.as_str() {
            "public" => false,
            "private" => true,
            _ => return Err(Error::NotFound("archive".into())),
        };
        if private {
            let user = Database::browser_user_tx(&mut tx, session).await?;
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id=$1 AND m.role='member' AND a.user_id=$2 AND a.verified_on IS NOT NULL AND (m.subscription_mode='as_address' OR m.user_id=$2)")
                .bind(list.as_str()).bind(user.to_string()).fetch_one(&mut *tx).await.map_err(db_error)?;
            if count == 0 {
                return Err(Error::Forbidden(
                    "verified archive membership required".into(),
                ));
            }
        }
        let pattern = format!(
            "%{}%",
            query
                .replace('!', "!!")
                .replace('%', "!%")
                .replace('_', "!_")
        );
        let rows = sqlx::query("SELECT hash,thread,raw_b64,sender_name,sender_email,message_date,parent_hash,(SELECT anonymous_list FROM mailing_lists WHERE list_id=$1) AS anonymous_list,(SELECT subject_prefix FROM mailing_lists WHERE list_id=$1) AS subject_prefix FROM archive_messages WHERE list_id=$1 AND ($2='' OR thread=$2) AND ($3='' OR hash=$3) AND (LOWER(subject) LIKE LOWER($4) ESCAPE '!' OR LOWER(body) LIKE LOWER($4) ESCAPE '!') ORDER BY created_at,hash LIMIT $5 OFFSET $6")
            .bind(list.as_str()).bind(thread).bind(hash).bind(pattern).bind(limit).bind(offset)
            .fetch_all(&mut *tx).await.map_err(db_error)?;
        if private {
            // Reject expiry across a later archive relation/IO wait as well.
            Database::browser_user_tx(&mut tx, session).await?;
        }
        tx.commit().await.map_err(db_error)?;
        let mut messages = render_rows(settings, &rows, self.db.base_url())?;
        self.attach_metadata(list, &mut messages).await?;
        Ok(messages)
    }

    /// One whole thread (500 posts at most) for the browser's tree view.
    /// # Errors
    /// Rejects inaccessible archives and invalid bounds.
    pub async fn read_browser_thread(
        &self,
        list: &ListId,
        session: Option<&WebSession>,
        thread: &str,
    ) -> Result<Vec<ArchiveMessage>> {
        if thread.is_empty() || thread.len() > 200 {
            return Err(Error::Validation("archive thread bounds".into()));
        }
        self.read_browser(list, session, Some(thread), "", 500, 0)
            .await
    }

    /// One stored attachment for the browser, after its message was
    /// authorized the way the page was.
    /// # Errors
    /// Rejects inaccessible archives and missing rows.
    pub async fn read_browser_attachment(
        &self,
        list: &ListId,
        session: Option<&WebSession>,
        hash: &str,
        position: i64,
    ) -> Result<super::AttachmentContent> {
        self.read_browser_message(list, session, hash).await?;
        self.attachment_row(list, hash, position).await
    }

    /// Move a post under another (or make it a root) and re-root its
    /// replies, for the list's owner, in one audited transaction. A post
    /// cannot become its own ancestor.
    /// # Errors
    /// Rejects revoked authority, an unknown post or parent, a cycle, and
    /// database or audit failures.
    pub async fn browser_reattach(
        &self,
        session: &WebSession,
        list: &ListId,
        hash: &str,
        parent: Option<&str>,
    ) -> Result<()> {
        if hash.is_empty()
            || hash.len() > 200
            || parent.is_some_and(|p| p.is_empty() || p.len() > 200)
        {
            return Err(Error::Validation("archive message hash bounds".into()));
        }
        let mut tx = self.db.browser_write_tx().await?;
        let user = Database::browser_owner_tx(&mut tx, session, list).await?;
        let old_thread: Option<String> =
            sqlx::query_scalar("SELECT thread FROM archive_messages WHERE list_id=$1 AND hash=$2")
                .bind(list.as_str())
                .bind(hash)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_error)?;
        let old_thread = old_thread.ok_or_else(|| Error::NotFound("archive message".into()))?;
        let new_thread = new_thread_for(&mut tx, list, hash, parent).await?;
        sqlx::query(
            "UPDATE archive_messages SET parent_hash=$1, thread=$2 WHERE list_id=$3 AND hash=$4",
        )
        .bind(parent)
        .bind(&new_thread)
        .bind(list.as_str())
        .bind(hash)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        let moved = move_descendants(&mut tx, list, hash, &old_thread, &new_thread).await?;
        Database::record_tx_with_context(
            &mut tx,
            &crate::AuditContext::new(Some(user), None, None),
            "archive.reattach",
            "list",
            list.as_str(),
            serde_json::json!({"hash": hash, "parent": parent, "thread": new_thread, "moved": moved}),
        )
        .await?;
        Database::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)
    }
}

/// The thread a post joins under `parent` (its own hash when it becomes a
/// root); the parent must exist in the list and must not descend from the
/// post.
async fn new_thread_for(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    list: &ListId,
    hash: &str,
    parent: Option<&str>,
) -> Result<String> {
    let Some(parent) = parent else {
        return Ok(hash.to_owned());
    };
    if parent == hash {
        return Err(Error::Validation("a post cannot reply to itself".into()));
    }
    let mut cursor = Some(parent.to_owned());
    let mut steps = 0;
    while let Some(current) = cursor {
        if current == hash {
            return Err(Error::Validation(
                "a post cannot become its own ancestor".into(),
            ));
        }
        steps += 1;
        if steps > 5000 {
            break;
        }
        let row: Option<(String, Option<String>)> = sqlx::query_as(
            "SELECT thread, parent_hash FROM archive_messages WHERE list_id=$1 AND hash=$2",
        )
        .bind(list.as_str())
        .bind(&current)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_error)?;
        let Some((_, next)) = row else {
            if current == parent {
                return Err(Error::NotFound("archive parent".into()));
            }
            break;
        };
        cursor = next;
    }
    sqlx::query_scalar("SELECT thread FROM archive_messages WHERE list_id=$1 AND hash=$2")
        .bind(list.as_str())
        .bind(parent)
        .fetch_one(&mut **tx)
        .await
        .map_err(db_error)
}

/// Every reply under the moved post follows it into the new thread; how
/// many rows moved, the post itself included.
async fn move_descendants(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    list: &ListId,
    hash: &str,
    old_thread: &str,
    new_thread: &str,
) -> Result<usize> {
    let rows: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT hash, parent_hash FROM archive_messages WHERE list_id=$1 AND thread=$2 AND hash<>$3 LIMIT 5000",
    )
    .bind(list.as_str())
    .bind(old_thread)
    .bind(hash)
    .fetch_all(&mut **tx)
    .await
    .map_err(db_error)?;
    let mut moved = std::collections::BTreeSet::from([hash.to_owned()]);
    let mut changed = true;
    while changed {
        changed = false;
        for (child, child_parent) in &rows {
            if !moved.contains(child) && child_parent.as_deref().is_some_and(|p| moved.contains(p))
            {
                moved.insert(child.clone());
                changed = true;
            }
        }
    }
    for child in moved.iter().filter(|c| c.as_str() != hash) {
        sqlx::query("UPDATE archive_messages SET thread=$1 WHERE list_id=$2 AND hash=$3")
            .bind(new_thread)
            .bind(list.as_str())
            .bind(child)
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
    }
    Ok(moved.len())
}
