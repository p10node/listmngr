use std::{
    io::{Read, Write},
    path::PathBuf,
};

use anyhow::Result;
use clap::Subcommand;
use listmngr_core::{Address, Error, ListId};
use listmngr_db::{
    Database,
    mail_queue::{JobId, NewMessage, Queue},
};
use serde_json::json;
use uuid::Uuid;

const MAX_RAW_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Store a message in the inbound queue; no delivery occurs without a worker.
    Inject {
        list_id: ListId,
        file: PathBuf,
        #[arg(long)]
        sender: String,
    },
    /// Inspect metadata; use --raw to explicitly export original message bytes.
    Show {
        id: Uuid,
        #[arg(long)]
        raw: bool,
    },
    /// List up to 1,000 job metadata records in ID order, including retained jobs.
    Ls {
        #[arg(long, default_value = "in", value_parser = parse_queue)]
        queue: Queue,
    },
    /// Replay a shunted (poison) job onto a live queue with a fresh attempt budget.
    Unshunt {
        id: Uuid,
        #[arg(long, value_parser = parse_queue)]
        target: Queue,
    },
}

fn parse_queue(value: &str) -> std::result::Result<Queue, String> {
    serde_json::from_value(json!(value)).map_err(|_| "unknown queue".into())
}

pub async fn run(db: &Database, command: Command) -> Result<()> {
    match command {
        Command::Inject {
            list_id,
            file,
            sender,
        } => {
            let sender = Address::new(&sender, String::new())?.email;
            db.lists().get(&list_id).await?;
            if !std::fs::metadata(&file)?.is_file() {
                return Err(
                    Error::Validation("message input must be a regular file".into()).into(),
                );
            }
            let mut raw = Vec::new();
            std::fs::File::open(file)?
                .take(MAX_RAW_BYTES + 1)
                .read_to_end(&mut raw)?;
            if raw.len() as u64 > MAX_RAW_BYTES {
                return Err(Error::Validation("message exceeds intake limit".into()).into());
            }
            let external_id = listmngr_mail::parse_message_id(&raw)
                .map_err(|_| Error::Validation("invalid message metadata".into()))?;
            let hash = listmngr_mail::message_id_hash(&external_id)
                .map_err(|_| Error::Validation("invalid message metadata".into()))?;
            let job = db.mail_queue().enqueue(NewMessage {
                raw,
                external_id,
                context: json!({"version": 1, "list_id": list_id, "envelope_sender": sender, "message_id_hash": hash}).to_string(),
                queue: Queue::In,
                max_attempts: 5,
            }, chrono::Utc::now().timestamp_millis()).await?;
            println!("{}", serde_json::to_string_pretty(&job)?);
        }
        Command::Show { id, raw } => {
            let job = db.mail_queue().job(JobId(id)).await?;
            if raw {
                let message = db.mail_queue().message(job.message_id).await?;
                std::io::stdout().lock().write_all(&message.raw)?;
            } else {
                println!("{}", serde_json::to_string_pretty(&job)?);
            }
        }
        Command::Ls { queue } => {
            let name = serde_json::to_value(queue)?;
            let ids: Vec<String> = sqlx::query_scalar(
                "SELECT id FROM queue_jobs WHERE queue=$1 ORDER BY id LIMIT 1000",
            )
            .bind(name.as_str().expect("queue is a serialized string"))
            .fetch_all(db.pool())
            .await
            .map_err(|_| Error::Database("queue listing failed".into()))?;
            let mut jobs = Vec::with_capacity(ids.len());
            for id in ids {
                jobs.push(db.mail_queue().job(JobId(id.parse()?)).await?);
            }
            println!("{}", serde_json::to_string_pretty(&jobs)?);
        }
        Command::Unshunt { id, target } => {
            let job = db
                .mail_queue()
                .unshunt(JobId(id), chrono::Utc::now().timestamp_millis(), target)
                .await?;
            println!("{}", serde_json::to_string_pretty(&job)?);
        }
    }
    Ok(())
}
