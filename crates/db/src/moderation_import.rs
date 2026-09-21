//! What was still waiting when another system was migrated: a message
//! held for a moderator, and a subscription nobody had decided.
//!
//! An import is not a fresh hold: the message keeps the date the old
//! system held it, the write is audited as `moderation.import` rather
//! than `moderation.hold`, and nobody is mailed — the moderators were
//! told about these once already, on the system being left behind.
use crate::mail_queue::MessageId;
use crate::moderation::{HeldId, HeldMessage, ModerationRepo};
use crate::workflows::{SubscriptionAction, TokenOwner, WorkflowRepo};
use crate::{AuditContext, Database, db_error};
use listmngr_core::{Address, Error, ListId, Result};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// A message the other system was holding.
#[derive(Debug, Clone, Copy)]
pub struct ImportedHold<'a> {
    pub list_id: &'a ListId,
    /// The message as the other system kept it.
    pub raw: &'a [u8],
    pub sender: &'a str,
    pub subject: &'a str,
    /// The other system's wording for why it was held.
    pub reason: &'a str,
    /// When it was held there, in milliseconds.
    pub hold_date: i64,
}

/// A subscription change the other system had not decided.
#[derive(Debug, Clone, Copy)]
pub struct ImportedRequest<'a> {
    pub list_id: &'a ListId,
    /// The mailbox as the requester wrote it.
    pub email: &'a str,
    pub display_name: &'a str,
    pub action: SubscriptionAction,
    /// When it was asked for there, in milliseconds.
    pub requested_at: i64,
}

impl ModerationRepo<'_> {
    /// Keep a message another system held, with its own hold date, for a
    /// moderator here. The bytes are stored whole, so accepting it later
    /// delivers exactly what was held.
    ///
    /// # Errors
    /// `NotFound` for an unknown list, `Conflict` when the same message
    /// is already held on the list, and the database's own errors.
    pub async fn hold_imported_with_context(
        &self,
        imported: ImportedHold<'_>,
        context: &AuditContext,
    ) -> Result<HeldMessage> {
        let db = self.db();
        db.lists().get(imported.list_id).await?;
        let key = format!("{:x}", Sha256::digest(imported.raw));
        let message_id = MessageId(Uuid::now_v7());
        let id = HeldId(Uuid::now_v7());
        let mut tx = db.write_tx().await?;
        let held_already: Option<String> = sqlx::query_scalar(
            "SELECT h.id FROM held_messages h JOIN messages m ON m.id=h.message_id WHERE h.list_id=$1 AND m.store_key=$2 AND h.disposition IS NULL",
        )
        .bind(imported.list_id.as_str())
        .bind(&key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?;
        if held_already.is_some() {
            return Err(Error::Conflict("the message is already held".into()));
        }
        sqlx::query(
            "INSERT INTO message_blobs(store_key,raw) VALUES($1,$2) ON CONFLICT(store_key) DO NOTHING",
        )
        .bind(&key)
        .bind(imported.raw)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        sqlx::query("INSERT INTO messages(id,store_key,external_id,context,created_at) VALUES($1,$2,'',$3,$4)")
            .bind(message_id.0.to_string())
            .bind(&key)
            .bind(
                serde_json::json!({
                    "version": 1,
                    "list_id": imported.list_id.as_str(),
                    "envelope_sender": imported.sender,
                    "imported": true,
                })
                .to_string(),
            )
            .bind(imported.hold_date)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        sqlx::query("INSERT INTO held_messages(id,list_id,message_id,sender,subject,reason,hold_date) VALUES($1,$2,$3,$4,$5,$6,$7)")
            .bind(id.0.to_string())
            .bind(imported.list_id.as_str())
            .bind(message_id.0.to_string())
            .bind(imported.sender)
            .bind(imported.subject)
            .bind(imported.reason)
            .bind(imported.hold_date)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        Database::record_tx_with_context(
            &mut tx,
            context,
            "moderation.import",
            "held_message",
            &id.0.to_string(),
            serde_json::json!({
                "list_id": imported.list_id.as_str(),
                "sender": imported.sender,
                "reason": imported.reason,
                "hold_date": imported.hold_date,
            }),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(HeldMessage {
            id,
            list_id: imported.list_id.clone(),
            message_id,
            sender: imported.sender.to_owned(),
            subject: imported.subject.to_owned(),
            reason: imported.reason.to_owned(),
            hold_date: imported.hold_date,
            disposition: None,
            moderator_id: None,
            disposed_at: None,
        })
    }
}

impl WorkflowRepo<'_> {
    /// Keep a subscription change another system had not decided, as a
    /// request waiting for a moderator here. No token is issued and
    /// nothing is mailed: whoever asked was already told by the other
    /// system, and a confirmation token from it cannot be honoured here.
    ///
    /// Returns the request's identifier (`/requests/{id}`).
    ///
    /// # Errors
    /// `NotFound` for an unknown list, `Validation` for a banned or
    /// unparsable address, and the database's own errors.
    pub async fn import_request_with_context(
        &self,
        imported: ImportedRequest<'_>,
        context: &AuditContext,
    ) -> Result<String> {
        let db = self.db();
        db.lists().get(imported.list_id).await?;
        let address = Address::new(imported.email, imported.display_name.to_owned())?;
        if db
            .bans()
            .is_banned(imported.list_id, &address.original_email)
            .await?
        {
            return Err(Error::Validation("address is banned".into()));
        }
        let id = Uuid::now_v7().to_string();
        // A token nobody holds: the moderator decides, and no
        // confirmation can arrive for a request made elsewhere.
        let hash = format!("{:x}", Sha256::digest(Uuid::now_v7().as_bytes()));
        let expires = imported
            .requested_at
            .checked_add(86_400_000)
            .ok_or_else(|| Error::Validation("invalid time".into()))?;
        let mut tx = db.write_tx().await?;
        sqlx::query("INSERT INTO subscription_workflows(id,list_id,email,action,token_hash,created_at,expires_at,original_email,state,display_name,pre_approved) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,0)")
            .bind(&id)
            .bind(imported.list_id.as_str())
            .bind(&address.email)
            .bind(imported.action.name())
            .bind(hash)
            .bind(imported.requested_at)
            .bind(expires)
            .bind(&address.original_email)
            .bind(TokenOwner::Moderator.state())
            .bind(&address.display_name)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        Database::record_tx_with_context(
            &mut tx,
            context,
            "subscription.import",
            "subscription_request",
            &id,
            serde_json::json!({
                "list_id": imported.list_id.as_str(),
                "email": address.email,
                "action": imported.action.name(),
                "requested_at": imported.requested_at,
            }),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(id)
    }
}
