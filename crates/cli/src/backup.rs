//! `listmngr backup <dir>` and `listmngr restore <dir>`: every table as
//! JSON lines with a manifest, and the way back into an empty database
//! migrated to the same schema, on either backend.
use anyhow::Result;
use listmngr_db::Database;
use std::path::Path;

pub async fn backup(db: &Database, dir: &Path) -> Result<()> {
    let manifest = listmngr_db::backup::backup(db, dir).await?;
    println!(
        "{}",
        serde_json::json!({
            "dir": dir,
            "database": manifest.database,
            "message_store": manifest.message_store,
            "migrations": manifest.migrations.len(),
            "tables": manifest.tables.len(),
            "rows": manifest.rows(),
        })
    );
    Ok(())
}

pub async fn restore(db: &Database, dir: &Path) -> Result<()> {
    let manifest = listmngr_db::backup::restore(db, dir).await?;
    println!(
        "{}",
        serde_json::json!({
            "dir": dir,
            "from": manifest.database,
            "tables": manifest.tables.len(),
            "rows": manifest.rows(),
            "message_store": db.blobs().name(),
        })
    );
    Ok(())
}
