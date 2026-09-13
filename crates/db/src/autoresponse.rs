//! Mailman's `replybot`: a list answers its owner, request and posting
//! addresses on its own.
//!
//! Each kind has its own `autorespond_*` action and text. A writer is
//! answered at most once per `autoresponse_grace_period` days per kind, and
//! `respond_and_discard` swallows the original whether or not a reply went
//! out this time. The record and the reply commit together, so a retried
//! job can never answer twice.
use crate::{AuditContext, Database, db_error};
use listmngr_core::{Address, ListId, ResponseAction, Result};
use sqlx::Row;
use uuid::Uuid;

/// Which of the list's addresses the message was sent to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseKind {
    /// `list-owner@`.
    Owner,
    /// `list@`: a post.
    Postings,
    /// `list-request@` and the other command addresses.
    Requests,
}

impl ResponseKind {
    const fn name(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Postings => "postings",
            Self::Requests => "requests",
        }
    }
    const fn action_column(self) -> &'static str {
        match self {
            Self::Owner => "autorespond_owner",
            Self::Postings => "autorespond_postings",
            Self::Requests => "autorespond_requests",
        }
    }
    const fn text_column(self) -> &'static str {
        match self {
            Self::Owner => "autoresponse_owner_text",
            Self::Postings => "autoresponse_postings_text",
            Self::Requests => "autoresponse_request_text",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AutoresponseRepo<'a> {
    db: &'a Database,
}

impl Database {
    #[must_use]
    pub const fn autoresponse(&self) -> AutoresponseRepo<'_> {
        AutoresponseRepo { db: self }
    }
}

const DAY_MS: i64 = 86_400_000;

impl AutoresponseRepo<'_> {
    /// Answer `to` for a message to the list's `kind` address, if the list
    /// does that and the grace period allows, and say what happens to the
    /// original. A null, malformed or list-owned writer is never answered.
    /// # Errors
    /// Returns `NotFound` for an unknown list and database errors.
    pub async fn respond(
        &self,
        list: &ListId,
        kind: ResponseKind,
        to: &str,
        now_ms: i64,
    ) -> Result<ResponseAction> {
        let mut tx = self.db.pool().begin().await.map_err(db_error)?;
        let row = sqlx::query(&format!(
            "SELECT {} AS action, {} AS text, autoresponse_grace_period FROM mailing_lists WHERE list_id=$1",
            kind.action_column(),
            kind.text_column()
        ))
        .bind(list.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?
        .ok_or_else(|| listmngr_core::Error::NotFound(list.to_string()))?;
        let action: ResponseAction = row
            .try_get::<String, _>("action")
            .map_err(db_error)?
            .parse()
            .map_err(|_| listmngr_core::Error::Validation("autorespond action".into()))?;
        if action == ResponseAction::None {
            return Ok(action);
        }
        let text: String = row.try_get("text").map_err(db_error)?;
        let grace_days: i64 = row.try_get("autoresponse_grace_period").map_err(db_error)?;
        let Some(writer) = answerable(to, list) else {
            return Ok(action);
        };
        // The grace period is per writer and kind; rows outside it are
        // pruned as they are met, keeping the table bounded.
        let grace_ms = grace_days.saturating_mul(DAY_MS);
        sqlx::query("DELETE FROM autoresponse_records WHERE responded_at<=$1")
            .bind(now_ms.saturating_sub(grace_ms))
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        if grace_ms > 0 {
            let recent: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM autoresponse_records WHERE list_id=$1 AND email=$2 AND kind=$3 AND responded_at>$4")
                .bind(list.as_str())
                .bind(&writer.email)
                .bind(kind.name())
                .bind(now_ms.saturating_sub(grace_ms))
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?;
            if recent > 0 {
                tx.commit().await.map_err(db_error)?;
                return Ok(action);
            }
        }
        sqlx::query("INSERT INTO autoresponse_records(list_id,email,kind,responded_at) VALUES($1,$2,$3,$4) ON CONFLICT(list_id,email,kind) DO UPDATE SET responded_at=excluded.responded_at")
            .bind(list.as_str())
            .bind(&writer.email)
            .bind(kind.name())
            .bind(now_ms)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        enqueue_reply(
            &mut tx,
            self.db,
            &Reply {
                list,
                kind,
                writer: &writer,
                text: &text,
                now_ms,
            },
        )
        .await?;
        Database::record_tx_with_context(
            &mut tx,
            &AuditContext::system(),
            "list.autoresponse",
            "list",
            list.as_str(),
            serde_json::json!({"kind": kind.name(), "action": action.as_str()}),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(action)
    }
}

/// The writer as a deliverable mailbox: never a null sender, never a
/// malformed address, never one of the list's own addresses.
fn answerable(to: &str, list: &ListId) -> Option<Address> {
    let address = Address::new(to, String::new()).ok()?;
    if !listmngr_mail::owner::safe_mailbox(&address.original_email)
        || listmngr_mail::owner::points_to_list(&address.original_email, list)
    {
        return None;
    }
    Some(address)
}

/// One automatic reply about to be composed.
struct Reply<'a> {
    list: &'a ListId,
    kind: ResponseKind,
    writer: &'a Address,
    /// The owner's configured text; empty means the built-in template.
    text: &'a str,
    now_ms: i64,
}

/// Compose the reply in the writer's language and queue it.
async fn enqueue_reply(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    db: &Database,
    reply: &Reply<'_>,
) -> Result<()> {
    let snapshot = crate::notices::list_snapshot(tx, reply.list).await?;
    let language = crate::notices::recipient_language(
        tx,
        &snapshot,
        &reply.writer.original_email,
        db.default_language(),
    )
    .await?;
    let subject = listmngr_i18n::message(
        &language,
        "notice-autoresponse-subject",
        &[("display_name", &snapshot.display_name)],
    );
    let values = crate::notices::list_placeholders(&snapshot);
    let body = if reply.text.trim().is_empty() {
        crate::notices::render(
            tx,
            &snapshot,
            "list:user:notice:autoresponse",
            &language,
            &values,
        )
        .await?
    } else {
        listmngr_mail::templates::expand(reply.text, &values)
    };
    let id = Uuid::now_v7().to_string();
    let date = chrono::DateTime::from_timestamp_millis(reply.now_ms)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc2822();
    let host = reply.list.mail_host().to_owned();
    let from = match reply.kind {
        ResponseKind::Owner => reply.list.owner_address(),
        ResponseKind::Postings => reply.list.bounces_address(),
        ResponseKind::Requests => reply.list.request_address(),
    };
    let message = crate::notices::serialize(
        &crate::notices::Envelope {
            from: &from,
            to: &reply.writer.original_email,
            reply_to: None,
            subject: &subject,
            message_id_local: &id,
            mail_host: &host,
            date: &date,
            auto_submitted: "auto-replied",
        },
        &body,
    )?;
    crate::workflows::enqueue_notice(
        tx,
        reply.list,
        &reply.writer.original_email,
        &id,
        message,
        reply.now_ms,
    )
    .await
}
