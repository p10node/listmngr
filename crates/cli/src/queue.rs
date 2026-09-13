use std::{
    io::{Read, Write},
    path::PathBuf,
};

use anyhow::Result;
use clap::Subcommand;
use listmngr_core::{Address, Error, ListId};
use listmngr_db::{
    AuditContext, Database,
    mail_queue::{JobId, JobState, NewMessage, Queue},
    queue_operations::{DeliveryResolution, ResolutionOutcome},
};
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

const MAX_RAW_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Mark a ready untrusted bounce report reviewed; retain its raw bytes.
    AcknowledgeBounce {
        id: Uuid,
        #[arg(long)]
        reason: String,
    },
    /// Inspect delivery outcomes, including quarantined ambiguous attempts.
    Recipients { id: Uuid },
    /// Resolve an ambiguous SMTP outcome after checking relay logs.
    Resolve {
        id: Uuid,
        email: String,
        #[arg(long, value_parser = parse_outcome)]
        outcome: ResolutionOutcome,
        #[arg(long)]
        reason: String,
        #[arg(long)]
        acknowledge_duplicate_risk: bool,
    },
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
        /// Read untrusted RFC3464 recipient claims; never score or acknowledge.
        #[arg(long, conflicts_with = "raw")]
        dsn: bool,
    },
    /// List up to 1,000 job metadata records in ID order, including retained jobs.
    Ls {
        #[arg(long, default_value = "in", value_parser = parse_queue)]
        queue: Queue,
        #[arg(long, value_parser = parse_state)]
        state: Option<JobState>,
    },
    /// Queue depth per queue and state, the shunted total and the age of the
    /// oldest ready job; metadata only.
    Stats,
    /// Replay a shunted (poison) job onto a live queue with a fresh attempt budget.
    Unshunt {
        id: Uuid,
        #[arg(long, value_parser = parse_queue)]
        target: Queue,
    },
}

fn parse_state(value: &str) -> std::result::Result<JobState, String> {
    serde_json::from_value(json!(value))
        .map_err(|_| "state must be ready, leased, done, or shunted".into())
}

fn parse_queue(value: &str) -> std::result::Result<Queue, String> {
    serde_json::from_value(json!(value)).map_err(|_| "unknown queue".into())
}

fn parse_outcome(value: &str) -> std::result::Result<ResolutionOutcome, String> {
    serde_json::from_value(json!(value))
        .map_err(|_| "outcome must be sent, failed, or retry".into())
}

async fn print_recipients(db: &Database, id: Uuid) -> Result<()> {
    db.mail_queue().job(JobId(id)).await?;
    let rows = sqlx::query(
        "SELECT email,status,detail FROM delivery_recipients WHERE job_id=$1 ORDER BY email",
    )
    .bind(id.to_string())
    .fetch_all(db.pool())
    .await
    .map_err(|_| Error::Database("recipient inspection failed".into()))?;
    let recipients = rows
        .iter()
        .map(|row| {
            Ok(json!({"email": row.try_get::<String, _>("email")?,
            "status": row.try_get::<String, _>("status")?,
            "detail": row.try_get::<String, _>("detail")?}))
        })
        .collect::<std::result::Result<Vec<_>, sqlx::Error>>()?;
    println!("{}", serde_json::to_string_pretty(&recipients)?);
    Ok(())
}

async fn print_job(db: &Database, id: Uuid, raw: bool, dsn: bool) -> Result<()> {
    let job = db.mail_queue().job(JobId(id)).await?;
    if dsn {
        if job.queue != Queue::Bounces {
            return Err(Error::Validation("DSN inspection requires a bounce job".into()).into());
        }
        let message = db.mail_queue().message(job.message_id).await?;
        let reports = listmngr_mail::dsn::parse(&message.raw)
            .ok_or_else(|| Error::Validation("unsupported or malformed DSN".into()))?;
        let recipients: Vec<_> = reports
            .into_iter()
            .map(|report| {
                json!({
                    "final_recipient":report.final_recipient, "action":report.action,
                    "status":report.status,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"untrusted":true,"recipients":recipients}))?
        );
    } else if raw {
        let message = db.mail_queue().message(job.message_id).await?;
        std::io::stdout().lock().write_all(&message.raw)?;
    } else {
        println!("{}", serde_json::to_string_pretty(&job)?);
    }
    Ok(())
}

pub async fn run(db: &Database, command: Command) -> Result<()> {
    match command {
        Command::AcknowledgeBounce { id, reason } => {
            db.acknowledge_bounce(JobId(id), &reason, &AuditContext::system())
                .await?;
            println!("{}", json!({"job_id": id, "state": "done"}));
        }
        Command::Resolve {
            id,
            email,
            outcome,
            reason,
            acknowledge_duplicate_risk,
        } => {
            db.resolve_delivery(
                DeliveryResolution {
                    job: JobId(id),
                    email: &email,
                    outcome,
                    reason: &reason,
                    acknowledge_duplicate_risk,
                },
                &AuditContext::system(),
                chrono::Utc::now().timestamp_millis(),
            )
            .await?;
            println!(
                "{}",
                json!({"job_id": id, "email": email, "outcome": outcome})
            );
        }
        Command::Recipients { id } => print_recipients(db, id).await?,
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
        Command::Show { id, raw, dsn } => print_job(db, id, raw, dsn).await?,
        Command::Ls { queue, state } => {
            let name = serde_json::to_value(queue)?;
            let state = state.map(serde_json::to_value).transpose()?;
            let ids: Vec<String> = sqlx::query_scalar(
                "SELECT id FROM queue_jobs WHERE queue=$1 AND ($2 IS NULL OR state=$2) ORDER BY id LIMIT 1000",
            )
            .bind(name.as_str().expect("queue is a serialized string"))
            .bind(state.as_ref().and_then(serde_json::Value::as_str))
            .fetch_all(db.pool())
            .await
            .map_err(|_| Error::Database("queue listing failed".into()))?;
            let mut jobs = Vec::with_capacity(ids.len());
            for id in ids {
                jobs.push(db.mail_queue().job(JobId(id.parse()?)).await?);
            }
            println!("{}", serde_json::to_string_pretty(&jobs)?);
        }
        Command::Stats => {
            let stats = db
                .mail_queue()
                .stats(chrono::Utc::now().timestamp_millis())
                .await?;
            println!("{}", serde_json::to_string_pretty(&stats)?);
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
