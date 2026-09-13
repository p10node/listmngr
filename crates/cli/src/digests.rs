use anyhow::Result;
use clap::Subcommand;
use listmngr_core::ListId;
use listmngr_db::Database;

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Publish posts already collected for this list (does not send SMTP).
    Send { list_id: ListId },
    /// Collect ready posts and publish due issues; SMTP requires the mail role.
    Periodic,
    /// Increment volume and reset the next issue number atomically.
    Bump { list_id: ListId },
}
pub async fn run(db: &Database, command: Command) -> Result<()> {
    match command {
        Command::Send { list_id } => {
            println!(
                "{} issue(s) published",
                listmngr_runners::digests::send(db, &list_id, true).await?
            );
        }
        Command::Periodic => {
            println!(
                "{} issue(s) published",
                listmngr_runners::digests::tick(db, "digest-cli", false).await?
            );
        }
        Command::Bump { list_id } => {
            db.digests().bump(&list_id).await?;
            println!("digest volume advanced");
        }
    }
    Ok(())
}
