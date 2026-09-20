//! Mailman's `nntp` runner: the `nntp` queue's consumer.
//!
//! It prepares an accepted post for the list's newsgroup and offers it to
//! the news server. A refusal for the post's `Message-ID` (a cross-post the
//! server already holds) is answered as Mailman answers it — a `Message-ID`
//! of the list's own and one more try; any other trouble waits and tries
//! again within the job's budget.
use listmngr_core::NntpConfig;
use listmngr_db::{Database, mail_queue::Lease, mail_queue::Queue};
use listmngr_mail::handlers::{Admission, Target, cook_with};
use listmngr_mail::nntp::{Client, Outcome as Verdict};
use std::time::Duration;
use tokio::sync::watch;

/// The lease held while a post is offered to the news server.
const LEASE_MS: i64 = 30_000;
/// The context key holding the replacement `Message-ID` after a refusal.
const MUNGED_ID: &str = "nntp_message_id";

pub async fn run(db: Database, config: NntpConfig, mut shutdown: watch::Receiver<bool>) {
    if !config.enabled() {
        // Gated posts wait in the queue for a news server to be configured.
        tracing::info!("nntp runner idle: no [nntp] host configured");
        let _ = shutdown.changed().await;
        return;
    }
    while !*shutdown.borrow() {
        let claimed = db
            .mail_queue()
            .live()
            .claim(
                Queue::Nntp,
                "nntp-0",
                chrono::Utc::now().timestamp_millis(),
                LEASE_MS,
            )
            .await;
        match claimed {
            Ok(Some(lease)) => process(&db, &config, &lease).await,
            Ok(None) => pause(&mut shutdown).await,
            Err(error) => {
                tracing::error!(%error, "nntp claim failed");
                pause(&mut shutdown).await;
            }
        }
    }
}

/// Wait for work, or for the shutdown signal.
async fn pause(shutdown: &mut watch::Receiver<bool>) {
    tokio::select! { ()=tokio::time::sleep(Duration::from_millis(500))=>{}, _=shutdown.changed()=>{} }
}

/// What became of one leased post.
#[derive(Debug)]
enum Gated {
    /// The server has it.
    Posted,
    /// The list no longer gateways: nothing to do, the job is done.
    Skipped,
    /// The server answered the article with a refusal.
    Refused(String),
}

/// Offer one leased post; the outcome decides the job's transition.
pub(crate) async fn process(db: &Database, config: &NntpConfig, lease: &Lease) {
    let now = chrono::Utc::now().timestamp_millis();
    let outcome =
        crate::heartbeat::run_while_renewing(db, lease, LEASE_MS, gate(db, config, lease)).await;
    let queue = db.mail_queue().live();
    let result = match outcome {
        crate::heartbeat::Outcome::Completed(Ok(Gated::Posted | Gated::Skipped)) => {
            queue.ack(lease, now).await.map(|_| ())
        }
        crate::heartbeat::Outcome::Completed(Ok(Gated::Refused(reply))) => {
            refused(db, lease, &reply, now).await
        }
        crate::heartbeat::Outcome::Completed(Err(error)) => {
            tracing::warn!(%error, "news posting failed; retrying");
            let delay = 60_000_i64.saturating_mul(1 << lease.job.attempts.clamp(0, 6));
            queue
                .retry(lease, now, delay.min(3_600_000), &error.to_string())
                .await
                .map(|_| ())
        }
        crate::heartbeat::Outcome::LeaseLost => Ok(()),
    };
    if let Err(error) = result {
        tracing::error!(%error, "nntp queue transition failed");
    }
}

/// Mailman's answer to a refusal: a `441` is most likely a duplicate
/// `Message-ID` from a cross-post, so the article gets one of the list's
/// own and one more try; a second `441`, or any other refusal, is the
/// article's own fault and the job is shunted with the server's words.
async fn refused(
    db: &Database,
    lease: &Lease,
    reply: &str,
    now: i64,
) -> Result<(), listmngr_core::Error> {
    let queue = db.mail_queue().live();
    if reply.starts_with("441") && !has_munged_id(db, lease).await {
        let munged = list_of(db, lease)
            .await
            .as_ref()
            .map(listmngr_mail::nntp::list_message_id)
            .unwrap_or_default();
        db.mail_queue()
            .set_context_value(lease.job.message_id, MUNGED_ID, serde_json::json!(munged))
            .await?;
        return queue
            .retry(
                lease,
                now,
                1_000,
                "news server refused the Message-ID; munged",
            )
            .await
            .map(|_| ());
    }
    tracing::warn!(reply, "news server refused the article");
    queue
        .shunt(
            lease,
            now,
            &format!("news server refused the article: {reply}"),
        )
        .await
        .map(|_| ())
}

async fn has_munged_id(db: &Database, lease: &Lease) -> bool {
    db.mail_queue()
        .message(lease.job.message_id)
        .await
        .ok()
        .and_then(|message| serde_json::from_str::<serde_json::Value>(&message.context).ok())
        .is_some_and(|context| context[MUNGED_ID].is_string())
}

async fn list_of(db: &Database, lease: &Lease) -> Option<listmngr_core::MailingList> {
    let message = db.mail_queue().message(lease.job.message_id).await.ok()?;
    let context: serde_json::Value = serde_json::from_str(&message.context).ok()?;
    let list_id: listmngr_core::ListId = context["list_id"].as_str()?.parse().ok()?;
    db.lists().get(&list_id).await.ok()
}

/// The post as of `to-usenet`, prepared for the newsgroup, offered.
async fn gate(
    db: &Database,
    config: &NntpConfig,
    lease: &Lease,
) -> Result<Gated, listmngr_core::Error> {
    let message = db.mail_queue().message(lease.job.message_id).await?;
    let context: serde_json::Value = serde_json::from_str(&message.context)
        .map_err(|_| listmngr_core::Error::Validation("invalid message context".into()))?;
    let list_id: listmngr_core::ListId = context["list_id"]
        .as_str()
        .ok_or_else(|| listmngr_core::Error::Validation("message without a list".into()))?
        .parse()?;
    let list = db.lists().get(&list_id).await?;
    if !list.usenet.gateway_to_news || list.usenet.linked_newsgroup.is_empty() {
        return Ok(Gated::Skipped);
    }
    let results = context["authentication_results"]
        .as_str()
        .map(str::to_owned);
    let cooked = cook_with(
        Target::Nntp,
        &message.raw,
        &list,
        &lease.job.message_id.0.to_string(),
        &Admission {
            base_url: db.base_url(),
            dmarc_mitigate: context["dmarc_mitigate"] == true,
            authentication_results: results.as_deref(),
            keep_arc: false,
            from_usenet: context["fromusenet"] == true,
        },
    )
    .map_err(|error| listmngr_core::Error::Validation(error.to_string()))?;
    let mut article = listmngr_mail::nntp::prepare(&cooked, &list, config)
        .map_err(|error| listmngr_core::Error::Validation(error.to_string()))?;
    if let Some(munged) = context[MUNGED_ID].as_str() {
        article = listmngr_mail::nntp::with_message_id(&article, munged)
            .map_err(|error| listmngr_core::Error::Validation(error.to_string()))?;
    }
    let client = Client::try_new(config)
        .map_err(|error| listmngr_core::Error::Validation(error.to_string()))?;
    match client
        .post(&article)
        .await
        .map_err(|error| listmngr_core::Error::Validation(error.to_string()))?
    {
        Verdict::Accepted => Ok(Gated::Posted),
        Verdict::Refused(reply) => Ok(Gated::Refused(reply)),
    }
}
