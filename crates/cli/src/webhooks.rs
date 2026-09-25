//! `listmngr webhooks`: the site's webhooks from the command line, one
//! JSON line per webhook or delivery; a secret is printed once, on `add`
//! and `rotate`, and never again.
use anyhow::Result;
use clap::Subcommand;
use listmngr_core::{ListId, WebhookId};
use listmngr_db::{AuditContext, Database, NewWebhook, WebhookPatch};

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List the site's webhooks, or one list's.
    Ls {
        #[arg(long, value_name = "LIST_ID")]
        list: Option<String>,
    },
    /// Create a webhook and print it with its secret, shown this once.
    Add {
        /// The `https://` URL the site posts to.
        url: String,
        /// Audit actions to post: `*`, a prefix such as `member.*`, or an
        /// action such as `list.config`; comma-separated.
        #[arg(long, value_delimiter = ',', default_value = "*")]
        events: Vec<String>,
        /// Only this list's events.
        #[arg(long, value_name = "LIST_ID")]
        list: Option<String>,
        #[arg(long, default_value = "")]
        description: String,
    },
    /// Change a webhook: what is not given is kept.
    Set {
        id: WebhookId,
        #[arg(long)]
        url: Option<String>,
        #[arg(long, value_delimiter = ',')]
        events: Option<Vec<String>>,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        enabled: Option<bool>,
    },
    /// Delete a webhook and every delivery it was owed.
    Rm { id: WebhookId },
    /// Give a webhook a new secret and print it, shown this once.
    Rotate { id: WebhookId },
    /// Queue a `ping` delivery to see the target answer.
    Ping { id: WebhookId },
    /// The webhook's deliveries, newest first.
    Deliveries {
        id: WebhookId,
        #[arg(long, default_value_t = 50)]
        limit: i64,
    },
}

fn parse_list(list: Option<String>) -> Result<Option<ListId>> {
    Ok(list.map(|list| list.parse()).transpose()?)
}

fn with_secret(webhook: &listmngr_db::Webhook, secret: &str) -> serde_json::Value {
    let mut value = serde_json::to_value(webhook).unwrap_or_default();
    value["secret"] = serde_json::Value::String(secret.to_owned());
    value
}

pub async fn run(db: &Database, command: Command) -> Result<()> {
    let repo = db.webhooks();
    let context = AuditContext::system();
    match command {
        Command::Ls { list } => {
            let list = parse_list(list)?;
            for webhook in repo.list(list.as_ref()).await? {
                println!("{}", serde_json::to_string(&webhook)?);
            }
        }
        Command::Add {
            url,
            events,
            list,
            description,
        } => {
            let (webhook, secret) = repo
                .create_with_context(
                    NewWebhook {
                        url,
                        events,
                        list_id: parse_list(list)?,
                        description,
                    },
                    &context,
                )
                .await?;
            println!("{}", with_secret(&webhook, &secret));
        }
        Command::Set {
            id,
            url,
            events,
            description,
            enabled,
        } => {
            let webhook = repo
                .update_with_context(
                    id,
                    WebhookPatch {
                        url,
                        events,
                        description,
                        enabled,
                    },
                    &context,
                )
                .await?;
            println!("{}", serde_json::to_string(&webhook)?);
        }
        Command::Rm { id } => {
            repo.delete_with_context(id, &context).await?;
            println!("deleted {id}");
        }
        Command::Rotate { id } => {
            let secret = repo.rotate_with_context(id, &context).await?;
            let webhook = repo.get(id).await?;
            println!("{}", with_secret(&webhook, &secret));
        }
        Command::Ping { id } => {
            let delivery = repo.ping_with_context(id, &context).await?;
            println!("{}", serde_json::to_string(&delivery)?);
        }
        Command::Deliveries { id, limit } => {
            for delivery in repo.deliveries(id, limit).await? {
                println!("{}", serde_json::to_string(&delivery)?);
            }
        }
    }
    Ok(())
}
