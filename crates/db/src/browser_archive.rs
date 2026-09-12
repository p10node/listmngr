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
        if !(1..=100).contains(&limit)
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
        let rows = sqlx::query("SELECT hash,thread,raw_b64,(SELECT anonymous_list FROM mailing_lists WHERE list_id=$1) AS anonymous_list,(SELECT subject_prefix FROM mailing_lists WHERE list_id=$1) AS subject_prefix FROM archive_messages WHERE list_id=$1 AND ($2='' OR thread=$2) AND ($3='' OR hash=$3) AND (LOWER(subject) LIKE LOWER($4) ESCAPE '!' OR LOWER(body) LIKE LOWER($4) ESCAPE '!') ORDER BY created_at,hash LIMIT $5 OFFSET $6")
            .bind(list.as_str()).bind(thread).bind(hash).bind(pattern).bind(limit).bind(offset)
            .fetch_all(&mut *tx).await.map_err(db_error)?;
        if private {
            // Reject expiry across a later archive relation/IO wait as well.
            Database::browser_user_tx(&mut tx, session).await?;
        }
        tx.commit().await.map_err(db_error)?;
        render_rows(settings, &rows, self.db.base_url())
    }
}
