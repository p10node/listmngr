use anyhow::Result;
use clap::Subcommand;
use listmngr_db::Database;

#[derive(Debug, Subcommand)]
pub enum Command {
    /// One task sweep now, as the mail role runs on its schedule.
    Run,
}
pub async fn run(db: &Database, config: &listmngr_core::Config, command: Command) -> Result<()> {
    let Command::Run = command;
    let summary = db
        .tasks()
        .sweep(
            chrono::Utc::now().timestamp_millis(),
            i64::from(config.mailman.finished_job_retention_secs) * 1000,
        )
        .await?;
    println!("{}", serde_json::to_string(&summary)?);
    Ok(())
}
