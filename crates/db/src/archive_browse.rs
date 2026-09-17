//! The archive's browsing views: overview figures, thread lists by
//! activity and by month, senders, last-view marks and the feed's posts.
//!
//! Every read first passes the archive's policy for the reader
//! (`ArchiveRepo::browser_authorize`); the aggregates themselves are plain
//! bounded queries over `archive_messages`, portable across `SQLite` and
//! `PostgreSQL` (months are bucketed in Rust from the stored milliseconds).
use super::{ArchiveMessage, ArchiveRepo};
use crate::{Database, db_error, web_sessions::WebSession};
use listmngr_core::{ArchivePolicy, Error, ListId, Result, UserId};
use sqlx::Row as _;

/// One thread as a list shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadSummary {
    pub thread: String,
    /// The root's subject, or the earliest post's when the root is absent.
    pub subject: String,
    pub posts: i64,
    pub participants: i64,
    /// Milliseconds of the earliest and the latest post.
    pub started_ms: i64,
    pub last_ms: i64,
    /// The latest post's sender, name or address.
    pub last_sender: String,
    /// Whether the signed-in reader has posts here they have not seen
    /// (never set for a visitor).
    pub unread: bool,
}

/// One sender as the overview and the sender page show them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Poster {
    pub name: String,
    pub email: String,
    pub posts: i64,
}

/// One month with posts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Month {
    pub year: i32,
    pub month: u32,
    pub posts: i64,
}

/// The overview's figures and lists.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Overview {
    pub posts: i64,
    pub threads: i64,
    pub participants: i64,
    pub months: Vec<Month>,
    pub recent: Vec<ThreadSummary>,
    pub active: Vec<ThreadSummary>,
    pub top_posters: Vec<Poster>,
}

/// Which threads a list asks for.
#[derive(Debug, Clone, Copy)]
pub enum ThreadSelection {
    /// By latest activity.
    Latest,
    /// Posts dated within `[from_ms, until_ms)`.
    Between { from_ms: i64, until_ms: i64 },
    /// By posts in the last `days`.
    Active { since_ms: i64 },
}

const THREAD_SQL: &str = "SELECT t.thread, t.posts, t.participants, t.started, t.last, COALESCE((SELECT r.subject FROM archive_messages r WHERE r.list_id=$1 AND r.hash=t.thread), (SELECT e.subject FROM archive_messages e WHERE e.list_id=$1 AND e.thread=t.thread ORDER BY e.created_at, e.hash LIMIT 1), '') AS subject, COALESCE((SELECT CASE WHEN l.sender_name<>'' THEN l.sender_name ELSE l.sender_email END FROM archive_messages l WHERE l.list_id=$1 AND l.thread=t.thread ORDER BY l.created_at DESC, l.hash DESC LIMIT 1), '') AS last_sender FROM (SELECT thread, COUNT(*) AS posts, COUNT(DISTINCT sender_email) AS participants, MIN(COALESCE(message_date, created_at)) AS started, MAX(COALESCE(message_date, created_at)) AS last FROM archive_messages WHERE list_id=$1 AND COALESCE(message_date, created_at)>=$2 AND COALESCE(message_date, created_at)<$3 GROUP BY thread) t ORDER BY ";

fn month_of(ms: i64) -> Option<(i32, u32)> {
    use chrono::Datelike as _;
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms).map(|d| (d.year(), d.month()))
}

/// The first millisecond of `year`-`month` and of the month after.
/// # Errors
/// Returns validation for a month outside 1..=12 or a year out of range.
pub fn month_bounds(year: i32, month: u32) -> Result<(i64, i64)> {
    let start = chrono::NaiveDate::from_ymd_opt(year, month, 1)
        .ok_or_else(|| Error::Validation("archive month".into()))?;
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let end = chrono::NaiveDate::from_ymd_opt(next_year, next_month, 1)
        .ok_or_else(|| Error::Validation("archive month".into()))?;
    let to_ms = |date: chrono::NaiveDate| {
        date.and_hms_opt(0, 0, 0)
            .map(|dt| dt.and_utc().timestamp_millis())
            .ok_or_else(|| Error::Validation("archive month".into()))
    };
    Ok((to_ms(start)?, to_ms(end)?))
}

impl ArchiveRepo<'_> {
    /// The archive's policy for this reader: public opens to anyone,
    /// private to a verified member with a live session, `never` is not
    /// there.
    /// # Errors
    /// `NotFound`, `Forbidden`, a stale session or database failures.
    pub async fn browser_authorize(
        &self,
        list: &ListId,
        session: Option<&WebSession>,
    ) -> Result<()> {
        let policy = self.db.lists().get(list).await?.archive_policy;
        match policy {
            ArchivePolicy::Public => Ok(()),
            ArchivePolicy::Never => Err(Error::NotFound("archive".into())),
            ArchivePolicy::Private => {
                let session = session.ok_or_else(|| Error::Forbidden("private archive".into()))?;
                let mut tx = self.db.browser_write_tx().await?;
                let user = Database::browser_user_tx(&mut tx, session).await?;
                let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id=$1 AND m.role='member' AND a.user_id=$2 AND a.verified_on IS NOT NULL AND (m.subscription_mode='as_address' OR m.user_id=$2)")
                    .bind(list.as_str()).bind(user.to_string()).fetch_one(&mut *tx).await.map_err(db_error)?;
                tx.commit().await.map_err(db_error)?;
                if count == 0 {
                    return Err(Error::Forbidden(
                        "verified archive membership required".into(),
                    ));
                }
                Ok(())
            }
        }
    }

    async fn threads(
        &self,
        list: &ListId,
        reader: Option<UserId>,
        selection: ThreadSelection,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ThreadSummary>> {
        let (from, until, order) = match selection {
            ThreadSelection::Latest => (i64::MIN, i64::MAX, "t.last DESC, t.thread"),
            ThreadSelection::Between { from_ms, until_ms } => {
                (from_ms, until_ms, "t.last DESC, t.thread")
            }
            ThreadSelection::Active { since_ms } => {
                (since_ms, i64::MAX, "t.posts DESC, t.last DESC, t.thread")
            }
        };
        let sql = format!("{THREAD_SQL}{order} LIMIT $4 OFFSET $5");
        let rows = sqlx::query(&sql)
            .bind(list.as_str())
            .bind(from)
            .bind(until)
            .bind(limit.clamp(1, 200))
            .bind(offset.clamp(0, 100_000))
            .fetch_all(self.db.pool())
            .await
            .map_err(db_error)?;
        let views = match reader {
            Some(user) => self.views(list, user).await?,
            None => Vec::new(),
        };
        rows.iter()
            .map(|row| {
                let thread: String = row.try_get("thread").map_err(db_error)?;
                let last_ms: i64 = row.try_get("last").map_err(db_error)?;
                let unread = reader.is_some()
                    && views
                        .iter()
                        .find(|(seen, _)| *seen == thread)
                        .is_none_or(|(_, at)| *at < last_ms);
                Ok(ThreadSummary {
                    subject: row.try_get("subject").map_err(db_error)?,
                    posts: row.try_get("posts").map_err(db_error)?,
                    participants: row.try_get("participants").map_err(db_error)?,
                    started_ms: row.try_get("started").map_err(db_error)?,
                    last_ms,
                    last_sender: row.try_get("last_sender").map_err(db_error)?,
                    unread,
                    thread,
                })
            })
            .collect()
    }

    async fn views(&self, list: &ListId, user: UserId) -> Result<Vec<(String, i64)>> {
        let rows = sqlx::query("SELECT thread, viewed_at FROM archive_thread_views WHERE user_id=$1 AND list_id=$2 LIMIT 5000")
            .bind(user.to_string())
            .bind(list.as_str())
            .fetch_all(self.db.pool())
            .await
            .map_err(db_error)?;
        rows.iter()
            .map(|row| {
                Ok((
                    row.try_get("thread").map_err(db_error)?,
                    row.try_get("viewed_at").map_err(db_error)?,
                ))
            })
            .collect()
    }

    /// The overview: totals, months, the latest threads, the threads most
    /// active in the last thirty days and the senders who posted most.
    /// # Errors
    /// Policy and database failures.
    pub async fn browser_overview(
        &self,
        list: &ListId,
        session: Option<&WebSession>,
        now_ms: i64,
    ) -> Result<Overview> {
        self.browser_authorize(list, session).await?;
        let reader = session.and_then(|s| s.user_id);
        let row = sqlx::query("SELECT COUNT(*) AS posts, COUNT(DISTINCT thread) AS threads, COUNT(DISTINCT sender_email) AS participants FROM archive_messages WHERE list_id=$1")
            .bind(list.as_str())
            .fetch_one(self.db.pool())
            .await
            .map_err(db_error)?;
        let dates: Vec<i64> = sqlx::query_scalar("SELECT COALESCE(message_date, created_at) FROM archive_messages WHERE list_id=$1 LIMIT 200000")
            .bind(list.as_str())
            .fetch_all(self.db.pool())
            .await
            .map_err(db_error)?;
        let mut months: Vec<Month> = Vec::new();
        for (year, month) in dates.into_iter().filter_map(month_of) {
            match months
                .iter_mut()
                .find(|m| m.year == year && m.month == month)
            {
                Some(m) => m.posts += 1,
                None => months.push(Month {
                    year,
                    month,
                    posts: 1,
                }),
            }
        }
        months.sort_by(|a, b| (b.year, b.month).cmp(&(a.year, a.month)));
        let since = now_ms - 30 * 24 * 3600 * 1000;
        let posters = sqlx::query("SELECT sender_name, sender_email, COUNT(*) AS posts FROM archive_messages WHERE list_id=$1 AND COALESCE(message_date, created_at)>=$2 AND sender_email<>'' GROUP BY sender_email, sender_name ORDER BY COUNT(*) DESC, sender_email LIMIT 10")
            .bind(list.as_str())
            .bind(since)
            .fetch_all(self.db.pool())
            .await
            .map_err(db_error)?;
        Ok(Overview {
            posts: row.try_get("posts").map_err(db_error)?,
            threads: row.try_get("threads").map_err(db_error)?,
            participants: row.try_get("participants").map_err(db_error)?,
            months,
            recent: self
                .threads(list, reader, ThreadSelection::Latest, 10, 0)
                .await?,
            active: self
                .threads(
                    list,
                    reader,
                    ThreadSelection::Active { since_ms: since },
                    10,
                    0,
                )
                .await?,
            top_posters: posters
                .iter()
                .map(|row| {
                    Ok(Poster {
                        name: row.try_get("sender_name").map_err(db_error)?,
                        email: row.try_get("sender_email").map_err(db_error)?,
                        posts: row.try_get("posts").map_err(db_error)?,
                    })
                })
                .collect::<Result<Vec<_>>>()?,
        })
    }

    /// A page of threads (21 at most, the last signalling a next page).
    /// # Errors
    /// Policy and database failures.
    pub async fn browser_threads(
        &self,
        list: &ListId,
        session: Option<&WebSession>,
        selection: ThreadSelection,
        offset: i64,
    ) -> Result<Vec<ThreadSummary>> {
        self.browser_authorize(list, session).await?;
        self.threads(list, session.and_then(|s| s.user_id), selection, 21, offset)
            .await
    }

    /// Record that the reader opened a thread now.
    /// # Errors
    /// A stale session or database failures.
    pub async fn browser_mark_viewed(
        &self,
        list: &ListId,
        session: &WebSession,
        thread: &str,
        now_ms: i64,
    ) -> Result<()> {
        let Some(user) = session.user_id else {
            return Ok(());
        };
        if thread.is_empty() || thread.len() > 200 {
            return Ok(());
        }
        sqlx::query(
            "DELETE FROM archive_thread_views WHERE user_id=$1 AND list_id=$2 AND thread=$3",
        )
        .bind(user.to_string())
        .bind(list.as_str())
        .bind(thread)
        .execute(self.db.pool())
        .await
        .map_err(db_error)?;
        sqlx::query("INSERT INTO archive_thread_views(user_id, list_id, thread, viewed_at) VALUES($1,$2,$3,$4)")
            .bind(user.to_string())
            .bind(list.as_str())
            .bind(thread)
            .bind(now_ms)
            .execute(self.db.pool())
            .await
            .map_err(db_error)?;
        Ok(())
    }

    /// The senders of a list (5000 at most), for the sender page's lookup.
    /// # Errors
    /// Policy and database failures.
    pub async fn browser_senders(
        &self,
        list: &ListId,
        session: Option<&WebSession>,
    ) -> Result<Vec<Poster>> {
        self.browser_authorize(list, session).await?;
        let rows = sqlx::query("SELECT sender_name, sender_email, COUNT(*) AS posts FROM archive_messages WHERE list_id=$1 AND sender_email<>'' GROUP BY sender_email, sender_name ORDER BY sender_email, sender_name LIMIT 5000")
            .bind(list.as_str())
            .fetch_all(self.db.pool())
            .await
            .map_err(db_error)?;
        rows.iter()
            .map(|row| {
                Ok(Poster {
                    name: row.try_get("sender_name").map_err(db_error)?,
                    email: row.try_get("sender_email").map_err(db_error)?,
                    posts: row.try_get("posts").map_err(db_error)?,
                })
            })
            .collect()
    }

    /// A page of one sender's posts (21 at most), newest first, through the
    /// same rendering as any page.
    /// # Errors
    /// Policy and database failures.
    pub async fn browser_sender_posts(
        &self,
        list: &ListId,
        session: Option<&WebSession>,
        email: &str,
        offset: i64,
    ) -> Result<Vec<ArchiveMessage>> {
        self.browser_authorize(list, session).await?;
        let hashes: Vec<String> = sqlx::query_scalar("SELECT hash FROM archive_messages WHERE list_id=$1 AND sender_email=$2 ORDER BY COALESCE(message_date, created_at) DESC, hash LIMIT 21 OFFSET $3")
            .bind(list.as_str())
            .bind(email)
            .bind(offset.clamp(0, 100_000))
            .fetch_all(self.db.pool())
            .await
            .map_err(db_error)?;
        let mut messages = Vec::with_capacity(hashes.len());
        for hash in hashes {
            messages.push(self.read_browser_message(list, session, &hash).await?);
        }
        Ok(messages)
    }

    /// The latest posts (20), newest first, for the feed.
    /// # Errors
    /// Policy and database failures.
    pub async fn browser_latest_posts(
        &self,
        list: &ListId,
        session: Option<&WebSession>,
    ) -> Result<Vec<ArchiveMessage>> {
        self.browser_authorize(list, session).await?;
        let hashes: Vec<String> = sqlx::query_scalar("SELECT hash FROM archive_messages WHERE list_id=$1 ORDER BY COALESCE(message_date, created_at) DESC, hash LIMIT 20")
            .bind(list.as_str())
            .fetch_all(self.db.pool())
            .await
            .map_err(db_error)?;
        let mut messages = Vec::with_capacity(hashes.len());
        for hash in hashes {
            messages.push(self.read_browser_message(list, session, &hash).await?);
        }
        Ok(messages)
    }
}
