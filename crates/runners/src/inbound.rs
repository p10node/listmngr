//! The LMTP-facing durable-intake handler: validates recipients against real
//! list identity and durably enqueues before any 250, exactly like `queue inject`.
use listmngr_core::ListId;
use listmngr_db::Database;
use listmngr_db::mail_queue::{NewMessage, Queue};
use listmngr_mail::lmtp::{LmtpHandler, RecipientOutcome};
use serde_json::json;
use std::time::Duration;

/// Sub-address suffixes this runtime explicitly recognizes as list command
/// addresses it does not yet implement, so they are refused rather than
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

#[derive(Debug, Clone)]
pub struct InboundHandler {
    pub db: Database,
    pub local_hostname: String,
    pub max_message_bytes: usize,
    pub max_recipients: usize,
    pub command_timeout: Duration,
    pub in_max_attempts: i64,
}

fn split_recipient(address: &str) -> Option<(String, String)> {
    let (local, domain) = address.rsplit_once('@')?;
    let domain = listmngr_core::normalize_domain(domain).ok()?;
    Some((local.to_ascii_lowercase(), domain))
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
        let Some((local, domain)) = split_recipient(address) else {
            return Err("malformed recipient address".into());
        };
        if let Ok(list_id) = format!("{local}.{domain}").parse::<ListId>()
            && self.db.lists().get(&list_id).await.is_ok()
        {
            return Ok(());
        }
        for suffix in RESERVED_SUFFIXES {
            let Some(base) = local.strip_suffix(suffix) else {
                continue;
            };
            if let Ok(list_id) = format!("{base}.{domain}").parse::<ListId>()
                && self.db.lists().get(&list_id).await.is_ok()
            {
                return Err(format!(
                    "list command address ({suffix}) is not implemented by this runtime"
                ));
            }
        }
        Err("unknown recipient".into())
    }

    async fn deliver(
        &mut self,
        mail_from: Option<&str>,
        recipients: &[String],
        data: &[u8],
    ) -> Vec<RecipientOutcome> {
        let Ok(external_id) = listmngr_mail::parse_message_id(data) else {
            return recipients
                .iter()
                .map(|_| RecipientOutcome {
                    code: 550,
                    detail: "5.6.0 invalid message metadata".into(),
                })
                .collect();
        };
        let hash = listmngr_mail::message_id_hash(&external_id).unwrap_or_default();
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut outcomes = Vec::with_capacity(recipients.len());
        for recipient in recipients {
            let Some((local, domain)) = split_recipient(recipient) else {
                outcomes.push(RecipientOutcome {
                    code: 550,
                    detail: "5.1.1 malformed recipient".into(),
                });
                continue;
            };
            let Ok(list_id) = format!("{local}.{domain}").parse::<ListId>() else {
                outcomes.push(RecipientOutcome {
                    code: 550,
                    detail: "5.1.1 unknown recipient".into(),
                });
                continue;
            };
            let context = json!({
                "version": 1,
                "list_id": list_id.to_string(),
                "envelope_sender": mail_from,
                "message_id_hash": hash,
            })
            .to_string();
            let input = NewMessage {
                raw: data.to_vec(),
                external_id: external_id.clone(),
                context,
                queue: Queue::In,
                max_attempts: self.in_max_attempts,
            };
            match self.db.mail_queue().enqueue(input, now_ms).await {
                Ok(_) => outcomes.push(RecipientOutcome {
                    code: 250,
                    detail: "2.1.5 accepted for delivery".into(),
                }),
                Err(_) => outcomes.push(RecipientOutcome {
                    code: 451,
                    detail: "4.3.0 temporary storage failure".into(),
                }),
            }
        }
        outcomes
    }
}
