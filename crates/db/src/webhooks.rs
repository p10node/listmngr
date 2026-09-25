//! Webhooks: where the site posts what its audit log records.
//!
//! A webhook subscribes to audit actions (`list.config`, `member.*`, `*`)
//! for the whole site or for one list. When a write commits its audit
//! event, [`fan_out`] — inside that same transaction — leaves one delivery
//! per webhook the event matches, so an event can neither be lost nor
//! exist without its write; the webhook runner posts the deliveries later.
//!
//! A webhook's secret is derived from the site's signing key and the
//! webhook's own salt (HKDF-SHA256), shown once and never stored: the
//! database keeps the secret's hash, to show a fingerprint, and the salt,
//! so a database on its own cannot sign a delivery.
use crate::{AuditContext, Database, db_error};
use hmac::{Hmac, Mac};
use listmngr_core::{Error, ListId, Result, WebhookId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Any, Row, Transaction};
use uuid::Uuid;

/// Longest target URL accepted.
pub const MAX_URL_BYTES: usize = 2048;
/// Most event patterns on one webhook.
pub const MAX_EVENTS: usize = 64;
const MAX_DESCRIPTION_BYTES: usize = 256;
const MAX_EVENT_BYTES: usize = 64;

/// What an operator gives to create a webhook.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct NewWebhook {
    pub url: String,
    /// Audit actions to post: `*`, a prefix such as `member.*`, or an
    /// action such as `list.config`.
    pub events: Vec<String>,
    /// Only this list's events; `None` for the whole site.
    #[serde(default)]
    pub list_id: Option<ListId>,
    #[serde(default)]
    pub description: String,
}

/// A webhook as the API shows it: never its secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Webhook {
    pub id: WebhookId,
    pub url: String,
    pub description: String,
    pub events: Vec<String>,
    pub list_id: Option<ListId>,
    pub enabled: bool,
    /// The first eight hex digits of the secret's SHA-256, so a rotated
    /// secret can be told from the old one without showing either.
    pub secret_fingerprint: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// The fields an update may change; a field left `None` is kept.
#[derive(Debug, Clone, Default, Serialize, Deserialize, utoipa::ToSchema)]
pub struct WebhookPatch {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub events: Option<Vec<String>>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// Where a delivery stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    Pending,
    Delivered,
    Failed,
}

impl DeliveryState {
    fn parse(value: &str) -> Result<Self> {
        Ok(match value {
            "pending" => Self::Pending,
            "delivered" => Self::Delivered,
            "failed" => Self::Failed,
            other => return Err(Error::Validation(format!("delivery state {other}"))),
        })
    }
}

/// One event owed to, or posted to, a webhook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Delivery {
    pub id: String,
    pub webhook_id: WebhookId,
    pub event: String,
    pub list_id: Option<String>,
    #[schema(value_type = Object)]
    pub payload: serde_json::Value,
    pub state: DeliveryState,
    pub attempts: i64,
    /// When it is next due, in milliseconds.
    pub next_attempt_at: i64,
    /// The last HTTP status the target answered, when it answered.
    pub last_status: Option<i64>,
    pub last_error: Option<String>,
    pub created_at: i64,
    pub finished_at: Option<i64>,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// The webhook's secret: HKDF-SHA256 (RFC 5869) of the site's signing key,
/// extracted with the webhook's salt and expanded one block with its id,
/// as 64 hex digits. The same inputs always give the same secret, so it
/// can be shown once and derived again when a delivery is signed.
fn derive_secret(key: &str, id: WebhookId, salt: &str) -> String {
    let prk = Hmac::<Sha256>::new_from_slice(salt.as_bytes())
        .expect("HMAC takes a key of any length")
        .chain_update(key.as_bytes())
        .finalize()
        .into_bytes();
    let okm = Hmac::<Sha256>::new_from_slice(&prk)
        .expect("HMAC takes a key of any length")
        .chain_update(b"listmngr webhook ")
        .chain_update(id.to_string().as_bytes())
        .chain_update([1u8])
        .finalize()
        .into_bytes();
    hex(&okm)
}

fn secret_hash(secret: &str) -> String {
    hex(&Sha256::digest(secret.as_bytes()))
}

fn fresh_salt() -> String {
    hex(&rand::random::<[u8; 16]>())
}

fn validate_url(url: &str, allow_http: bool) -> Result<String> {
    let url = url.trim();
    let invalid = || Error::Validation("url".into());
    if url.is_empty()
        || url.len() > MAX_URL_BYTES
        || url.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(invalid());
    }
    let rest = match url.strip_prefix("https://") {
        Some(rest) => rest,
        None if allow_http => url.strip_prefix("http://").ok_or_else(invalid)?,
        None => return Err(Error::Validation("url must be https://".into())),
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return Err(invalid());
    }
    Ok(url.to_owned())
}

fn validate_events(events: &[String]) -> Result<Vec<String>> {
    if events.is_empty() || events.len() > MAX_EVENTS {
        return Err(Error::Validation("events".into()));
    }
    let mut out: Vec<String> = Vec::with_capacity(events.len());
    for event in events {
        let event = event.trim();
        let name = event.strip_suffix(".*").unwrap_or(event);
        let well_formed = event == "*"
            || (!name.is_empty()
                && name.len() <= MAX_EVENT_BYTES
                && name.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.'
                })
                && !name.starts_with('.')
                && !name.ends_with('.')
                && !name.contains(".."));
        if !well_formed {
            return Err(Error::Validation(format!("events: {event:?}")));
        }
        if !out.iter().any(|known| known == event) {
            out.push(event.to_owned());
        }
    }
    Ok(out)
}

fn validate_description(description: &str) -> Result<String> {
    let description = description.trim();
    if description.len() > MAX_DESCRIPTION_BYTES || description.chars().any(char::is_control) {
        return Err(Error::Validation("description".into()));
    }
    Ok(description.to_owned())
}

/// Whether `action` is one of the events `patterns` subscribe to.
#[must_use]
pub fn event_matches(patterns: &[String], action: &str) -> bool {
    patterns.iter().any(|pattern| {
        pattern == "*"
            || pattern == action
            || pattern.strip_suffix(".*").is_some_and(|prefix| {
                action
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('.'))
            })
    })
}

fn decode_webhook(row: &sqlx::any::AnyRow) -> Result<Webhook> {
    let id: String = row.try_get("id").map_err(db_error)?;
    let events: String = row.try_get("events").map_err(db_error)?;
    let list_id: Option<String> = row.try_get("list_id").map_err(db_error)?;
    let enabled: i64 = row.try_get("enabled").map_err(db_error)?;
    let hash: String = row.try_get("secret_hash").map_err(db_error)?;
    Ok(Webhook {
        id: WebhookId(crate::parse_uuid(&id)?),
        url: row.try_get("url").map_err(db_error)?,
        description: row.try_get("description").map_err(db_error)?,
        events: serde_json::from_str(&events).unwrap_or_default(),
        list_id: list_id.map(|value| value.parse()).transpose()?,
        enabled: enabled != 0,
        secret_fingerprint: hash.chars().take(8).collect(),
        created_at: row.try_get("created_at").map_err(db_error)?,
        updated_at: row.try_get("updated_at").map_err(db_error)?,
    })
}

fn decode_delivery(row: &sqlx::any::AnyRow) -> Result<Delivery> {
    let webhook_id: String = row.try_get("webhook_id").map_err(db_error)?;
    let payload: String = row.try_get("payload").map_err(db_error)?;
    let state: String = row.try_get("state").map_err(db_error)?;
    Ok(Delivery {
        id: row.try_get("id").map_err(db_error)?,
        webhook_id: WebhookId(crate::parse_uuid(&webhook_id)?),
        event: row.try_get("event").map_err(db_error)?,
        list_id: row.try_get("list_id").map_err(db_error)?,
        payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
        state: DeliveryState::parse(&state)?,
        attempts: row.try_get("attempts").map_err(db_error)?,
        next_attempt_at: row.try_get("next_attempt_at").map_err(db_error)?,
        last_status: row.try_get("last_status").map_err(db_error)?,
        last_error: row.try_get("last_error").map_err(db_error)?,
        created_at: row.try_get("created_at").map_err(db_error)?,
        finished_at: row.try_get("finished_at").map_err(db_error)?,
    })
}

/// The list an audit event is about, when it is about one: the target
/// itself, the `list_id` its diff names, or the list the member or held
/// message belongs to.
async fn event_list(
    tx: &mut Transaction<'_, Any>,
    target_type: &str,
    target_id: &str,
    diff: &serde_json::Value,
) -> Result<Option<String>> {
    if target_type == "list" {
        return Ok(Some(target_id.to_owned()));
    }
    if let Some(list) = diff.get("list_id").and_then(serde_json::Value::as_str) {
        return Ok(Some(list.to_owned()));
    }
    let sql = match target_type {
        "member" => "SELECT list_id FROM members WHERE id=$1",
        "held_message" => "SELECT list_id FROM held_messages WHERE id=$1",
        _ => return Ok(None),
    };
    sqlx::query_scalar(sql)
        .bind(target_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_error)
}

/// Leave one pending delivery per enabled webhook that subscribes to
/// `action` — for the whole site, or for the list the event is about —
/// in the transaction that records the audit event. `diff` is the
/// redacted diff the audit row keeps.
pub(crate) async fn fan_out(
    tx: &mut Transaction<'_, Any>,
    context: &AuditContext,
    at: &str,
    action: &str,
    target_type: &str,
    target_id: &str,
    diff: &serde_json::Value,
) -> Result<()> {
    let hooks = sqlx::query("SELECT id, events, list_id FROM webhooks WHERE enabled=1")
        .fetch_all(&mut **tx)
        .await
        .map_err(db_error)?;
    if hooks.is_empty() {
        return Ok(());
    }
    let mut about: Option<Option<String>> = None;
    let stamp = now_ms();
    for hook in &hooks {
        let events: String = hook.try_get("events").map_err(db_error)?;
        let events: Vec<String> = serde_json::from_str(&events).unwrap_or_default();
        if !event_matches(&events, action) {
            continue;
        }
        let list = if let Some(list) = &about {
            list.clone()
        } else {
            let list = event_list(tx, target_type, target_id, diff).await?;
            about = Some(list.clone());
            list
        };
        let hook_list: Option<String> = hook.try_get("list_id").map_err(db_error)?;
        if let Some(hook_list) = &hook_list
            && list.as_deref() != Some(hook_list.as_str())
        {
            continue;
        }
        let hook_id: String = hook.try_get("id").map_err(db_error)?;
        let id = Uuid::now_v7().to_string();
        let payload = serde_json::json!({
            "id": id,
            "event": action,
            "at": at,
            "list_id": list,
            "target": {"type": target_type, "id": target_id},
            "actor": {"user_id": context.user_id, "token_id": context.token_id},
            "data": diff,
        });
        insert_delivery(tx, &id, &hook_id, action, list.as_deref(), &payload, stamp).await?;
    }
    Ok(())
}

async fn insert_delivery(
    tx: &mut Transaction<'_, Any>,
    id: &str,
    webhook_id: &str,
    event: &str,
    list: Option<&str>,
    payload: &serde_json::Value,
    stamp: i64,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO webhook_deliveries(id,webhook_id,event,list_id,payload,state,attempts,next_attempt_at,created_at) VALUES($1,$2,$3,$4,$5,'pending',0,$6,$6)",
    )
    .bind(id)
    .bind(webhook_id)
    .bind(event)
    .bind(list)
    .bind(payload.to_string())
    .bind(stamp)
    .execute(&mut **tx)
    .await
    .map_err(db_error)?;
    Ok(())
}

/// The webhooks of the site and their deliveries.
#[derive(Debug)]
pub struct WebhookRepo<'a> {
    pub(crate) db: &'a Database,
}

impl Database {
    #[must_use]
    pub const fn webhooks(&self) -> WebhookRepo<'_> {
        WebhookRepo { db: self }
    }
}

impl WebhookRepo<'_> {
    fn key(&self) -> Result<&str> {
        self.db
            .webhook_key()
            .ok_or_else(|| Error::Validation("webhooks.signing_key is not configured".into()))
    }

    async fn fetch(tx: &mut Transaction<'_, Any>, id: WebhookId) -> Result<Webhook> {
        let row = sqlx::query("SELECT * FROM webhooks WHERE id=$1")
            .bind(id.to_string())
            .fetch_optional(&mut **tx)
            .await
            .map_err(db_error)?
            .ok_or_else(|| Error::NotFound(format!("webhook {id}")))?;
        decode_webhook(&row)
    }

    /// Create a webhook and return it with its secret, which is shown
    /// this once: the database keeps only its hash and salt.
    /// # Errors
    /// Validation for a URL that is not `https://` (unless `allow_http`),
    /// carries userinfo or is malformed, for an event pattern that is not
    /// `*`, `name.*` or `name`, or when no signing key is configured;
    /// `NotFound` for an unknown list; database errors.
    pub async fn create_with_context(
        &self,
        new: NewWebhook,
        context: &AuditContext,
    ) -> Result<(Webhook, String)> {
        let key = self.key()?;
        let url = validate_url(&new.url, self.db.webhook_allow_http())?;
        let events = validate_events(&new.events)?;
        let description = validate_description(&new.description)?;
        let id = WebhookId::new();
        let salt = fresh_salt();
        let secret = derive_secret(key, id, &salt);
        let stamp = now_ms();
        let mut tx = self.db.write_tx().await?;
        if let Some(list) = &new.list_id {
            let known: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM mailing_lists WHERE list_id=$1")
                    .bind(list.as_str())
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(db_error)?;
            if known == 0 {
                return Err(Error::NotFound(list.to_string()));
            }
        }
        sqlx::query(
            "INSERT INTO webhooks(id,url,description,events,list_id,enabled,secret_hash,secret_salt,created_at,updated_at) VALUES($1,$2,$3,$4,$5,1,$6,$7,$8,$8)",
        )
        .bind(id.to_string())
        .bind(&url)
        .bind(&description)
        .bind(serde_json::to_string(&events).unwrap_or_default())
        .bind(new.list_id.as_ref().map(ListId::as_str))
        .bind(secret_hash(&secret))
        .bind(&salt)
        .bind(stamp)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        Database::record_tx_with_context(
            &mut tx,
            context,
            "webhook.create",
            "webhook",
            &id.to_string(),
            serde_json::json!({"url": url, "events": events, "list_id": new.list_id, "description": description}),
        )
        .await?;
        let webhook = Self::fetch(&mut tx, id).await?;
        tx.commit().await.map_err(db_error)?;
        Ok((webhook, secret))
    }

    /// # Errors
    /// `NotFound` for an unknown webhook; database errors.
    pub async fn get(&self, id: WebhookId) -> Result<Webhook> {
        let row = sqlx::query("SELECT * FROM webhooks WHERE id=$1")
            .bind(id.to_string())
            .fetch_optional(self.db.pool())
            .await
            .map_err(db_error)?
            .ok_or_else(|| Error::NotFound(format!("webhook {id}")))?;
        decode_webhook(&row)
    }

    /// Every webhook, or the ones bound to `list`, oldest first.
    /// # Errors
    /// Database errors.
    pub async fn list(&self, list: Option<&ListId>) -> Result<Vec<Webhook>> {
        let rows = match list {
            Some(list) => {
                sqlx::query("SELECT * FROM webhooks WHERE list_id=$1 ORDER BY created_at, id")
                    .bind(list.as_str())
                    .fetch_all(self.db.pool())
                    .await
            }
            None => {
                sqlx::query("SELECT * FROM webhooks ORDER BY created_at, id")
                    .fetch_all(self.db.pool())
                    .await
            }
        }
        .map_err(db_error)?;
        rows.iter().map(decode_webhook).collect()
    }

    /// Change what a patch names and audit the change.
    /// # Errors
    /// Validation as for [`Self::create_with_context`]; `NotFound`;
    /// database errors.
    pub async fn update_with_context(
        &self,
        id: WebhookId,
        patch: WebhookPatch,
        context: &AuditContext,
    ) -> Result<Webhook> {
        let mut tx = self.db.write_tx().await?;
        let current = Self::fetch(&mut tx, id).await?;
        let url = match &patch.url {
            Some(url) => validate_url(url, self.db.webhook_allow_http())?,
            None => current.url.clone(),
        };
        let events = match &patch.events {
            Some(events) => validate_events(events)?,
            None => current.events.clone(),
        };
        let description = match &patch.description {
            Some(description) => validate_description(description)?,
            None => current.description.clone(),
        };
        let enabled = patch.enabled.unwrap_or(current.enabled);
        let stamp = now_ms();
        sqlx::query(
            "UPDATE webhooks SET url=$1, events=$2, description=$3, enabled=$4, updated_at=$5 WHERE id=$6",
        )
        .bind(&url)
        .bind(serde_json::to_string(&events).unwrap_or_default())
        .bind(&description)
        .bind(i64::from(enabled))
        .bind(stamp)
        .bind(id.to_string())
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        let mut diff = serde_json::Map::new();
        if url != current.url {
            diff.insert(
                "url".into(),
                serde_json::json!({"from": current.url, "to": url}),
            );
        }
        if events != current.events {
            diff.insert(
                "events".into(),
                serde_json::json!({"from": current.events, "to": events}),
            );
        }
        if description != current.description {
            diff.insert(
                "description".into(),
                serde_json::json!({"from": current.description, "to": description}),
            );
        }
        if enabled != current.enabled {
            diff.insert(
                "enabled".into(),
                serde_json::json!({"from": current.enabled, "to": enabled}),
            );
        }
        Database::record_tx_with_context(
            &mut tx,
            context,
            "webhook.update",
            "webhook",
            &id.to_string(),
            serde_json::Value::Object(diff),
        )
        .await?;
        let webhook = Self::fetch(&mut tx, id).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(webhook)
    }

    /// Delete a webhook and every delivery it was owed.
    /// # Errors
    /// `NotFound`; database errors.
    pub async fn delete_with_context(&self, id: WebhookId, context: &AuditContext) -> Result<()> {
        let mut tx = self.db.write_tx().await?;
        let webhook = Self::fetch(&mut tx, id).await?;
        sqlx::query("DELETE FROM webhook_deliveries WHERE webhook_id=$1")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        sqlx::query("DELETE FROM webhooks WHERE id=$1")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        Database::record_tx_with_context(
            &mut tx,
            context,
            "webhook.delete",
            "webhook",
            &id.to_string(),
            serde_json::json!({"url": webhook.url, "list_id": webhook.list_id}),
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }

    /// Give the webhook a new secret — a new salt, the same key — and
    /// return it, shown this once. Deliveries still pending are signed
    /// with the new secret.
    /// # Errors
    /// Validation when no signing key is configured; `NotFound`;
    /// database errors.
    pub async fn rotate_with_context(
        &self,
        id: WebhookId,
        context: &AuditContext,
    ) -> Result<String> {
        let key = self.key()?;
        let salt = fresh_salt();
        let secret = derive_secret(key, id, &salt);
        let mut tx = self.db.write_tx().await?;
        let before = Self::fetch(&mut tx, id).await?;
        sqlx::query(
            "UPDATE webhooks SET secret_hash=$1, secret_salt=$2, updated_at=$3 WHERE id=$4",
        )
        .bind(secret_hash(&secret))
        .bind(&salt)
        .bind(now_ms())
        .bind(id.to_string())
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        Database::record_tx_with_context(
            &mut tx,
            context,
            "webhook.rotate",
            "webhook",
            &id.to_string(),
            serde_json::json!({"previous_fingerprint": before.secret_fingerprint}),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(secret)
    }

    /// The webhook's current secret, derived again for signing a delivery.
    /// # Errors
    /// Validation when no signing key is configured; `NotFound`;
    /// database errors.
    pub async fn secret(&self, id: WebhookId) -> Result<String> {
        let key = self.key()?;
        let salt: Option<String> =
            sqlx::query_scalar("SELECT secret_salt FROM webhooks WHERE id=$1")
                .bind(id.to_string())
                .fetch_optional(self.db.pool())
                .await
                .map_err(db_error)?;
        let salt = salt.ok_or_else(|| Error::NotFound(format!("webhook {id}")))?;
        Ok(derive_secret(key, id, &salt))
    }

    /// Queue a `ping` delivery, so an operator can see the target answer
    /// without waiting for an event.
    /// # Errors
    /// `NotFound`; database errors.
    pub async fn ping_with_context(
        &self,
        id: WebhookId,
        context: &AuditContext,
    ) -> Result<Delivery> {
        let mut tx = self.db.write_tx().await?;
        let webhook = Self::fetch(&mut tx, id).await?;
        let created = now_ms();
        let delivery_id = Uuid::now_v7().to_string();
        let payload = serde_json::json!({
            "id": delivery_id,
            "event": "ping",
            "at": chrono::Utc::now().to_rfc3339(),
            "list_id": webhook.list_id,
            "target": {"type": "webhook", "id": id},
            "actor": {"user_id": context.user_id, "token_id": context.token_id},
            "data": {},
        });
        insert_delivery(
            &mut tx,
            &delivery_id,
            &id.to_string(),
            "ping",
            webhook.list_id.as_ref().map(ListId::as_str),
            &payload,
            created,
        )
        .await?;
        Database::record_tx_with_context(
            &mut tx,
            context,
            "webhook.ping",
            "webhook",
            &id.to_string(),
            serde_json::json!({"delivery_id": delivery_id}),
        )
        .await?;
        let row = sqlx::query("SELECT * FROM webhook_deliveries WHERE id=$1")
            .bind(&delivery_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(db_error)?;
        let delivery = decode_delivery(&row)?;
        tx.commit().await.map_err(db_error)?;
        Ok(delivery)
    }

    /// The webhook's deliveries, newest first, at most `limit`.
    /// # Errors
    /// `NotFound`; database errors.
    pub async fn deliveries(&self, id: WebhookId, limit: i64) -> Result<Vec<Delivery>> {
        self.get(id).await?;
        let rows = sqlx::query(
            "SELECT * FROM webhook_deliveries WHERE webhook_id=$1 ORDER BY created_at DESC, id DESC LIMIT $2",
        )
        .bind(id.to_string())
        .bind(limit.clamp(1, 1000))
        .fetch_all(self.db.pool())
        .await
        .map_err(db_error)?;
        rows.iter().map(decode_delivery).collect()
    }

    /// One delivery by id.
    /// # Errors
    /// `NotFound`; database errors.
    pub async fn delivery(&self, id: &str) -> Result<Delivery> {
        let row = sqlx::query("SELECT * FROM webhook_deliveries WHERE id=$1")
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
            .map_err(db_error)?
            .ok_or_else(|| Error::NotFound(format!("delivery {id}")))?;
        decode_delivery(&row)
    }

    /// A list's webhooks and their deliveries go with the list.
    pub(crate) async fn delete_list_tx(tx: &mut Transaction<'_, Any>, list: &ListId) -> Result<()> {
        sqlx::query(
            "DELETE FROM webhook_deliveries WHERE webhook_id IN (SELECT id FROM webhooks WHERE list_id=$1)",
        )
        .bind(list.as_str())
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
        sqlx::query("DELETE FROM webhooks WHERE list_id=$1")
            .bind(list.as_str())
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
        Ok(())
    }
}

/// What became of one attempt to post a delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The target answered 2xx.
    Delivered { status: i64 },
    /// Try again at `next_attempt_at`.
    Retry {
        status: Option<i64>,
        error: String,
        next_attempt_at: i64,
    },
    /// Given up: too many attempts, or a target that can never be reached.
    Failed { status: Option<i64>, error: String },
}

impl WebhookRepo<'_> {
    /// Claim the next delivery that is due — pending, past `next_attempt_at`,
    /// not leased, and for a webhook that is enabled — for `lease_ms`,
    /// counting the attempt. Two runners never claim the same delivery:
    /// `BEGIN IMMEDIATE` on `SQLite`, `FOR UPDATE SKIP LOCKED` on `PostgreSQL`.
    /// # Errors
    /// Validation for a non-positive lease; database errors.
    pub async fn claim_due(
        &self,
        now_ms: i64,
        lease_ms: i64,
    ) -> Result<Option<(Delivery, Webhook)>> {
        if lease_ms <= 0 {
            return Err(Error::Validation("lease must be positive".into()));
        }
        let mut tx = self.db.write_tx().await?;
        let lock = if self.db.sqlite {
            ""
        } else {
            " FOR UPDATE OF d SKIP LOCKED"
        };
        let sql = format!(
            "SELECT d.id AS id FROM webhook_deliveries d JOIN webhooks w ON w.id=d.webhook_id \
             WHERE d.state='pending' AND d.next_attempt_at<=$1 AND (d.leased_until IS NULL OR d.leased_until<=$1) AND w.enabled=1 \
             ORDER BY d.next_attempt_at, d.id LIMIT 1{lock}"
        );
        let id: Option<String> = sqlx::query_scalar(&sql)
            .bind(now_ms)
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_error)?;
        let Some(id) = id else {
            tx.commit().await.map_err(db_error)?;
            return Ok(None);
        };
        let row = sqlx::query(
            "UPDATE webhook_deliveries SET leased_until=$1, attempts=attempts+1 WHERE id=$2 RETURNING *",
        )
        .bind(now_ms.saturating_add(lease_ms))
        .bind(&id)
        .fetch_one(&mut *tx)
        .await
        .map_err(db_error)?;
        let delivery = decode_delivery(&row)?;
        let webhook = Self::fetch(&mut tx, delivery.webhook_id).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(Some((delivery, webhook)))
    }

    /// Record what became of a claimed delivery and release its lease.
    /// # Errors
    /// `NotFound` when the delivery is not pending any more; database
    /// errors.
    pub async fn record(&self, id: &str, outcome: &Outcome, now_ms: i64) -> Result<Delivery> {
        let mut tx = self.db.write_tx().await?;
        let updated = match outcome {
            Outcome::Delivered { status } => {
                sqlx::query("UPDATE webhook_deliveries SET state='delivered', leased_until=NULL, last_status=$1, last_error=NULL, finished_at=$2 WHERE id=$3 AND state='pending'")
                    .bind(*status)
                    .bind(now_ms)
                    .bind(id)
                    .execute(&mut *tx)
                    .await
            }
            Outcome::Retry { status, error, next_attempt_at } => {
                sqlx::query("UPDATE webhook_deliveries SET leased_until=NULL, last_status=$1, last_error=$2, next_attempt_at=$3 WHERE id=$4 AND state='pending'")
                    .bind(*status)
                    .bind(error)
                    .bind(*next_attempt_at)
                    .bind(id)
                    .execute(&mut *tx)
                    .await
            }
            Outcome::Failed { status, error } => {
                sqlx::query("UPDATE webhook_deliveries SET state='failed', leased_until=NULL, last_status=$1, last_error=$2, finished_at=$3 WHERE id=$4 AND state='pending'")
                    .bind(*status)
                    .bind(error)
                    .bind(now_ms)
                    .bind(id)
                    .execute(&mut *tx)
                    .await
            }
        }
        .map_err(db_error)?;
        if updated.rows_affected() == 0 {
            return Err(Error::NotFound(format!("pending delivery {id}")));
        }
        let row = sqlx::query("SELECT * FROM webhook_deliveries WHERE id=$1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(db_error)?;
        let delivery = decode_delivery(&row)?;
        tx.commit().await.map_err(db_error)?;
        Ok(delivery)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_match_exact_names_prefixes_and_everything() {
        let patterns = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert!(event_matches(&patterns(&["*"]), "anything.at.all"));
        assert!(event_matches(&patterns(&["member.*"]), "member.create"));
        assert!(!event_matches(&patterns(&["member.*"]), "members.create"));
        assert!(!event_matches(&patterns(&["member.*"]), "member"));
        assert!(event_matches(&patterns(&["list.config"]), "list.config"));
        assert!(!event_matches(&patterns(&["list.config"]), "list.create"));
        assert!(
            validate_events(&patterns(&["*", "member.*", "list.config", "list.config"]))
                .unwrap()
                .len()
                == 3
        );
        for bad in [
            "",
            ".member",
            "member.",
            "member..x",
            "Member.*",
            "member *",
            "**",
        ] {
            assert!(validate_events(&patterns(&[bad])).is_err(), "{bad:?}");
        }
        assert!(validate_events(&[]).is_err());
    }

    #[test]
    fn urls_are_https_without_userinfo_unless_http_is_allowed() {
        assert!(validate_url("https://hooks.example.invalid/x?y=1", false).is_ok());
        assert!(validate_url("http://hooks.example.invalid/x", false).is_err());
        assert!(validate_url("http://hooks.example.invalid/x", true).is_ok());
        assert!(validate_url("https://user:pw@hooks.example.invalid/", false).is_err());
        assert!(validate_url("https:///x", false).is_err());
        assert!(validate_url("ftp://hooks.example.invalid/", true).is_err());
        assert!(validate_url("https://hooks.example.invalid/a b", false).is_err());
    }

    #[test]
    fn the_secret_is_a_function_of_key_id_and_salt() {
        let id = WebhookId::new();
        let a = derive_secret("k".repeat(32).as_str(), id, "salt");
        assert_eq!(a.len(), 64);
        assert_eq!(a, derive_secret("k".repeat(32).as_str(), id, "salt"));
        assert_ne!(a, derive_secret("k".repeat(32).as_str(), id, "other"));
        assert_ne!(a, derive_secret("j".repeat(32).as_str(), id, "salt"));
        assert_ne!(
            a,
            derive_secret("k".repeat(32).as_str(), WebhookId::new(), "salt")
        );
    }
}
