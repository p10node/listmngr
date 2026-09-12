//! The moderator's subscription queue: list what is waiting on a list and
//! decide it. Decisions are attributed to the operator running the command
//! and commit with the membership change they cause.
use anyhow::Result;
use clap::Subcommand;
use listmngr_core::ListId;
use listmngr_db::workflows::RequestDecision;
use listmngr_db::{AuditContext, Database};

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Subscription requests waiting for a moderator, oldest first.
    Ls { list_id: ListId },
    /// Apply the membership change the request asked for.
    Accept { id: String },
    /// Refuse the request; it stops waiting and nothing changes.
    Reject { id: String },
    /// Refuse it silently, leaving no request behind.
    Discard { id: String },
    /// Leave it waiting, recording that it was looked at.
    Defer { id: String },
}

pub async fn run(db: &Database, command: Command) -> Result<()> {
    let (id, decision) = match command {
        Command::Ls { list_id } => {
            for request in db.workflows().pending(&list_id).await? {
                println!("{}", serde_json::to_string(&request)?);
            }
            return Ok(());
        }
        Command::Accept { id } => (id, RequestDecision::Accept),
        Command::Reject { id } => (id, RequestDecision::Reject),
        Command::Discard { id } => (id, RequestDecision::Discard),
        Command::Defer { id } => (id, RequestDecision::Defer),
    };
    db.workflows()
        .decide(&id, decision, &AuditContext::system())
        .await?;
    println!("{id} {}", serde_json::to_string(&decision)?);
    Ok(())
}
