//! `listmngr message-store`: the store behind `message_blobs` from the
//! command line — move the bytes rows still hold into the configured
//! `fs` or `s3` store, and check that every row's bytes are somewhere.
use anyhow::Result;
use clap::Subcommand;
use listmngr_db::Database;

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Move the bytes rows still hold into the configured `fs` or `s3`
    /// store; each row is emptied only after its object is written. With
    /// the `db` backend there is nothing to move.
    Migrate,
    /// Count the rows and name every row whose bytes are neither in the
    /// row nor in the store.
    Check,
}

pub async fn run(db: &Database, command: Command) -> Result<()> {
    let store = db.blobs();
    match command {
        Command::Migrate => {
            let moved = store.migrate_rows(db).await?;
            println!(
                "{}",
                serde_json::json!({"backend": store.name(), "moved": moved})
            );
        }
        Command::Check => {
            let report = store.check(db).await?;
            let ok = report.missing.is_empty();
            println!(
                "{}",
                serde_json::json!({"backend": store.name(), "ok": ok, "rows": report.rows, "in_rows": report.in_rows, "in_store": report.in_store, "missing": report.missing})
            );
            if !ok {
                anyhow::bail!("{} rows have no bytes anywhere", report.missing.len());
            }
        }
    }
    Ok(())
}
