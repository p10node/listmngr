//! `listmngr import21`: Mailman's `mailman import21`, a 2.1 `config.pck`
//! applied to an existing list.
use anyhow::{Context as _, Result};
use listmngr_core::ListId;
use listmngr_db::{AuditContext, Database};
use listmngr_import::{Config21, apply, plan};
use std::path::PathBuf;

pub async fn run(db: &Database, list_id: &str, path: &PathBuf, dry_run: bool) -> Result<()> {
    let list: ListId = list_id.parse()?;
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let config = Config21::from_pickle(&bytes).with_context(|| format!("{}", path.display()))?;
    let plan = plan(&config, &list);
    if dry_run {
        for warning in &plan.warnings {
            eprintln!("warning: {warning}");
        }
        println!("{}", plan.to_json());
        return Ok(());
    }
    let report = apply(db, &list, &plan, &AuditContext::system()).await?;
    for warning in &report.warnings {
        eprintln!("warning: {warning}");
    }
    println!("{}", report.to_json());
    Ok(())
}
