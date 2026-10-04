//! Read-only operator inspection. Never calls the migrator's write methods.
use crate::{MIGRATOR, install_drivers};
use sqlx::{ConnectOptions, Connection, migrate::Migrate};
use std::str::FromStr;

/// What the read-only inspection found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inspection {
    /// At most 1000 mail domains.
    pub domains: Vec<String>,
    /// TOTP secrets sealed under a master key, and in the clear.
    pub totp_sealed: u64,
    pub totp_plain: u64,
}

/// Check the embedded migration ledger, enumerate at most 1000 mail domains
/// and count the sealed and plain TOTP secrets.
/// The caller must impose an overall deadline. No raw database error escapes.
///
/// # Errors
/// Returns a static diagnostic code for connection/schema/inventory failures.
pub async fn inspect(url: &str) -> Result<Inspection, &'static str> {
    install_drivers();
    let sqlite = url.starts_with("sqlite:");
    let url = if sqlite {
        sqlx::sqlite::SqliteConnectOptions::from_str(url)
            .map_err(|_| "database_unavailable")?
            .create_if_missing(false)
            .read_only(true)
            .to_url_lossy()
            .to_string()
    } else if url.starts_with("postgres:") || url.starts_with("postgresql:") {
        url.to_owned()
    } else {
        return Err("database_unavailable");
    };
    let options = sqlx::any::AnyConnectOptions::from_str(&url)
        .map_err(|_| "database_unavailable")?
        .disable_statement_logging();
    let mut connection = sqlx::AnyConnection::connect_with(&options)
        .await
        .map_err(|_| "database_unavailable")?;
    sqlx::query(if sqlite { "BEGIN" } else { "BEGIN READ ONLY" })
        .execute(&mut connection)
        .await
        .map_err(|_| "database_unavailable")?;
    let result = inspect_connection(&mut connection).await;
    let _ = sqlx::query("ROLLBACK").execute(&mut connection).await;
    let _ = connection.close().await;
    result
}

async fn inspect_connection(
    connection: &mut sqlx::AnyConnection,
) -> Result<Inspection, &'static str> {
    let dirty = connection
        .dirty_version()
        .await
        .map_err(|_| "schema_unavailable")?;
    if dirty.is_some() {
        return Err("schema_dirty");
    }
    let applied = connection
        .list_applied_migrations()
        .await
        .map_err(|_| "schema_unavailable")?;
    let expected: Vec<_> = MIGRATOR
        .iter()
        .filter(|migration| migration.migration_type.is_up_migration())
        .collect();
    if applied.len() != expected.len()
        || expected.iter().any(|migration| {
            !applied
                .iter()
                .any(|row| row.version == migration.version && row.checksum == migration.checksum)
        })
    {
        return Err("schema_mismatch");
    }
    let domains: Vec<String> =
        sqlx::query_scalar("SELECT mail_host FROM domains ORDER BY mail_host LIMIT 1001")
            .fetch_all(&mut *connection)
            .await
            .map_err(|_| "schema_unavailable")?;
    if domains.len() > 1000 {
        return Err("domain_limit_exceeded");
    }
    let (sealed, plain): (i64, i64) = sqlx::query_as(
        "SELECT COALESCE(SUM(CASE WHEN secret LIKE 'v1:%' THEN 1 ELSE 0 END),0), COALESCE(SUM(CASE WHEN secret LIKE 'v1:%' THEN 0 ELSE 1 END),0) FROM user_totp",
    )
    .fetch_one(&mut *connection)
    .await
    .map_err(|_| "schema_unavailable")?;
    Ok(Inspection {
        domains,
        totp_sealed: u64::try_from(sealed).unwrap_or(0),
        totp_plain: u64::try_from(plain).unwrap_or(0),
    })
}
