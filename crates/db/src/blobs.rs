//! The message store: where the bytes behind `message_blobs.store_key`
//! live.
//!
//! `db` keeps them in the row; `fs` under `path/<aa>/<bb>/<key>`; `s3`
//! as `<prefix><key>` in a bucket. The key is the SHA-256 of the bytes,
//! so the same message stored twice is one object and one row, and
//! writing an object again is harmless. With `fs` and `s3` the row stays
//! as the reference (its `raw` empty) so every foreign key, sweep and
//! count works as before, and a row that still holds bytes — a store
//! switched after the fact — is read as it is until `migrate_rows` moves
//! them out. An object is written before its row and never deleted
//! inline: a transaction rolled back after the write leaves an orphan,
//! and the task sweep collects orphans older than [`ORPHAN_GRACE_MS`],
//! so a committed row never loses its bytes and a rolled-back one leaves
//! nothing for long.
mod s3;
pub mod sigv4;

use crate::{Database, db_error};
use listmngr_core::{Error, MessageStoreConfig, Result};
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use sqlx::{Any, Row as _, Transaction};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use s3::S3Settings;

/// Objects younger than this are never collected: the transaction that
/// wrote them may still be open.
pub const ORPHAN_GRACE_MS: i64 = 3_600_000;

/// Keys looked up per statement by the sweep and `check`.
const CHUNK: usize = 100;

/// Rows `migrate_rows` and `check` take per round.
const BATCH: i64 = 200;

#[derive(Clone, Debug)]
enum Backend {
    Db,
    Fs(PathBuf),
    S3(Arc<s3::S3>),
}

/// One store, cheap to clone, shared by every repository of a
/// [`Database`].
#[derive(Clone, Debug)]
pub struct BlobStore(Backend);

/// What [`BlobStore::check`] found.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CheckReport {
    /// Rows in `message_blobs`.
    pub rows: u64,
    /// Rows still holding their bytes.
    pub in_rows: u64,
    /// Rows whose object the store has.
    pub in_store: u64,
    /// Keys whose bytes are neither in the row nor in the store.
    pub missing: Vec<String>,
}

fn storage(message: impl std::fmt::Display) -> Error {
    Error::Database(format!("message store: {message}"))
}

/// Whether `key` is one of ours: 64 lowercase hex digits.
fn is_key(key: &str) -> bool {
    key.len() == 64
        && key
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn object_path(root: &Path, key: &str) -> PathBuf {
    root.join(&key[..2]).join(&key[2..4]).join(key)
}

impl BlobStore {
    /// The bytes stay in `message_blobs.raw`.
    #[must_use]
    pub const fn db() -> Self {
        Self(Backend::Db)
    }

    /// The bytes go under `root/<aa>/<bb>/<key>`, owner-only.
    #[must_use]
    pub fn fs(root: impl Into<PathBuf>) -> Self {
        Self(Backend::Fs(root.into()))
    }

    /// The bytes go to a bucket.
    /// # Errors
    /// An endpoint that is not `http(s)://host[:port]`.
    pub fn s3(settings: S3Settings) -> Result<Self> {
        Ok(Self(Backend::S3(Arc::new(s3::S3::new(settings)?))))
    }

    /// `[message_store]` as `Config::load` validated it.
    /// # Errors
    /// A backend `Config::load` would have refused.
    pub fn from_config(config: &MessageStoreConfig) -> Result<Self> {
        match config.backend.as_str() {
            "db" => Ok(Self::db()),
            "fs" => Ok(Self::fs(config.path.trim())),
            "s3" => Self::s3(S3Settings {
                bucket: config.s3_bucket.clone().unwrap_or_default(),
                region: config.s3_region.clone(),
                endpoint: config.s3_endpoint.clone(),
                prefix: config.s3_prefix.clone(),
                credentials: sigv4::Credentials {
                    access_key_id: config.s3_access_key_id.clone().unwrap_or_default(),
                    secret_access_key: config.s3_secret_access_key().unwrap_or_default().to_owned(),
                },
            }),
            other => Err(Error::Validation(format!(
                "message_store.backend must be db, fs or s3, not {other:?}"
            ))),
        }
    }

    /// `db`, `fs` or `s3`.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self.0 {
            Backend::Db => "db",
            Backend::Fs(_) => "fs",
            Backend::S3(_) => "s3",
        }
    }

    /// The key `raw` is stored under: its SHA-256, hex.
    #[must_use]
    pub fn key(raw: &[u8]) -> String {
        format!("{:x}", Sha256::digest(raw))
    }

    /// Store `raw` under its key: the object first (for `fs` and `s3`),
    /// then the row inside `tx`. Storing the same bytes again changes
    /// nothing, except that a row the `db` store finds emptied gets its
    /// bytes back.
    pub(crate) async fn put_tx(&self, tx: &mut Transaction<'_, Any>, raw: &[u8]) -> Result<String> {
        let key = Self::key(raw);
        let (stored, sql): (&[u8], &str) = match &self.0 {
            Backend::Db => (
                raw,
                "INSERT INTO message_blobs(store_key,raw) VALUES($1,$2) ON CONFLICT(store_key) DO UPDATE SET raw=excluded.raw WHERE length(message_blobs.raw)=0",
            ),
            Backend::Fs(root) => {
                write_object(root, &key, raw).await?;
                (
                    &[],
                    "INSERT INTO message_blobs(store_key,raw) VALUES($1,$2) ON CONFLICT(store_key) DO NOTHING",
                )
            }
            Backend::S3(s3) => {
                s3.put(&key, raw).await?;
                (
                    &[],
                    "INSERT INTO message_blobs(store_key,raw) VALUES($1,$2) ON CONFLICT(store_key) DO NOTHING",
                )
            }
        };
        sqlx::query(sql)
            .bind(&key)
            .bind(stored)
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
        Ok(key)
    }

    /// The bytes under `key`: the row's when it holds them, the store's
    /// otherwise.
    /// # Errors
    /// `NotFound` for a key no row names; a database error for an object
    /// the store should have and has not.
    pub(crate) async fn get<'e, E>(&self, executor: E, key: &str) -> Result<Vec<u8>>
    where
        E: sqlx::Executor<'e, Database = Any>,
    {
        let row: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT raw FROM message_blobs WHERE store_key=$1")
                .bind(key)
                .fetch_optional(executor)
                .await
                .map_err(db_error)?;
        let row = row.ok_or_else(|| Error::NotFound("message".into()))?;
        if !row.is_empty() {
            return Ok(row);
        }
        self.object(key)
            .await?
            .ok_or_else(|| storage(format!("{} has no object for {key}", self.name())))
    }

    /// The store's own copy, `None` when it has none (always for `db`).
    async fn object(&self, key: &str) -> Result<Option<Vec<u8>>> {
        if !is_key(key) {
            return Ok(None);
        }
        match &self.0 {
            Backend::Db => Ok(None),
            Backend::Fs(root) => read_object(root, key).await,
            Backend::S3(s3) => s3.get(key).await,
        }
    }

    async fn has_object(&self, key: &str) -> Result<bool> {
        if !is_key(key) {
            return Ok(false);
        }
        match &self.0 {
            Backend::Db => Ok(false),
            Backend::Fs(root) => Ok(object_path(root, key).is_file()),
            Backend::S3(s3) => s3.exists(key).await,
        }
    }

    async fn remove_object(&self, key: &str) -> Result<()> {
        match &self.0 {
            Backend::Db => Ok(()),
            Backend::Fs(root) => remove_object(root, key).await,
            Backend::S3(s3) => s3.delete(key).await,
        }
    }

    /// Objects no row names, older than [`ORPHAN_GRACE_MS`] at `now_ms`,
    /// removed; how many. Only objects that look like keys are
    /// considered — anything else in the directory or under the prefix
    /// is somebody else's.
    /// # Errors
    /// The store's and the database's own errors; objects already
    /// removed stay removed.
    pub async fn sweep_orphans(&self, db: &Database, now_ms: i64) -> Result<u64> {
        let before = now_ms.saturating_sub(ORPHAN_GRACE_MS);
        let candidates: Vec<String> = match &self.0 {
            Backend::Db => return Ok(0),
            Backend::Fs(root) => old_objects(root, before).await?,
            Backend::S3(s3) => s3
                .list()
                .await?
                .into_iter()
                .filter(|(key, at)| is_key(key) && at.timestamp_millis() <= before)
                .map(|(key, _)| key)
                .collect(),
        };
        let mut removed = 0;
        for chunk in candidates.chunks(CHUNK) {
            let referenced = referenced(db, chunk).await?;
            for key in chunk {
                if !referenced.contains(key) {
                    self.remove_object(key).await?;
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }

    /// Move the bytes rows still hold into this store, row by row: the
    /// object is written first, the row emptied after; how many.
    /// # Errors
    /// The store's and the database's own errors; rows already moved
    /// stay moved.
    pub async fn migrate_rows(&self, db: &Database) -> Result<u64> {
        if matches!(self.0, Backend::Db) {
            return Ok(0);
        }
        let mut moved = 0;
        loop {
            let rows = sqlx::query(
                "SELECT store_key, raw FROM message_blobs WHERE length(raw)>0 ORDER BY store_key LIMIT $1",
            )
            .bind(BATCH)
            .fetch_all(db.pool())
            .await
            .map_err(db_error)?;
            if rows.is_empty() {
                return Ok(moved);
            }
            for row in &rows {
                let key: String = row.try_get("store_key").map_err(db_error)?;
                let raw: Vec<u8> = row.try_get("raw").map_err(db_error)?;
                if !is_key(&key) {
                    return Err(storage(format!("{key:?} is not a key this store can hold")));
                }
                match &self.0 {
                    Backend::Db => {}
                    Backend::Fs(root) => write_object(root, &key, &raw).await?,
                    Backend::S3(s3) => s3.put(&key, &raw).await?,
                }
                sqlx::query("UPDATE message_blobs SET raw=$1 WHERE store_key=$2")
                    .bind(Vec::<u8>::new())
                    .bind(&key)
                    .execute(db.pool())
                    .await
                    .map_err(db_error)?;
                moved += 1;
            }
        }
    }

    /// Every row: whether its bytes are in the row, in the store, or
    /// nowhere.
    /// # Errors
    /// The store's and the database's own errors.
    pub async fn check(&self, db: &Database) -> Result<CheckReport> {
        let mut report = CheckReport::default();
        let mut after = String::new();
        loop {
            let rows: Vec<(String, i64)> = sqlx::query_as(
                "SELECT store_key, length(raw) FROM message_blobs WHERE store_key>$1 ORDER BY store_key LIMIT $2",
            )
            .bind(&after)
            .bind(BATCH)
            .fetch_all(db.pool())
            .await
            .map_err(db_error)?;
            let Some((last, _)) = rows.last() else {
                return Ok(report);
            };
            after.clone_from(last);
            for (key, held) in &rows {
                report.rows += 1;
                if *held > 0 {
                    report.in_rows += 1;
                } else if self.has_object(key).await? {
                    report.in_store += 1;
                } else {
                    report.missing.push(key.clone());
                }
            }
        }
    }
}

/// Which of `keys` a row names.
async fn referenced(db: &Database, keys: &[String]) -> Result<HashSet<String>> {
    if keys.is_empty() {
        return Ok(HashSet::new());
    }
    let placeholders = (1..=keys.len())
        .map(|n| format!("${n}"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("SELECT store_key FROM message_blobs WHERE store_key IN ({placeholders})");
    let mut query = sqlx::query_scalar::<_, String>(&sql);
    for key in keys {
        query = query.bind(key);
    }
    query
        .fetch_all(db.pool())
        .await
        .map(|rows| rows.into_iter().collect())
        .map_err(db_error)
}

fn io(context: &str, error: &std::io::Error) -> Error {
    storage(format!("{context}: {error}"))
}

/// Write `raw` at `root/<aa>/<bb>/<key>` unless it is there already:
/// into a temporary file beside it, owner-only, then renamed into place,
/// so a reader never sees a partial object.
async fn write_object(root: &Path, key: &str, raw: &[u8]) -> Result<()> {
    let path = object_path(root, key);
    let raw = raw.to_vec();
    tokio::task::spawn_blocking(move || -> Result<()> {
        if path.is_file() {
            return Ok(());
        }
        let parent = path
            .parent()
            .ok_or_else(|| storage("an object path without a parent"))?;
        std::fs::create_dir_all(parent).map_err(|error| io("fs create_dir_all", &error))?;
        let temporary = parent.join(format!(
            ".{}.{}",
            path.file_name().unwrap_or_default().to_string_lossy(),
            uuid::Uuid::now_v7().simple()
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let written = (|| {
            use std::io::Write as _;
            let mut file = options.open(&temporary)?;
            file.write_all(&raw)?;
            file.sync_all()?;
            std::fs::rename(&temporary, &path)
        })();
        if let Err(error) = written {
            let _ = std::fs::remove_file(&temporary);
            return Err(io("fs write", &error));
        }
        Ok(())
    })
    .await
    .map_err(|error| storage(format!("fs write: {error}")))?
}

async fn read_object(root: &Path, key: &str) -> Result<Option<Vec<u8>>> {
    let path = object_path(root, key);
    tokio::task::spawn_blocking(move || match std::fs::read(&path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io("fs read", &error)),
    })
    .await
    .map_err(|error| storage(format!("fs read: {error}")))?
}

async fn remove_object(root: &Path, key: &str) -> Result<()> {
    let path = object_path(root, key);
    tokio::task::spawn_blocking(move || match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io("fs remove", &error)),
    })
    .await
    .map_err(|error| storage(format!("fs remove: {error}")))?
}

/// The keys of every object under `root` whose file was last modified
/// at or before `before_ms`: `<aa>/<bb>/<key>` with the key's first four
/// digits agreeing with the directories, nothing else.
async fn old_objects(root: &Path, before_ms: i64) -> Result<Vec<String>> {
    let root = root.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<Vec<String>> {
        let mut keys = Vec::new();
        let Ok(first) = std::fs::read_dir(&root) else {
            return Ok(keys);
        };
        for aa in first.flatten() {
            let Ok(second) = std::fs::read_dir(aa.path()) else {
                continue;
            };
            for bb in second.flatten() {
                let Ok(files) = std::fs::read_dir(bb.path()) else {
                    continue;
                };
                for file in files.flatten() {
                    let name = file.file_name().to_string_lossy().into_owned();
                    let placed = is_key(&name)
                        && aa.file_name().to_string_lossy() == name[..2]
                        && bb.file_name().to_string_lossy() == name[2..4];
                    if !placed {
                        continue;
                    }
                    let Ok(metadata) = file.metadata() else {
                        continue;
                    };
                    let modified = metadata
                        .modified()
                        .ok()
                        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
                        .and_then(|age| i64::try_from(age.as_millis()).ok());
                    if metadata.is_file() && modified.is_some_and(|at| at <= before_ms) {
                        keys.push(name);
                    }
                }
            }
        }
        keys.sort();
        Ok(keys)
    })
    .await
    .map_err(|error| storage(format!("fs list: {error}")))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_sha256_hex() {
        let key = BlobStore::key(b"");
        assert_eq!(key, sigv4::EMPTY_SHA256);
        assert!(is_key(&key));
        assert!(!is_key("not-a-key"));
        assert!(!is_key(&key.to_ascii_uppercase()));
        assert_eq!(
            object_path(Path::new("/var/lib/listmngr/messages"), &key),
            Path::new("/var/lib/listmngr/messages/e3/b0").join(&key)
        );
    }

    #[test]
    fn the_configuration_picks_the_backend() {
        let mut config = MessageStoreConfig::default();
        assert_eq!(BlobStore::from_config(&config).unwrap().name(), "db");
        config.backend = "fs".into();
        assert_eq!(BlobStore::from_config(&config).unwrap().name(), "fs");
        config.backend = "tape".into();
        assert!(BlobStore::from_config(&config).is_err());
    }
}
