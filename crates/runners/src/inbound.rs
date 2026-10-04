//! The LMTP-facing durable-intake handler: validates recipients against real
//! list identity and durably enqueues before any 250, exactly like `queue inject`.
use listmngr_core::{EmailCommand, ListId};
use listmngr_db::Database;
use listmngr_db::mail_queue::{NewMessage, Queue};
use listmngr_mail::lmtp::{LmtpHandler, RecipientOutcome, RecipientRejection};
use serde_json::json;
use std::time::Duration;

/// Sub-address suffixes this runtime explicitly recognizes as list command
/// addresses, so unsupported commands are refused rather than
/// silently misrouted as a differently-named list's normal posting address.
const RESERVED_SUFFIXES: &[&str] = &[
    "-owner",
    "-request",
    "-join",
    "-subscribe",
    "-leave",
    "-unsubscribe",
    "-bounces",
    "-confirm",
];

pub use listmngr_mail::mta::COMMAND_SUFFIXES;

#[derive(Debug, Clone)]
pub struct InboundHandler {
    pub db: Database,
    pub local_hostname: String,
    pub max_message_bytes: usize,
    pub max_recipients: usize,
    pub command_timeout: Duration,
    pub in_max_attempts: i64,
    /// `[mta] verp_delimiter`: `list-bounces<delimiter>local=domain` names
    /// the recipient a bounce concerns.
    pub verp_delimiter: String,
    /// `[mta] max_header_count`, `max_mime_parts`, `max_mime_depth`: a
    /// message over any of them is refused for every recipient before
    /// anything is stored.
    pub structure: listmngr_mail::structure::Limits,
}

/// Where an inbound recipient address routes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    list: ListId,
    command: Option<&'static str>,
    /// The recipient a VERP bounce address encodes.
    verp_recipient: Option<String>,
}

fn split_recipient(address: &str) -> Option<(String, String)> {
    let (local, domain) = address.rsplit_once('@')?;
    let domain = listmngr_core::normalize_domain(domain).ok()?;
    Some((local.to_ascii_lowercase(), domain))
}

/// The stored context of one queued submission.
fn submission_context(
    list_id: &ListId,
    mail_from: Option<&str>,
    hash: &str,
    verp_recipient: Option<String>,
) -> serde_json::Value {
    let mut context = json!({
        "version": 1,
        "list_id": list_id.to_string(),
        "envelope_sender": mail_from,
        "message_id_hash": hash,
    });
    if let Some(recipient) = verp_recipient {
        // A probe's one-time address encodes its token where a recipient's
        // domain would be; that is the bounce runner's cue, not a mailbox.
        match recipient.strip_prefix(concat!("probe", "@")) {
            Some(token) if token.len() == 40 && token.bytes().all(|b| b.is_ascii_hexdigit()) => {
                context["probe_token"] = json!(token);
            }
            _ => context["verp_recipient"] = json!(recipient),
        }
    }
    context
}

fn invalid_metadata() -> RecipientOutcome {
    RecipientOutcome {
        code: 550,
        detail: "5.6.0 invalid message metadata".into(),
    }
}

fn message_identity(external_id: Option<&str>, bounce: bool) -> Option<(String, String)> {
    // An internal storage identity only; never inserted into the raw report and
    // never evidence of sender authenticity or correlation with an outgoing job.
    let id = external_id
        .map(str::to_owned)
        .or_else(|| bounce.then(|| format!("bounce-{}@listmngr.invalid", uuid::Uuid::now_v7())))?;
    let hash = listmngr_mail::message_id_hash(&id).ok()?;
    Some((id, hash))
}

fn parse_command(route: &str, data: &[u8]) -> Option<EmailCommand> {
    use listmngr_mail::commands::{confirmation_token, parse};
    match route {
        "join" => Some(EmailCommand::Join),
        "leave" => Some(EmailCommand::Leave),
        "confirm" => confirmation_token(data).map(EmailCommand::Confirm),
        "request" => parse(data),
        _ => None,
    }
}

impl InboundHandler {
    async fn delivery_target(&self, recipient: &str) -> Result<Target, RecipientRejection> {
        // Ordinary recipients were admitted during RCPT. Only suffix and
        // VERP routing need disambiguation from an exactly-named posting
        // address.
        let Some((local, domain)) = split_recipient(recipient) else {
            return Err(RecipientRejection::Permanent(
                "malformed recipient address".into(),
            ));
        };
        if RESERVED_SUFFIXES
            .iter()
            .any(|suffix| local.ends_with(suffix))
            || local.contains(&self.verp_delimiter)
        {
            self.target(recipient).await
        } else {
            format!("{local}.{domain}")
                .parse::<ListId>()
                .map(|id| Target {
                    list: id,
                    command: None,
                    verp_recipient: None,
                })
                .map_err(|_| RecipientRejection::Permanent("malformed recipient address".into()))
        }
    }

    async fn list_exists(&self, list_id: &ListId) -> Result<bool, RecipientRejection> {
        match self.db.lists().get(list_id).await {
            Ok(_) => Ok(true),
            Err(listmngr_core::Error::NotFound(_)) => Ok(false),
            Err(_) => Err(RecipientRejection::Temporary(
                "temporary recipient lookup failure".into(),
            )),
        }
    }

    async fn target(&self, address: &str) -> Result<Target, RecipientRejection> {
        let Some((local, domain)) = split_recipient(address) else {
            return Err(RecipientRejection::Permanent(
                "malformed recipient address".into(),
            ));
        };
        if let Ok(id) = format!("{local}.{domain}").parse::<ListId>()
            && self.list_exists(&id).await?
        {
            return Ok(Target {
                list: id,
                command: None,
                verp_recipient: None,
            });
        }
        // A VERP bounce address: the list's bounces address with the
        // recipient encoded after the delimiter.
        if let Some((bounces, recipient)) =
            listmngr_core::verp::decode(&local, &self.verp_delimiter)
            && let Some(base) = bounces.strip_suffix("-bounces")
            && let Ok(id) = format!("{base}.{domain}").parse::<ListId>()
            && self.list_exists(&id).await?
        {
            return Ok(Target {
                list: id,
                command: Some("bounces"),
                verp_recipient: Some(recipient),
            });
        }
        for suffix in RESERVED_SUFFIXES {
            let Some(base) = local.strip_suffix(suffix) else {
                continue;
            };
            if let Ok(id) = format!("{base}.{domain}").parse::<ListId>()
                && self.list_exists(&id).await?
            {
                if let Some((_, command)) = COMMAND_SUFFIXES
                    .iter()
                    .find(|(supported, _)| Some(*supported) == suffix.strip_prefix('-'))
                {
                    return Ok(Target {
                        list: id,
                        command: Some(command),
                        verp_recipient: None,
                    });
                }
                return Err(RecipientRejection::Permanent(format!(
                    "list command address ({suffix}) is not implemented by this runtime"
                )));
            }
        }
        Err(RecipientRejection::Permanent("unknown recipient".into()))
    }
}

impl InboundHandler {
    /// The checks before anything is stored: the structure ceilings, then
    /// the header block and its `Message-ID`. `Err` carries the reply for
    /// every recipient.
    fn admit(
        &self,
        recipients: &[String],
        data: &[u8],
    ) -> Result<Option<String>, Vec<RecipientOutcome>> {
        if let Err(excess) = listmngr_mail::structure::check(data, &self.structure) {
            tracing::info!(
                %excess,
                recipients = recipients.len(),
                "intake: message over the structure ceilings refused"
            );
            let outcomes: Vec<_> = recipients
                .iter()
                .map(|_| RecipientOutcome {
                    code: 554,
                    detail: format!("5.6.0 message structure exceeds the site's limits: {excess}"),
                })
                .collect();
            count_lmtp_outcomes(&outcomes);
            return Err(outcomes);
        }
        listmngr_mail::parse_optional_message_id(data)
            .map_err(|_| recipients.iter().map(|_| invalid_metadata()).collect())
    }
}

impl LmtpHandler for InboundHandler {
    fn local_hostname(&self) -> &str {
        &self.local_hostname
    }
    fn max_message_bytes(&self) -> usize {
        self.max_message_bytes
    }
    fn max_recipients(&self) -> usize {
        self.max_recipients
    }
    fn command_timeout(&self) -> Duration {
        self.command_timeout
    }

    async fn accept_recipient(&mut self, address: &str) -> Result<(), String> {
        self.validate_recipient(address)
            .await
            .map_err(|error| error.detail().to_owned())
    }

    async fn validate_recipient(&mut self, address: &str) -> Result<(), RecipientRejection> {
        self.target(address).await.map(|_| ())
    }

    async fn deliver(
        &mut self,
        mail_from: Option<&str>,
        recipients: &[String],
        data: &[u8],
    ) -> Vec<RecipientOutcome> {
        let external_id = match self.admit(recipients, data) {
            Ok(external_id) => external_id,
            Err(outcomes) => return outcomes,
        };

        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut outcomes = Vec::with_capacity(recipients.len());
        let mut inputs = Vec::with_capacity(recipients.len());
        let mut queued_indices = Vec::with_capacity(recipients.len());
        for recipient in recipients {
            let target = self.delivery_target(recipient).await;
            let Target {
                list: list_id,
                command,
                verp_recipient,
            } = match target {
                Ok(target) => target,
                Err(error) => {
                    outcomes.push(RecipientOutcome {
                        code: match &error {
                            RecipientRejection::Temporary(_) => 451,
                            RecipientRejection::Permanent(_) => 550,
                        },
                        detail: error.detail().into(),
                    });
                    continue;
                }
            };
            let bounce = command == Some("bounces");
            let Some((external_id, hash)) = message_identity(external_id.as_deref(), bounce) else {
                outcomes.push(invalid_metadata());
                continue;
            };
            if command.is_some()
                && !bounce
                && (mail_from.is_none_or(str::is_empty)
                    || !listmngr_mail::commands::allows_reply(data))
            {
                outcomes.push(RecipientOutcome {
                    code: 550,
                    detail: "5.7.1 command requires a non-automatic reply address".into(),
                });
                continue;
            }
            if command == Some("owner")
                && !listmngr_mail::owner::allows_forward(data, mail_from, &list_id)
            {
                outcomes.push(RecipientOutcome {
                    code: 550,
                    detail: "5.7.1 unsafe owner forwarding request".into(),
                });
                continue;
            }
            let mut context = submission_context(&list_id, mail_from, &hash, verp_recipient);
            if command == Some("owner") {
                context["owner_route"] = json!(true);
            } else if let Some(command) = command.filter(|_| !bounce) {
                let Some(command) = parse_command(command, data) else {
                    outcomes.push(RecipientOutcome {
                        code: 550,
                        detail: "5.6.0 unsupported or malformed email command".into(),
                    });
                    continue;
                };
                context["subscription_command"] = json!(command);
            }
            let input = NewMessage {
                raw: data.to_vec(),
                external_id: external_id.clone(),
                context: context.to_string(),
                queue: if bounce { Queue::Bounces } else { Queue::In },
                max_attempts: self.in_max_attempts,
            };
            inputs.push(input);
            queued_indices.push(outcomes.len());
            outcomes.push(RecipientOutcome {
                code: 451,
                detail: "4.3.0 temporary storage failure".into(),
            });
        }
        if !inputs.is_empty()
            && self
                .db
                .mail_queue()
                .enqueue_batch(&inputs, now_ms)
                .await
                .is_ok()
        {
            for index in queued_indices {
                outcomes[index] = RecipientOutcome {
                    code: 250,
                    detail: "2.1.5 accepted for delivery".into(),
                };
            }
        }
        count_lmtp_outcomes(&outcomes);
        outcomes
    }
}

/// `listmngr_lmtp_recipients_total` by reply class.
fn count_lmtp_outcomes(outcomes: &[RecipientOutcome]) {
    let metrics = listmngr_core::metrics::global();
    for outcome in outcomes {
        metrics.lmtp_recipients.inc(match outcome.code {
            250 => "accepted",
            451 => "deferred",
            _ => "rejected",
        });
    }
}
