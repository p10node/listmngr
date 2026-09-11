use anyhow::{Result, bail};
use clap::Subcommand;
use listmngr_db::Database;
use uuid::Uuid;

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Publish warnings or remove due memberships in one bounded keyset page.
    Sweep {
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=1000))]
        limit: u32,
        /// Continue after the previous page's `next_cursor`; restart without it next cycle.
        #[arg(long)]
        after: Option<Uuid>,
    },
}
pub async fn run(db: &Database, command: Command) -> Result<()> {
    let Command::Sweep { limit, after } = command;
    let summary = db.bounce_maintenance().sweep(limit, after).await?;
    println!("{}", serde_json::to_string(&summary)?);
    if summary.failed != 0 {
        bail!("bounce maintenance contains failed items");
    }
    Ok(())
}
