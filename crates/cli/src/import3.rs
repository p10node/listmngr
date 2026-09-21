//! `listmngr import3`: a Mailman 3 site read over its REST API and
//! written here.
use anyhow::{Context as _, Result};
use listmngr_core::ListId;
use listmngr_db::{AuditContext, Database};
use listmngr_import::import3::{apply, fetch, plan};
use listmngr_import::rest3::Rest;
use std::path::PathBuf;

#[derive(Debug, clap::Args)]
pub struct Options {
    /// The core's REST root, e.g. `http://127.0.0.1:8001/3.1`.
    #[arg(long, value_name = "URL")]
    pub rest: String,
    /// The `[webservice] admin_user` of that core.
    #[arg(long, default_value = "restadmin")]
    pub user: String,
    /// A file holding its `admin_pass`; the password is never taken on
    /// the command line, where it would land in the shell history.
    #[arg(long, value_name = "FILE")]
    pub password_file: PathBuf,
    /// Import this list only (its list id), instead of the whole site.
    #[arg(long, value_name = "LIST_ID")]
    pub list: Option<String>,
    /// Print the plan as JSON and change nothing.
    #[arg(long)]
    pub dry_run: bool,
}

pub async fn run(db: &Database, options: Options) -> Result<()> {
    let password = std::fs::read_to_string(&options.password_file)
        .with_context(|| format!("reading {}", options.password_file.display()))?;
    let rest = Rest::new(&options.rest, &options.user, password.trim())?;
    let only: Option<ListId> = options.list.as_deref().map(str::parse).transpose()?;
    let site = fetch(&rest, only.as_ref()).await?;
    let plan = plan(&site);
    if options.dry_run {
        for warning in &plan.warnings {
            eprintln!("warning: {warning}");
        }
        println!("{}", plan.to_json());
        return Ok(());
    }
    let report = apply(db, &plan, &AuditContext::system()).await?;
    for warning in &report.warnings {
        eprintln!("warning: {warning}");
    }
    println!("{}", report.to_json());
    Ok(())
}
