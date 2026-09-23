//! `HyperKitty`'s own database: what readers left on the old archive.
//!
//! `HyperKitty` keeps a message's text, not the message, so the posts
//! themselves come over as an mbox (`listmngr archive import`). What an
//! mbox cannot carry is here: the votes, the tags, the category of each
//! thread and the favourites, keyed by the same Message-ID-Hash this
//! archive uses, so they land on the posts the mbox brought.
use crate::{Error, Result};
use listmngr_core::ListId;
use listmngr_db::{AuditContext, Database};
use listmngr_db::{ImportedInteractions, InteractionReport};
use serde_json::{Value as Json, json};
use sqlx::Row;
use sqlx::any::{AnyPoolOptions, AnyRow};
use std::sync::Once;

static DRIVERS: Once = Once::new();

/// What `HyperKitty` holds for one list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Archive {
    pub list_id: ListId,
    /// `(message hash, the reader's address, 1 or -1)`.
    pub votes: Vec<(String, String, i32)>,
    /// `(thread, tag, the tagger's address)`.
    pub tags: Vec<(String, String, String)>,
    /// `(thread, category)`.
    pub categories: Vec<(String, String)>,
    /// `(thread, the reader's address)`.
    pub favorites: Vec<(String, String)>,
    /// How many messages and threads the old archive had, for the report.
    pub messages: usize,
    pub threads: usize,
}

impl Archive {
    /// The archive as JSON, for `--dry-run`.
    #[must_use]
    pub fn to_json(&self) -> Json {
        json!({
            "list_id": self.list_id.as_str(),
            "messages": self.messages,
            "threads": self.threads,
            "votes": self.votes.len(),
            "tags": self.tags.len(),
            "categories": self.categories.len(),
            "favorites": self.favorites.len(),
        })
    }
}

fn db_error(error: &sqlx::Error) -> Error {
    Error::Database(crate::db3::scrub(&error.to_string()))
}

fn text(row: &AnyRow, column: &str) -> String {
    row.try_get::<String, _>(column).unwrap_or_default()
}

fn int(row: &AnyRow, column: &str) -> Option<i64> {
    row.try_get::<i64, _>(column)
        .or_else(|_| row.try_get::<i32, _>(column).map(i64::from))
        .or_else(|_| row.try_get::<i16, _>(column).map(i64::from))
        .ok()
}

/// Read every list `HyperKitty` archives, or one of them.
///
/// # Errors
/// `Database` when the database cannot be opened or a query fails, and
/// `Core` for a list identifier this site cannot parse.
pub async fn fetch(url: &str, only: Option<&ListId>) -> Result<Vec<Archive>> {
    DRIVERS.call_once(sqlx::any::install_default_drivers);
    let url = url.replacen("postgresql://", "postgres://", 1);
    let pool = AnyPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .map_err(|error| Error::Database(crate::db3::scrub(&error.to_string())))?;
    let mut archives = Vec::new();
    let lists = sqlx::query("SELECT id, list_id FROM hyperkitty_mailinglist ORDER BY id")
        .fetch_all(&pool)
        .await
        .map_err(|error| db_error(&error))?;
    for row in &lists {
        let list_id: ListId = text(row, "list_id").parse()?;
        if only.is_some_and(|wanted| wanted != &list_id) {
            continue;
        }
        let Some(id) = int(row, "id") else {
            continue;
        };
        archives.push(one(&pool, id, list_id).await?);
    }
    pool.close().await;
    Ok(archives)
}

async fn one(pool: &sqlx::AnyPool, id: i64, list_id: ListId) -> Result<Archive> {
    let rows = |sql: String| async move {
        sqlx::query(&sql)
            .fetch_all(pool)
            .await
            .map_err(|error| db_error(&error))
    };
    let mut archive = Archive {
        list_id,
        votes: Vec::new(),
        tags: Vec::new(),
        categories: Vec::new(),
        favorites: Vec::new(),
        messages: 0,
        threads: 0,
    };
    archive.messages = rows(format!(
        "SELECT id FROM hyperkitty_email WHERE mailinglist_id={id}"
    ))
    .await?
    .len();
    archive.threads = rows(format!(
        "SELECT id FROM hyperkitty_thread WHERE mailinglist_id={id}"
    ))
    .await?
    .len();
    // A vote belongs to a message; the reader is a Django account, whose
    // address is the one this site matches on.
    for row in rows(format!(
        "SELECT e.message_id_hash AS hash, u.email AS email, v.value AS value \
         FROM hyperkitty_vote v \
         JOIN hyperkitty_email e ON e.id=v.email_id \
         JOIN auth_user u ON u.id=v.user_id \
         WHERE e.mailinglist_id={id} ORDER BY v.id"
    ))
    .await?
    {
        let value = int(&row, "value").unwrap_or(0);
        let Ok(value) = i32::try_from(value) else {
            continue;
        };
        archive
            .votes
            .push((text(&row, "hash"), text(&row, "email"), value));
    }
    for row in rows(format!(
        "SELECT t.thread_id AS thread, g.name AS name, u.email AS email \
         FROM hyperkitty_tagging tg \
         JOIN hyperkitty_thread t ON t.id=tg.thread_id \
         JOIN hyperkitty_tag g ON g.id=tg.tag_id \
         JOIN auth_user u ON u.id=tg.user_id \
         WHERE t.mailinglist_id={id} ORDER BY tg.id"
    ))
    .await?
    {
        archive.tags.push((
            text(&row, "thread"),
            text(&row, "name"),
            text(&row, "email"),
        ));
    }
    for row in rows(format!(
        "SELECT t.thread_id AS thread, c.name AS name \
         FROM hyperkitty_thread t \
         JOIN hyperkitty_threadcategory c ON c.id=t.category_id \
         WHERE t.mailinglist_id={id} ORDER BY t.id"
    ))
    .await?
    {
        archive
            .categories
            .push((text(&row, "thread"), text(&row, "name")));
    }
    for row in rows(format!(
        "SELECT t.thread_id AS thread, u.email AS email \
         FROM hyperkitty_favorite f \
         JOIN hyperkitty_thread t ON t.id=f.thread_id \
         JOIN auth_user u ON u.id=f.user_id \
         WHERE t.mailinglist_id={id} ORDER BY f.id"
    ))
    .await?
    {
        archive
            .favorites
            .push((text(&row, "thread"), text(&row, "email")));
    }
    Ok(archive)
}

/// Write one list's interactions onto this site's archive.
///
/// # Errors
/// Whatever the repository refuses: an unknown list, or a database
/// failure.
pub async fn apply(
    db: &Database,
    archive: &Archive,
    context: &AuditContext,
    now_ms: i64,
) -> Result<InteractionReport> {
    Ok(db
        .archive()
        .import_interactions(
            &ImportedInteractions {
                list: &archive.list_id,
                votes: archive.votes.clone(),
                tags: archive.tags.clone(),
                categories: archive.categories.clone(),
                favorites: archive.favorites.clone(),
                at: now_ms,
            },
            context,
        )
        .await?)
}
