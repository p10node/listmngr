//! `listmngr import3`: a Mailman 3 site read over its REST API, or
//! straight from its database and message store, and written here; with
//! `--hyperkitty`, what readers left on its `HyperKitty` archive too.
use anyhow::{Context as _, Result};
use listmngr_core::ListId;
use listmngr_db::{AuditContext, Database};
use listmngr_import::db3::fetch_db;
use listmngr_import::hyperkitty;
use listmngr_import::import3::{Site, apply, fetch, plan};
use listmngr_import::rest3::Rest;
use std::path::PathBuf;

#[derive(Debug, clap::Args)]
#[command(group(
    clap::ArgGroup::new("source")
        .required(true)
        .multiple(true)
        .args(["rest", "db", "hyperkitty"])
))]
pub struct Options {
    /// The core's REST root, e.g. `http://127.0.0.1:8001/3.1`.
    #[arg(long, value_name = "URL", requires = "password_file")]
    pub rest: Option<String>,
    /// The `[webservice] admin_user` of that core.
    #[arg(long, default_value = "restadmin")]
    pub user: String,
    /// A file holding its `admin_pass`; the password is never taken on
    /// the command line, where it would land in the shell history.
    #[arg(long, value_name = "FILE")]
    pub password_file: Option<PathBuf>,
    /// The core's own database instead of its REST API (`sqlite:///…`
    /// or `postgres://…`, as in its `[database] url`); the core need not
    /// be running. Credentials in the URL never reach the output.
    #[arg(long, value_name = "URL", conflicts_with_all = ["rest", "user", "password_file"])]
    pub db: Option<String>,
    /// With `--db`: the core's `var_dir`, whose `messages/` holds the
    /// held messages; without it they are reported and not imported.
    #[arg(long, value_name = "DIR", requires = "db")]
    pub var_dir: Option<PathBuf>,
    /// `HyperKitty`'s own database (`sqlite:///…` or `postgres://…`, its
    /// Django `DATABASES`): votes, tags, thread categories and favourites,
    /// placed on the posts `listmngr archive import` brought from its mbox
    /// export and on the accounts imported here. Alone, or after the site.
    #[arg(long, value_name = "URL")]
    pub hyperkitty: Option<String>,
    /// Import this list only (its list id), instead of the whole site.
    #[arg(long, value_name = "LIST_ID")]
    pub list: Option<String>,
    /// Print the plan as JSON and change nothing.
    #[arg(long)]
    pub dry_run: bool,
}

async fn read(options: &Options, only: Option<&ListId>) -> Result<Site> {
    if let Some(url) = &options.db {
        let mut site = fetch_db(url, options.var_dir.as_deref()).await?;
        if let Some(only) = only {
            site.lists.retain(|list| &list.list_id == only);
        }
        return Ok(site);
    }
    let rest = options.rest.as_deref().unwrap_or_default();
    let password_file = options
        .password_file
        .as_ref()
        .context("--password-file is required with --rest")?;
    let password = std::fs::read_to_string(password_file)
        .with_context(|| format!("reading {}", password_file.display()))?;
    let rest = Rest::new(rest, &options.user, password.trim())?;
    Ok(fetch(&rest, only).await?)
}

pub async fn run(db: &Database, options: Options) -> Result<()> {
    let only: Option<ListId> = options.list.as_deref().map(str::parse).transpose()?;
    if options.rest.is_some() || options.db.is_some() {
        site(db, &options, only.as_ref()).await?;
    }
    if let Some(url) = &options.hyperkitty {
        archives(db, url, only.as_ref(), options.dry_run).await?;
    }
    Ok(())
}

async fn site(db: &Database, options: &Options, only: Option<&ListId>) -> Result<()> {
    let site = read(options, only).await?;
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

/// One JSON line per archived list: what `HyperKitty` holds for it, or
/// what landed here. A list this site does not have is a warning.
async fn archives(db: &Database, url: &str, only: Option<&ListId>, dry_run: bool) -> Result<()> {
    for archive in hyperkitty::fetch(url, only).await? {
        if dry_run {
            println!("{}", serde_json::json!({"hyperkitty": archive.to_json()}));
            continue;
        }
        if db.lists().get(&archive.list_id).await.is_err() {
            eprintln!(
                "warning: {} is archived by HyperKitty but is not a list here",
                archive.list_id
            );
            continue;
        }
        let now = chrono::Utc::now().timestamp_millis();
        let report = hyperkitty::apply(db, &archive, &AuditContext::system(), now).await?;
        println!(
            "{}",
            serde_json::json!({"hyperkitty": {
                "list_id": archive.list_id.as_str(),
                "votes": report.votes,
                "tags": report.tags,
                "categories": report.categories,
                "favorites": report.favorites,
                "skipped": report.skipped,
            }})
        );
    }
    Ok(())
}
