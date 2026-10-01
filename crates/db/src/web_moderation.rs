//! The moderator's queues: held posts with what the page shows of them,
//! decisions one or many at a time, the sender's moderation and ban taken
//! from a held post, and the subscription requests queue.
//!
//! Every write checks the moderator's live authority on the list, changes
//! the rows and records the audit event in one transaction.
use crate::bans::BanRepo;
use crate::moderation::{HeldId, HeldMessage, ModerationRepo, ReviewAction, decode_held};
use crate::web_sessions::WebSession;
use crate::workflows::{PendingRequest, RequestDecision, WorkflowRepo, pending_row};
use crate::{AuditContext, Database, db_error};
use listmngr_core::{Address, Error, ListId, MemberId, ModerationAction, Result, UserId};
use sqlx::Row;
use uuid::Uuid;

/// The most raw bytes the page reads of one held post (the literal in the
/// query below).
pub const HELD_SOURCE_LIMIT: usize = 65_536;

/// One held post as the queue shows it.
#[derive(Debug, Clone)]
pub struct HeldPreview {
    pub held: HeldMessage,
    /// Decoded `From`, `To` and `Date` headers, as text.
    pub from: String,
    pub to: String,
    pub date: String,
    /// The first text body, decoded; empty when there is none.
    pub body: String,
    /// The raw source, bounded.
    pub raw: String,
    /// How many attachments the MIME structure names.
    pub attachments: usize,
    /// The sender's row on this list: role and moderation override.
    pub sender_row: Option<(String, Option<ModerationAction>)>,
    /// Whether the sender's address is banned on this list already.
    pub sender_banned: bool,
}

/// One list the reader moderates, with what waits on it.
#[derive(Debug, Clone)]
pub struct ModeratedList {
    pub id: String,
    pub display_name: String,
    pub held: i64,
    pub requests: i64,
}

impl Database {
    pub(crate) async fn moderator_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Any>,
        session: &WebSession,
        list: &ListId,
    ) -> Result<UserId> {
        let user = Self::browser_user_tx(tx, session).await?;
        let allowed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users u WHERE u.id=$1 AND ((u.is_server_owner=1 AND EXISTS (SELECT 1 FROM addresses a WHERE a.user_id=u.id AND a.verified_on IS NOT NULL)) OR EXISTS (SELECT 1 FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id=$2 AND a.user_id=u.id AND a.verified_on IS NOT NULL AND (m.subscription_mode='as_address' OR m.user_id=u.id) AND m.role IN ('owner','moderator')))")
            .bind(user.to_string()).bind(list.as_str()).fetch_one(&mut **tx).await.map_err(db_error)?;
        if allowed != 1 {
            return Err(Error::Forbidden("browser moderator authority".into()));
        }
        Ok(user)
    }

    async fn moderator_check(&self, session: &WebSession, list: &ListId) -> Result<UserId> {
        let mut tx = self.browser_write_tx().await?;
        let user = Self::moderator_tx(&mut tx, session, list).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(user)
    }

    /// The lists the reader moderates (one page of 21), with the counts of
    /// held posts and undecided requests on each.
    /// # Errors
    /// Rejects a stale or anonymous session and database failures.
    pub async fn browser_moderated_lists(
        &self,
        session: &WebSession,
        offset: i64,
    ) -> Result<Vec<ModeratedList>> {
        crate::web_admin::valid_offset(offset)?;
        let live = self
            .web_session(&session.token, chrono::Utc::now().timestamp_millis())
            .await?;
        let user = live.user_id.ok_or(Error::Authentication)?;
        let rows = sqlx::query(
            "SELECT l.list_id,l.display_name,
             (SELECT COUNT(*) FROM held_messages h WHERE h.list_id=l.list_id AND h.disposition IS NULL) AS held,
             (SELECT COUNT(*) FROM subscription_workflows w WHERE w.list_id=l.list_id AND w.state IN ('pending_confirmation','pending_moderation')) AS requests
             FROM mailing_lists l WHERE
             EXISTS (SELECT 1 FROM users u JOIN addresses a ON a.user_id=u.id
                     WHERE u.id=$1 AND u.is_server_owner=1 AND a.verified_on IS NOT NULL)
             OR EXISTS (SELECT 1 FROM members m JOIN addresses a ON a.id=m.address_id
                        WHERE m.list_id=l.list_id AND a.user_id=$1 AND a.verified_on IS NOT NULL
                        AND (m.subscription_mode='as_address' OR m.user_id=$1)
                        AND m.role IN ('owner','moderator'))
             ORDER BY l.list_id LIMIT 21 OFFSET $2",
        )
        .bind(user.to_string())
        .bind(offset)
        .fetch_all(self.pool())
        .await
        .map_err(db_error)?;
        rows.iter()
            .map(|row| {
                Ok(ModeratedList {
                    id: row.try_get("list_id").map_err(db_error)?,
                    display_name: row.try_get("display_name").map_err(db_error)?,
                    held: row.try_get("held").map_err(db_error)?,
                    requests: row.try_get("requests").map_err(db_error)?,
                })
            })
            .collect()
    }

    /// One page (21 rows) of the list's held posts with what the page shows
    /// of each: decoded headers, the text body, the bounded source, the
    /// sender's standing on the list.
    /// # Errors
    /// Rejects revoked authority, a bad offset and database failures.
    pub async fn browser_held_queue(
        &self,
        session: &WebSession,
        list: &ListId,
        offset: i64,
    ) -> Result<Vec<HeldPreview>> {
        crate::web_admin::valid_offset(offset)?;
        self.moderator_check(session, list).await?;
        let rows = sqlx::query("SELECT h.*, m.store_key AS store_key FROM held_messages h JOIN messages m ON m.id=h.message_id WHERE h.list_id=$1 AND h.disposition IS NULL ORDER BY h.hold_date,h.id LIMIT 21 OFFSET $2")
            .bind(list.as_str()).bind(offset)
            .fetch_all(self.pool()).await.map_err(db_error)?;
        let mut previews = Vec::with_capacity(rows.len());
        for row in &rows {
            let held = decode_held(row)?;
            let store_key: String = row.try_get("store_key").map_err(db_error)?;
            let mut source = self.blobs().get(self.pool(), &store_key).await?;
            source.truncate(65_536);
            let parsed = mail_parser::MessageParser::default().parse(&source);
            let mailbox = |address: Option<&mail_parser::Address<'_>>| -> String {
                address
                    .map(|address| {
                        address
                            .iter()
                            .map(|addr| match (addr.name(), addr.address()) {
                                (Some(name), Some(email)) => format!("{name} <{email}>"),
                                (None, Some(email)) => email.to_owned(),
                                (Some(name), None) => name.to_owned(),
                                (None, None) => String::new(),
                            })
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default()
            };
            let body = parsed
                .as_ref()
                .and_then(|message| message.body_text(0))
                .map(|text| text.chars().take(65_536).collect::<String>())
                .unwrap_or_default();
            let attachments = listmngr_mail::attachments::names(&source)
                .map(|names| names.len())
                .unwrap_or(0);
            let sender = Address::new(held.sender.trim(), String::new()).map_or_else(
                |_| held.sender.to_ascii_lowercase(),
                |address| address.email,
            );
            let sender_row: Option<(String, Option<String>)> = sqlx::query_as(
                "SELECT m.role, m.moderation_action FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id=$1 AND a.email=$2 ORDER BY CASE m.role WHEN 'member' THEN 0 ELSE 1 END LIMIT 1",
            )
            .bind(list.as_str())
            .bind(&sender)
            .fetch_optional(self.pool())
            .await
            .map_err(db_error)?;
            let sender_row = sender_row
                .map(|(role, action)| {
                    Ok::<_, Error>((role, action.map(|value| value.parse()).transpose()?))
                })
                .transpose()?;
            let sender_banned: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM bans WHERE (list_id=$1 OR list_id IS NULL) AND email_or_regex=$2",
            )
            .bind(list.as_str())
            .bind(&sender)
            .fetch_one(self.pool())
            .await
            .map_err(db_error)?;
            previews.push(HeldPreview {
                from: mailbox(parsed.as_ref().and_then(|message| message.from())),
                to: mailbox(parsed.as_ref().and_then(|message| message.to())),
                date: parsed
                    .as_ref()
                    .and_then(|message| message.date())
                    .map(mail_parser::DateTime::to_rfc3339)
                    .unwrap_or_default(),
                body,
                raw: String::from_utf8_lossy(&source).into_owned(),
                attachments,
                sender_row,
                sender_banned: sender_banned > 0,
                held,
            });
        }
        Ok(previews)
    }

    /// Apply one decision to several held posts of the list in one
    /// transaction; a post already decided, or not on this list, is skipped.
    /// Returns `(decided, skipped)`.
    /// # Errors
    /// Rejects revoked authority, a bad forward address, an empty
    /// selection, and database or audit failures.
    #[allow(clippy::too_many_arguments)]
    pub async fn browser_review_many(
        &self,
        session: &WebSession,
        list: &ListId,
        ids: &[HeldId],
        action: &ReviewAction,
        reason: &str,
        forward_to: Option<&str>,
        now_ms: i64,
    ) -> Result<(usize, usize)> {
        if ids.is_empty() {
            return Err(Error::Validation("no held message selected".into()));
        }
        let mut tx = self.browser_write_tx().await?;
        let user = Self::moderator_tx(&mut tx, session, list).await?;
        let context = AuditContext::new(Some(user), None, None);
        let mut done = 0;
        let mut skipped = 0;
        for id in ids {
            let on_list: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM held_messages WHERE id=$1 AND list_id=$2 AND disposition IS NULL",
            )
            .bind(id.0.to_string())
            .bind(list.as_str())
            .fetch_one(&mut *tx)
            .await
            .map_err(db_error)?;
            if on_list != 1 {
                skipped += 1;
                continue;
            }
            ModerationRepo::review_tx(
                &mut tx, self, *id, &context, action, reason, forward_to, now_ms,
            )
            .await?;
            done += 1;
        }
        Self::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)?;
        Ok((done, skipped))
    }

    /// Set the moderation override of a held post's sender on the list: the
    /// member's row when the sender is a member, otherwise a nonmember row
    /// (created when missing), as Mailman's "moderate sender" does.
    /// # Errors
    /// Rejects revoked authority, a held post of another list, an
    /// unparseable sender, and database or audit failures.
    pub async fn browser_moderate_sender(
        &self,
        session: &WebSession,
        list: &ListId,
        held: HeldId,
        action: Option<ModerationAction>,
    ) -> Result<()> {
        let mut tx = self.browser_write_tx().await?;
        let user = Self::moderator_tx(&mut tx, session, list).await?;
        let context = AuditContext::new(Some(user), None, None);
        let sender: Option<String> =
            sqlx::query_scalar("SELECT sender FROM held_messages WHERE id=$1 AND list_id=$2")
                .bind(held.0.to_string())
                .bind(list.as_str())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_error)?;
        let sender = sender.ok_or_else(|| Error::NotFound("held message".into()))?;
        let address = Address::new(sender.trim(), String::new())?;
        let existing: Option<(String, String)> = sqlx::query_as(
            "SELECT m.id, m.role FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id=$1 AND a.email=$2 ORDER BY CASE m.role WHEN 'member' THEN 0 WHEN 'nonmember' THEN 1 ELSE 2 END LIMIT 1",
        )
        .bind(list.as_str())
        .bind(&address.email)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?;
        let member_id = match existing {
            Some((id, role)) if role == "member" || role == "nonmember" => {
                sqlx::query("UPDATE members SET moderation_action=$1 WHERE id=$2")
                    .bind(action.map(|a| a.to_string()))
                    .bind(&id)
                    .execute(&mut *tx)
                    .await
                    .map_err(db_error)?;
                id
            }
            _ => {
                let address_id: Option<String> =
                    sqlx::query_scalar("SELECT id FROM addresses WHERE email=$1")
                        .bind(&address.email)
                        .fetch_optional(&mut *tx)
                        .await
                        .map_err(db_error)?;
                let address_id = if let Some(id) = address_id {
                    id
                } else {
                    sqlx::query("INSERT INTO addresses(id,email,original_email,display_name,registered_on) VALUES($1,$2,$3,'',$4)")
                        .bind(address.id.to_string()).bind(&address.email).bind(&address.original_email).bind(address.registered_on.to_rfc3339())
                        .execute(&mut *tx).await.map_err(db_error)?;
                    address.id.to_string()
                };
                let member = MemberId::new();
                let preferences = listmngr_core::PreferencesId::new();
                sqlx::query("INSERT INTO preferences(id) VALUES($1)")
                    .bind(preferences.to_string())
                    .execute(&mut *tx)
                    .await
                    .map_err(db_error)?;
                sqlx::query("INSERT INTO members(id,list_id,role,address_id,user_id,subscription_mode,moderation_action,display_name,preferences_id,created_at) VALUES($1,$2,'nonmember',$3,NULL,'as_address',$4,'',$5,$6)")
                    .bind(member.to_string()).bind(list.as_str()).bind(&address_id).bind(action.map(|a| a.to_string()))
                    .bind(preferences.to_string()).bind(chrono::Utc::now().to_rfc3339())
                    .execute(&mut *tx).await.map_err(db_error)?;
                Self::record_tx_with_context(
                    &mut tx,
                    &context,
                    "member.create",
                    "member",
                    &member.to_string(),
                    serde_json::json!({"list_id": list, "role": "nonmember", "source": "browser-moderation"}),
                )
                .await?;
                member.to_string()
            }
        };
        Self::record_tx_with_context(
            &mut tx,
            &context,
            "member.update",
            "member",
            &member_id,
            serde_json::json!({"moderation_action": action, "list_id": list, "held_id": held.0, "source": "browser-moderation"}),
        )
        .await?;
        Self::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)
    }

    /// Ban a held post's sender on the list.
    /// # Errors
    /// Rejects revoked authority, a held post of another list, a sender
    /// already banned (conflict), and database or audit failures.
    pub async fn browser_ban_sender(
        &self,
        session: &WebSession,
        list: &ListId,
        held: HeldId,
    ) -> Result<String> {
        let mut tx = self.browser_write_tx().await?;
        let user = Self::moderator_tx(&mut tx, session, list).await?;
        let sender: Option<String> =
            sqlx::query_scalar("SELECT sender FROM held_messages WHERE id=$1 AND list_id=$2")
                .bind(held.0.to_string())
                .bind(list.as_str())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_error)?;
        let sender = sender.ok_or_else(|| Error::NotFound("held message".into()))?;
        let address = Address::new(sender.trim(), String::new())?;
        let already: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM bans WHERE list_id=$1 AND email_or_regex=$2")
                .bind(list.as_str())
                .bind(&address.email)
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?;
        if already > 0 {
            return Err(Error::Conflict("sender already banned".into()));
        }
        let value = BanRepo::create_tx(
            &mut tx,
            list,
            &address.email,
            &AuditContext::new(Some(user), None, None),
        )
        .await?;
        Self::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(value)
    }

    /// The list's undecided subscription requests, oldest first.
    /// # Errors
    /// Rejects revoked authority and database failures.
    pub async fn browser_requests(
        &self,
        session: &WebSession,
        list: &ListId,
    ) -> Result<Vec<PendingRequest>> {
        self.moderator_check(session, list).await?;
        let rows = sqlx::query("SELECT id,list_id,original_email,display_name,action,state,created_at FROM subscription_workflows WHERE list_id=$1 AND state IN ('pending_confirmation','pending_moderation') ORDER BY created_at,id LIMIT 200")
            .bind(list.as_str())
            .fetch_all(self.pool())
            .await
            .map_err(db_error)?;
        rows.iter().map(pending_row).collect()
    }

    /// Decide one of the list's requests.
    /// # Errors
    /// Rejects revoked authority, a request of another list (not found),
    /// and database or audit failures.
    pub async fn browser_decide_request(
        &self,
        session: &WebSession,
        list: &ListId,
        request: &str,
        decision: RequestDecision,
        reason: &str,
    ) -> Result<()> {
        if Uuid::parse_str(request).is_err() {
            return Err(Error::NotFound("subscription request".into()));
        }
        let mut tx = self.browser_write_tx().await?;
        let user = Self::moderator_tx(&mut tx, session, list).await?;
        let on_list: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM subscription_workflows WHERE id=$1 AND list_id=$2 AND state IN ('pending_confirmation','pending_moderation')",
        )
        .bind(request)
        .bind(list.as_str())
        .fetch_one(&mut *tx)
        .await
        .map_err(db_error)?;
        if on_list != 1 {
            return Err(Error::NotFound("subscription request".into()));
        }
        WorkflowRepo::decide_tx(
            &mut tx,
            self,
            request,
            decision,
            reason,
            &AuditContext::new(Some(user), None, None),
        )
        .await?;
        Self::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)
    }
}
