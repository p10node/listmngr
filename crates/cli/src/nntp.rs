//! `listmngr nntp`: Mailman's `gatenews`, run once by hand.
use anyhow::Result;
use clap::Subcommand;
use listmngr_core::Config;
use listmngr_db::Database;

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Poll every gatewayed list's newsgroup once and hand new articles to
    /// the `in` queue (the running `nntp` runner does this on its own every
    /// `nntp.gatenews_every_secs`).
    Gate,
}

pub async fn run(db: &Database, config: &Config, command: Command) -> Result<()> {
    match command {
        Command::Gate => {
            anyhow::ensure!(
                config.nntp.enabled(),
                "no news server: set nntp.host before gating"
            );
            let report = listmngr_runners::nntp::gate_news_with(
                db,
                &config.nntp,
                &listmngr_runners::StructureLimits::from(&config.mta),
            )
            .await?;
            for entry in &report {
                println!("{}", entry.to_json());
            }
            if report.is_empty() {
                eprintln!("no list gateways from a newsgroup");
            }
        }
    }
    Ok(())
}
