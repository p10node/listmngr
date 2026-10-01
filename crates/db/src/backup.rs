//! `listmngr backup <dir>` and `listmngr restore <dir>`: every table as
//! JSON lines, with a manifest naming the schema they came from.
//!
//! A backup is one read transaction over every table of the schema the
//! embedded migrator knows. The manifest records that ledger (the
//! version and checksum of each migration), the database and
//! message-store backends, and each table's columns and row count; the
//! rows go to `tables/<name>.jsonl`, one JSON array per row in column
//! order — `null`, booleans, integers, floats and strings as themselves,
//! bytes as `{"b64": …}`. `message_blobs` always carries the bytes: a
//! row the store keeps outside is read through the store, so a backup
//! is complete whatever the backend. A restore takes a database migrated
//! to the same ledger whose tables are empty (but for the rows a
//! migration seeds, which are replaced), inserts every table in the
//! manifest's order — parents before the tables that reference them,
//! the order the backup computed from its own foreign keys — binding
//! each value by the target column's type, and writes `message_blobs`
//! through the target's own store, all in one transaction.
use crate::blobs::BlobStore;
use crate::{Database, MIGRATOR, db_error};
use base64::Engine as _;
use futures::TryStreamExt as _;
use listmngr_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::any::{AnyArguments, AnyRow, AnyTypeInfoKind};
use sqlx::query::Query;
use sqlx::{Any, Column as _, Executor as _, Row as _, Transaction, ValueRef as _};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{BufRead as _, Write as _};
use std::path::Path;

/// The manifest format this binary writes and reads.
pub const FORMAT: u32 = 1;
const MANIFEST: &str = "manifest.json";
/// Tables a migration seeds on an empty database; a restore replaces
/// their rows with the backup's.
const SEEDED: &[&str] = &["list_styles", "subscription_rate"];
/// Binds per `INSERT` at most (`PostgreSQL` takes 65535).
const BINDS: usize = 2_000;

/// One applied migration, as the ledger records it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Migration {
    pub version: i64,
    pub description: String,
    /// Hex `SHA-384` of the migration's SQL, as `sqlx` stores it.
    pub checksum: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnEntry {
    pub name: String,
    /// The column's type as the source reported it (informational).
    pub kind: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableEntry {
    pub name: String,
    pub columns: Vec<ColumnEntry>,
    pub rows: u64,
    /// Relative to the backup directory.
    pub file: String,
}

/// `manifest.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub listmngr: String,
    pub created_at: String,
    /// `sqlite` or `postgres`.
    pub database: String,
    /// `db`, `fs` or `s3`.
    pub message_store: String,
    pub migrations: Vec<Migration>,
    /// In the order a restore inserts them.
    pub tables: Vec<TableEntry>,
}

impl Manifest {
    /// Rows over every table.
    #[must_use]
    pub fn rows(&self) -> u64 {
        self.tables.iter().map(|table| table.rows).sum()
    }
}

fn refused(message: impl std::fmt::Display) -> Error {
    Error::Validation(format!("backup: {message}"))
}

fn io(context: &str, error: &std::io::Error) -> Error {
    Error::Database(format!("backup: {context}: {error}"))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// The migrations this binary carries.
fn embedded() -> Vec<Migration> {
    MIGRATOR
        .iter()
        .filter(|migration| migration.migration_type.is_up_migration())
        .map(|migration| Migration {
            version: migration.version,
            description: migration.description.to_string(),
            checksum: hex(&migration.checksum),
        })
        .collect()
}

/// The migrations a database has applied.
async fn ledger(tx: &mut Transaction<'_, Any>) -> Result<Vec<Migration>> {
    let rows: Vec<(i64, String, Vec<u8>)> = sqlx::query_as(
        "SELECT version, description, checksum FROM _sqlx_migrations WHERE success ORDER BY version",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(db_error)?;
    Ok(rows
        .into_iter()
        .map(|(version, description, checksum)| Migration {
            version,
            description,
            checksum: hex(&checksum),
        })
        .collect())
}

/// Every base table of the schema but the migrator's own.
async fn tables(tx: &mut Transaction<'_, Any>, sqlite: bool) -> Result<BTreeSet<String>> {
    let sql = if sqlite {
        "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name"
    } else {
        "SELECT c.relname::text FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE c.relkind='r' AND n.nspname=current_schema() ORDER BY 1"
    };
    let names: Vec<String> = sqlx::query_scalar(sql)
        .fetch_all(&mut **tx)
        .await
        .map_err(db_error)?;
    Ok(names
        .into_iter()
        .filter(|name| !name.starts_with("_sqlx_") && !name.starts_with("sqlite_"))
        .collect())
}

/// `(child, parent)` for every foreign key between the tables.
async fn foreign_keys(
    tx: &mut Transaction<'_, Any>,
    sqlite: bool,
    tables: &BTreeSet<String>,
) -> Result<Vec<(String, String)>> {
    let mut edges = Vec::new();
    if sqlite {
        for table in tables {
            let parents: Vec<String> = sqlx::query_scalar(&format!(
                "SELECT \"table\" FROM pragma_foreign_key_list('{table}')"
            ))
            .fetch_all(&mut **tx)
            .await
            .map_err(db_error)?;
            edges.extend(parents.into_iter().map(|parent| (table.clone(), parent)));
        }
    } else {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT c.relname::text, p.relname::text FROM pg_constraint k JOIN pg_class c ON c.oid=k.conrelid JOIN pg_class p ON p.oid=k.confrelid JOIN pg_namespace n ON n.oid=c.relnamespace WHERE k.contype='f' AND n.nspname=current_schema()",
        )
        .fetch_all(&mut **tx)
        .await
        .map_err(db_error)?;
        edges = rows;
    }
    Ok(edges)
}

/// The tables with every parent before its children where the foreign
/// keys allow it; the tables of a cycle (`users` and `addresses` point at
/// each other, as in Mailman) follow in name order — a restore defers
/// every foreign key check to its commit anyway.
fn ordered(tables: &BTreeSet<String>, edges: &[(String, String)]) -> Vec<String> {
    let mut parents: BTreeMap<&str, BTreeSet<&str>> = tables
        .iter()
        .map(|table| (table.as_str(), BTreeSet::new()))
        .collect();
    for (child, parent) in edges {
        if child != parent && tables.contains(parent) {
            if let Some(set) = parents.get_mut(child.as_str()) {
                set.insert(parent.as_str());
            }
        }
    }
    let mut order = Vec::with_capacity(tables.len());
    let mut done: BTreeSet<&str> = BTreeSet::new();
    while done.len() < tables.len() {
        let ready: Vec<&str> = parents
            .iter()
            .filter(|(table, needs)| {
                !done.contains(*table) && needs.iter().all(|p| done.contains(p))
            })
            .map(|(table, _)| *table)
            .collect();
        if ready.is_empty() {
            let rest: Vec<&str> = parents
                .keys()
                .filter(|table| !done.contains(*table))
                .copied()
                .collect();
            for table in rest {
                done.insert(table);
                order.push(table.to_owned());
            }
            break;
        }
        for table in ready {
            done.insert(table);
            order.push(table.to_owned());
        }
    }
    order
}

/// Every foreign key of the schema made deferrable and deferred for the
/// rest of the transaction, so a cycle (`users` ↔ `addresses`) loads;
/// `(table, constraint)` of each, to put back before the commit.
async fn defer_foreign_keys(tx: &mut Transaction<'_, Any>) -> Result<Vec<(String, String)>> {
    let constraints: Vec<(String, String)> = sqlx::query_as(
        "SELECT c.relname::text, k.conname::text FROM pg_constraint k JOIN pg_class c ON c.oid=k.conrelid JOIN pg_namespace n ON n.oid=c.relnamespace WHERE k.contype='f' AND n.nspname=current_schema() AND NOT k.condeferrable ORDER BY 1, 2",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(db_error)?;
    for (table, constraint) in &constraints {
        sqlx::query(&format!(
            "ALTER TABLE \"{table}\" ALTER CONSTRAINT \"{constraint}\" DEFERRABLE INITIALLY DEFERRED"
        ))
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    }
    sqlx::query("SET CONSTRAINTS ALL DEFERRED")
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    Ok(constraints)
}

/// The deferred checks run now, then the constraints as they were.
async fn check_foreign_keys(
    tx: &mut Transaction<'_, Any>,
    constraints: &[(String, String)],
) -> Result<()> {
    sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    for (table, constraint) in constraints {
        sqlx::query(&format!(
            "ALTER TABLE \"{table}\" ALTER CONSTRAINT \"{constraint}\" NOT DEFERRABLE"
        ))
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    }
    Ok(())
}

const fn kind_name(kind: AnyTypeInfoKind) -> &'static str {
    match kind {
        AnyTypeInfoKind::Null => "null",
        AnyTypeInfoKind::Bool => "bool",
        AnyTypeInfoKind::SmallInt => "smallint",
        AnyTypeInfoKind::Integer => "integer",
        AnyTypeInfoKind::BigInt => "bigint",
        AnyTypeInfoKind::Real => "real",
        AnyTypeInfoKind::Double => "double",
        AnyTypeInfoKind::Text => "text",
        AnyTypeInfoKind::Blob => "blob",
    }
}

fn b64(bytes: &[u8]) -> Value {
    serde_json::json!({ "b64": base64::engine::general_purpose::STANDARD.encode(bytes) })
}

/// A JSON value for the row's `index`th column, typed by the value
/// itself (a `BYTEA` column's declared type is nothing to `SQLite`).
fn json_value(row: &AnyRow, index: usize) -> Result<Value> {
    let cell = row.try_get_raw(index).map_err(db_error)?;
    if cell.is_null() {
        return Ok(Value::Null);
    }
    let kind = cell.type_info().kind();
    Ok(match kind {
        AnyTypeInfoKind::Null => Value::Null,
        AnyTypeInfoKind::Bool => Value::Bool(row.try_get::<bool, _>(index).map_err(db_error)?),
        AnyTypeInfoKind::SmallInt | AnyTypeInfoKind::Integer | AnyTypeInfoKind::BigInt => {
            Value::from(row.try_get::<i64, _>(index).map_err(db_error)?)
        }
        AnyTypeInfoKind::Real | AnyTypeInfoKind::Double => {
            let number = row.try_get::<f64, _>(index).map_err(db_error)?;
            serde_json::Number::from_f64(number)
                .map(Value::Number)
                .ok_or_else(|| refused("a float JSON cannot carry"))?
        }
        AnyTypeInfoKind::Text => Value::String(row.try_get::<String, _>(index).map_err(db_error)?),
        AnyTypeInfoKind::Blob => b64(&row.try_get::<Vec<u8>, _>(index).map_err(db_error)?),
    })
}

/// Write `dir` from `db`: every table in foreign-key order, then the
/// manifest, last, so an interrupted backup has none.
/// # Errors
/// A directory that already holds a backup, a database whose ledger is
/// not this binary's, a store object a row names that is missing, and
/// the database's and the file system's own errors.
pub async fn backup(db: &Database, dir: &Path) -> Result<Manifest> {
    std::fs::create_dir_all(dir).map_err(|error| io("create_dir_all", &error))?;
    if dir.join(MANIFEST).exists() {
        return Err(refused(format!("{} already holds a backup", dir.display())));
    }
    std::fs::create_dir_all(dir.join("tables")).map_err(|error| io("create_dir_all", &error))?;
    let mut tx = db.pool().begin().await.map_err(db_error)?;
    if !db.is_sqlite() {
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
    }
    let migrations = ledger(&mut tx).await?;
    if migrations != embedded() {
        return Err(refused(
            "the database's migration ledger is not this binary's",
        ));
    }
    let present = tables(&mut tx, db.is_sqlite()).await?;
    let edges = foreign_keys(&mut tx, db.is_sqlite(), &present).await?;
    let mut entries = Vec::with_capacity(present.len());
    for name in ordered(&present, &edges) {
        let file = format!("tables/{name}.jsonl");
        let entry = dump_table(db, &mut tx, &name, &dir.join(&file), file).await?;
        entries.push(entry);
    }
    tx.commit().await.map_err(db_error)?;
    let manifest = Manifest {
        format: FORMAT,
        listmngr: env!("CARGO_PKG_VERSION").to_owned(),
        created_at: chrono::Utc::now().to_rfc3339(),
        database: if db.is_sqlite() { "sqlite" } else { "postgres" }.to_owned(),
        message_store: db.blobs().name().to_owned(),
        migrations,
        tables: entries,
    };
    let json = serde_json::to_vec_pretty(&manifest).map_err(refused)?;
    std::fs::write(dir.join(MANIFEST), json).map_err(|error| io("manifest", &error))?;
    Ok(manifest)
}

async fn dump_table(
    db: &Database,
    tx: &mut Transaction<'_, Any>,
    name: &str,
    path: &Path,
    file: String,
) -> Result<TableEntry> {
    let mut out =
        std::io::BufWriter::new(std::fs::File::create(path).map_err(|error| io("create", &error))?);
    let sql = format!("SELECT * FROM {name}");
    let mut columns: Option<Vec<ColumnEntry>> = None;
    let mut rows = 0;
    {
        let mut stream = sqlx::query(&sql).fetch(&mut **tx);
        while let Some(row) = stream.try_next().await.map_err(db_error)? {
            let names = columns.get_or_insert_with(|| {
                row.columns()
                    .iter()
                    .map(|column| ColumnEntry {
                        name: column.name().to_owned(),
                        kind: kind_name(column.type_info().kind()).to_owned(),
                    })
                    .collect()
            });
            let mut values = Vec::with_capacity(row.len());
            for index in 0..row.len() {
                values.push(json_value(&row, index)?);
            }
            if name == "message_blobs" {
                materialize(db, names, &mut values).await?;
            }
            serde_json::to_writer(&mut out, &values).map_err(refused)?;
            out.write_all(b"\n").map_err(|error| io("write", &error))?;
            rows += 1;
        }
    }
    let columns = match columns {
        Some(columns) => columns,
        None => (&mut **tx)
            .describe(&sql)
            .await
            .map(|described| {
                described
                    .columns()
                    .iter()
                    .map(|column| ColumnEntry {
                        name: column.name().to_owned(),
                        kind: kind_name(column.type_info().kind()).to_owned(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    };
    out.flush().map_err(|error| io("flush", &error))?;
    Ok(TableEntry {
        name: name.to_owned(),
        columns,
        rows,
        file,
    })
}

/// A `message_blobs` row whose bytes the store keeps outside gets them
/// back, so the backup carries every message whatever the backend.
async fn materialize(db: &Database, columns: &[ColumnEntry], values: &mut [Value]) -> Result<()> {
    let index = |wanted: &str| columns.iter().position(|column| column.name == wanted);
    let (Some(key), Some(raw)) = (index("store_key"), index("raw")) else {
        return Ok(());
    };
    let empty = match &values[raw] {
        Value::Object(object) => object.get("b64").and_then(Value::as_str) == Some(""),
        Value::Null => true,
        _ => false,
    };
    if !empty {
        return Ok(());
    }
    let Some(key) = values[key].as_str() else {
        return Ok(());
    };
    let bytes = db.blobs().object(key).await?.ok_or_else(|| {
        refused(format!(
            "the {} store has no object for {key}",
            db.blobs().name()
        ))
    })?;
    values[raw] = b64(&bytes);
    Ok(())
}

/// Read `dir` into `db`, in one transaction.
/// # Errors
/// A missing or foreign manifest, a database not migrated to the
/// backup's ledger, a table that is not empty (the seeded ones aside),
/// a row that does not fit its table, and the database's and the file
/// system's own errors.
pub async fn restore(db: &Database, dir: &Path) -> Result<Manifest> {
    let manifest: Manifest = serde_json::from_slice(
        &std::fs::read(dir.join(MANIFEST)).map_err(|error| io("manifest", &error))?,
    )
    .map_err(|error| refused(format!("manifest: {error}")))?;
    if manifest.format != FORMAT {
        return Err(refused(format!(
            "manifest format {} is not {FORMAT}",
            manifest.format
        )));
    }
    if manifest.migrations != embedded() {
        return Err(refused(
            "the backup is of a schema this binary does not know",
        ));
    }
    let mut tx = db.write_tx().await?;
    if ledger(&mut tx).await? != manifest.migrations {
        return Err(refused(
            "the database is not migrated to the backup's ledger",
        ));
    }
    let present = tables(&mut tx, db.is_sqlite()).await?;
    for table in &manifest.tables {
        if !present.contains(&table.name) {
            return Err(refused(format!("the database has no table {}", table.name)));
        }
    }
    for table in manifest.tables.iter().rev() {
        let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {}", table.name))
            .fetch_one(&mut *tx)
            .await
            .map_err(db_error)?;
        if count == 0 {
            continue;
        }
        if SEEDED.contains(&table.name.as_str()) {
            sqlx::query(&format!("DELETE FROM {}", table.name))
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
        } else {
            return Err(refused(format!("{} is not empty", table.name)));
        }
    }
    let deferred = if db.is_sqlite() {
        sqlx::query("PRAGMA defer_foreign_keys = ON")
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        Vec::new()
    } else {
        defer_foreign_keys(&mut tx).await?
    };
    for table in &manifest.tables {
        load_table(db, &mut tx, table, &dir.join(&table.file)).await?;
    }
    if !db.is_sqlite() {
        check_foreign_keys(&mut tx, &deferred).await?;
    }
    tx.commit().await.map_err(db_error)?;
    Ok(manifest)
}

async fn load_table(
    db: &Database,
    tx: &mut Transaction<'_, Any>,
    table: &TableEntry,
    path: &Path,
) -> Result<u64> {
    if table.rows == 0 {
        return Ok(0);
    }
    if table.columns.is_empty() {
        return Err(refused(format!("{} has rows but no columns", table.name)));
    }
    let described = (&mut **tx)
        .describe(&format!("SELECT * FROM {}", table.name))
        .await
        .map_err(db_error)?;
    let kinds: HashMap<String, AnyTypeInfoKind> = described
        .columns()
        .iter()
        .map(|column| (column.name().to_owned(), column.type_info().kind()))
        .collect();
    let mut targets = Vec::with_capacity(table.columns.len());
    for column in &table.columns {
        targets.push(
            *kinds
                .get(&column.name)
                .ok_or_else(|| refused(format!("{} has no column {}", table.name, column.name)))?,
        );
    }
    let names: Vec<&str> = table
        .columns
        .iter()
        .map(|column| column.name.as_str())
        .collect();
    let per_statement = (BINDS / names.len()).clamp(1, 100);
    let file = std::io::BufReader::new(
        std::fs::File::open(path).map_err(|error| io(&table.file, &error))?,
    );
    let mut batch: Vec<Vec<Value>> = Vec::with_capacity(per_statement);
    let mut inserted = 0;
    for line in file.lines() {
        let line = line.map_err(|error| io(&table.file, &error))?;
        if line.trim().is_empty() {
            continue;
        }
        let values: Vec<Value> = serde_json::from_str(&line)
            .map_err(|error| refused(format!("{}: {error}", table.file)))?;
        if values.len() != names.len() {
            return Err(refused(format!(
                "{}: a row of {} values for {} columns",
                table.file,
                values.len(),
                names.len()
            )));
        }
        if table.name == "message_blobs" {
            insert_blob(db, tx, &names, &values).await?;
            inserted += 1;
            continue;
        }
        batch.push(values);
        if batch.len() == per_statement {
            insert(tx, &table.name, &names, &targets, &batch).await?;
            inserted += batch.len() as u64;
            batch.clear();
        }
    }
    if !batch.is_empty() {
        insert(tx, &table.name, &names, &targets, &batch).await?;
        inserted += batch.len() as u64;
    }
    if inserted != table.rows {
        return Err(refused(format!(
            "{}: {} rows where the manifest says {}",
            table.file, inserted, table.rows
        )));
    }
    Ok(inserted)
}

/// One `INSERT` for `batch`.
async fn insert(
    tx: &mut Transaction<'_, Any>,
    table: &str,
    names: &[&str],
    targets: &[AnyTypeInfoKind],
    batch: &[Vec<Value>],
) -> Result<()> {
    let mut sql = format!("INSERT INTO {table} ({}) VALUES ", names.join(","));
    let mut placeholder = 1;
    for (row, _) in batch.iter().enumerate() {
        if row > 0 {
            sql.push(',');
        }
        sql.push('(');
        for column in 0..names.len() {
            if column > 0 {
                sql.push(',');
            }
            sql.push('$');
            sql.push_str(&placeholder.to_string());
            placeholder += 1;
        }
        sql.push(')');
    }
    let mut query = sqlx::query(&sql);
    for row in batch {
        for (value, kind) in row.iter().zip(targets) {
            query = bind(query, value, *kind)?;
        }
    }
    query.execute(&mut **tx).await.map_err(db_error)?;
    Ok(())
}

/// `value` bound as the target column's type; a target the driver does
/// not type (`SQLite`'s `BYTEA`) takes the value as it is.
fn bind<'q>(
    query: Query<'q, Any, AnyArguments<'q>>,
    value: &Value,
    kind: AnyTypeInfoKind,
) -> Result<Query<'q, Any, AnyArguments<'q>>> {
    use AnyTypeInfoKind as K;
    let mismatch = || {
        refused(format!(
            "a {} value for a {} column",
            describe_value(value),
            kind_name(kind)
        ))
    };
    Ok(match (value, kind) {
        (Value::Null, K::Blob) => query.bind(None::<Vec<u8>>),
        (Value::Null, K::Bool) => query.bind(None::<bool>),
        (Value::Null, K::SmallInt | K::Integer | K::BigInt) => query.bind(None::<i64>),
        (Value::Null, K::Real | K::Double) => query.bind(None::<f64>),
        (Value::Null, K::Text | K::Null) => query.bind(None::<String>),
        (Value::Bool(flag), K::Bool) => query.bind(*flag),
        (Value::Bool(flag), K::SmallInt | K::Integer | K::BigInt | K::Null) => {
            query.bind(i64::from(*flag))
        }
        (Value::Number(number), K::Bool) => match number.as_i64() {
            Some(0) => query.bind(false),
            Some(1) => query.bind(true),
            _ => return Err(mismatch()),
        },
        (Value::Number(number), K::Real | K::Double) => {
            query.bind(number.as_f64().ok_or_else(mismatch)?)
        }
        (Value::Number(number), K::SmallInt | K::Integer | K::BigInt | K::Null) => {
            match number.as_i64() {
                Some(integer) => query.bind(integer),
                None if kind == K::Null => query.bind(number.as_f64().ok_or_else(mismatch)?),
                None => return Err(mismatch()),
            }
        }
        (Value::String(text), K::Text | K::Null) => query.bind(text.clone()),
        (Value::Object(object), K::Blob | K::Null) => {
            let encoded = object
                .get("b64")
                .and_then(Value::as_str)
                .ok_or_else(mismatch)?;
            query.bind(
                base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .map_err(|_| mismatch())?,
            )
        }
        _ => return Err(mismatch()),
    })
}

const fn describe_value(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "bytes",
    }
}

/// A `message_blobs` row written through the target's store: the bytes
/// go where the store keeps them and the row says so.
async fn insert_blob(
    db: &Database,
    tx: &mut Transaction<'_, Any>,
    names: &[&str],
    values: &[Value],
) -> Result<()> {
    let index = |wanted: &str| names.iter().position(|name| *name == wanted);
    let (Some(key), Some(raw)) = (index("store_key"), index("raw")) else {
        return Err(refused("message_blobs without store_key and raw"));
    };
    let key = values[key]
        .as_str()
        .ok_or_else(|| refused("message_blobs: a store_key that is not text"))?;
    let bytes = match &values[raw] {
        Value::Object(object) => object
            .get("b64")
            .and_then(Value::as_str)
            .map(|encoded| base64::engine::general_purpose::STANDARD.decode(encoded))
            .transpose()
            .map_err(|_| refused("message_blobs: raw is not base64"))?,
        Value::Null => None,
        _ => return Err(refused("message_blobs: raw is not bytes")),
    }
    .unwrap_or_default();
    if BlobStore::key(&bytes) == key {
        db.blobs().put_tx(tx, &bytes).await?;
    } else {
        sqlx::query("INSERT INTO message_blobs(store_key,raw) VALUES($1,$2)")
            .bind(key)
            .bind(bytes)
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_ordered_parents_first() {
        let tables: BTreeSet<String> = ["members", "lists", "domains", "audit_log"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let edges = vec![
            ("members".to_owned(), "lists".to_owned()),
            ("lists".to_owned(), "domains".to_owned()),
            ("members".to_owned(), "members".to_owned()),
        ];
        assert_eq!(
            ordered(&tables, &edges),
            vec!["audit_log", "domains", "lists", "members"]
        );
        let cycle = vec![
            ("lists".to_owned(), "members".to_owned()),
            ("members".to_owned(), "lists".to_owned()),
            ("lists".to_owned(), "domains".to_owned()),
        ];
        assert_eq!(
            ordered(&tables, &cycle),
            vec!["audit_log", "domains", "lists", "members"],
            "the cycle's tables follow in name order"
        );
    }

    #[test]
    fn the_embedded_ledger_is_hex_checksums() {
        let migrations = embedded();
        assert!(!migrations.is_empty());
        assert!(migrations.iter().all(|m| m.checksum.len() == 96));
        assert!(
            migrations
                .windows(2)
                .all(|pair| pair[0].version < pair[1].version)
        );
    }
}
