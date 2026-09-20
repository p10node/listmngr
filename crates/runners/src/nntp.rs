//! Mailman's `nntp` runner: the `nntp` queue's consumer.
//!
//! It prepares an accepted post for the list's newsgroup and offers it to
//! the news server. A refusal for the post's `Message-ID` (a cross-post the
//! server already holds) is answered as Mailman answers it — a `Message-ID`
//! of the list's own and one more try; any other trouble waits and tries
//! again within the job's budget.
use listmngr_core::NntpConfig;
use listmngr_db::{
    Database,
    mail_queue::{Lease, NewMessage, Queue},
};
use listmngr_mail::handlers::{Admission, Target, cook_with};
use listmngr_mail::nntp::{Client, Outcome as Verdict};
use std::time::{Duration, Instant};
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
    let every = Duration::from_secs(u64::from(config.gatenews_every_secs));
    let mut last_poll: Option<Instant> = None;
    while !*shutdown.borrow() {
        // Mailman's nntp runner runs `gatenews` between its jobs, every
        // `gatenews_every`; the first poll happens at once.
        if !every.is_zero() && last_poll.is_none_or(|at| at.elapsed() >= every) {
            last_poll = Some(Instant::now());
            poll_and_log(&db, &config).await;
        }
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

/// One scheduled `gatenews`, its report in the log.
async fn poll_and_log(db: &Database, config: &NntpConfig) {
    match gate_news(db, config).await {
        Ok(report) => {
            for entry in report {
                tracing::info!(
                    list = %entry.list_id,
                    newsgroup = entry.newsgroup,
                    gated = entry.gated,
                    watermark = entry.watermark,
                    error = entry.error,
                    "gatenews"
                );
            }
        }
        Err(error) => tracing::error!(%error, "gatenews failed"),
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

/// What one poll did for one list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateReport {
    pub list_id: String,
    pub newsgroup: String,
    /// The watermark after the poll; `None` when it could not be set.
    pub watermark: Option<i64>,
    /// Articles handed to the `in` queue.
    pub gated: u64,
    /// Why the list was not polled to the end, when it was not.
    pub error: Option<String>,
}

/// Mailman's `gatenews`.
///
/// For every list that gateways from its newsgroup, read the articles the
/// list has not seen and hand them to the `in` queue as posts from Usenet.
/// A list never polled before only catches up (its watermark becomes the
/// group's last article), so a newly linked group does not flood the
/// list. Each article the poll passes — gated, the list's own, or without
/// a sender — moves the watermark with an audit event, so a poll cut
/// short never repeats what it did.
/// # Errors
/// Returns the database error that stopped the poll; a news server that
/// cannot be reached, or a group it does not know, is reported per list.
pub async fn gate_news(
    db: &Database,
    config: &NntpConfig,
) -> Result<Vec<GateReport>, listmngr_core::Error> {
    let mut report = Vec::new();
    let lists: Vec<_> = db
        .lists()
        .list(None)
        .await?
        .into_iter()
        .filter(|list| list.usenet.gateway_to_mail && !list.usenet.linked_newsgroup.is_empty())
        .collect();
    if lists.is_empty() {
        return Ok(report);
    }
    let client = Client::try_new(config)
        .map_err(|error| listmngr_core::Error::Validation(error.to_string()))?;
    let mut reader = match client.reader().await {
        Ok(reader) => reader,
        Err(error) => {
            // No server: every list is reported, none is touched.
            for list in lists {
                report.push(GateReport {
                    list_id: list.id.to_string(),
                    newsgroup: list.usenet.linked_newsgroup,
                    watermark: list.usenet.usenet_watermark,
                    gated: 0,
                    error: Some(error.to_string()),
                });
            }
            return Ok(report);
        }
    };
    for list in lists {
        let newsgroup = list.usenet.linked_newsgroup.clone();
        let mut entry = GateReport {
            list_id: list.id.to_string(),
            newsgroup: newsgroup.clone(),
            watermark: list.usenet.usenet_watermark,
            gated: 0,
            error: None,
        };
        match reader.group(&newsgroup).await {
            Err(error) => entry.error = Some(error.to_string()),
            Ok((first, last)) => {
                if let Err(error) = poll(db, &mut reader, &list, first, last, &mut entry).await {
                    entry.error = Some(error.to_string());
                }
            }
        }
        report.push(entry);
    }
    reader.quit().await;
    Ok(report)
}

/// One list's poll of the group's `lowest..=highest`, from where it left
/// off.
async fn poll(
    db: &Database,
    reader: &mut listmngr_mail::nntp::Reader,
    list: &listmngr_core::MailingList,
    lowest: u64,
    highest: u64,
    entry: &mut GateReport,
) -> Result<(), listmngr_core::Error> {
    let watermark = |number: u64| i64::try_from(number).unwrap_or(i64::MAX);
    let Some(seen) = list.usenet.usenet_watermark else {
        // Never polled: catch up without flooding the list.
        db.usenet()
            .set_watermark(&list.id, watermark(highest))
            .await?;
        entry.watermark = Some(watermark(highest));
        return Ok(());
    };
    let start = u64::try_from(seen)
        .unwrap_or(0)
        .saturating_add(1)
        .max(lowest);
    for number in start..=highest {
        match reader.article(number).await {
            Ok(article) => {
                if gate_article(db, list, &article).await? {
                    entry.gated += 1;
                }
            }
            // An expired or damaged article: Mailman logs and moves on.
            Err(error) => {
                tracing::warn!(list = %list.id, number, %error, "gatenews: article skipped");
            }
        }
        db.usenet()
            .set_watermark(&list.id, watermark(number))
            .await?;
        entry.watermark = Some(watermark(number));
    }
    Ok(())
}

/// Hand one article to the `in` queue as a post from Usenet; `false` when
/// it is the list's own or has no sender.
async fn gate_article(
    db: &Database,
    list: &listmngr_core::MailingList,
    article: &[u8],
) -> Result<bool, listmngr_core::Error> {
    let Some((raw, sender)) = listmngr_mail::nntp::inbound(article, list)
        .map_err(|error| listmngr_core::Error::Validation(error.to_string()))?
    else {
        return Ok(false);
    };
    let external_id = listmngr_mail::header_value(&raw, "Message-ID")
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| listmngr_mail::nntp::list_message_id(list));
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw,
                external_id,
                context: serde_json::json!({
                    "version": 1,
                    "list_id": list.id.to_string(),
                    "envelope_sender": sender,
                    "fromusenet": true,
                })
                .to_string(),
                queue: Queue::In,
                max_attempts: 5,
            },
            chrono::Utc::now().timestamp_millis(),
        )
        .await?;
    Ok(true)
}

impl GateReport {
    /// The report as JSON, for the command line.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "list_id": self.list_id,
            "newsgroup": self.newsgroup,
            "watermark": self.watermark,
            "gated": self.gated,
            "error": self.error,
        })
    }
}
