//! Durable, confirm-only public join/leave. Tokens are 256-bit OS randomness.
//!
//! Workflow/audit/notice spool commit together; confirmation never trusts an
//! email supplied by the confirmer. Delivery spools contain secrets: restrict DB access.
use crate::mail_queue::{ChildJob, Lease, MessageId, Queue, ack_leased_job, insert_child_job};
use crate::{AuditContext, Database, db_error};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use listmngr_core::{Address, Error, ListId, MemberRole, Result, SubscriptionMode};
use rand::TryRngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Any, Row, Transaction};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SubscriptionAction {
    Join,
    Leave,
}
impl SubscriptionAction {
    const fn name(self) -> &'static str {
        match self {
            Self::Join => "join",
            Self::Leave => "leave",
        }
    }
}
pub use listmngr_core::EmailCommand;

#[derive(Clone, Copy, Debug)]
pub struct WorkflowRepo<'a> {
    db: &'a Database,
    clock: Option<&'a dyn crate::mail_queue::LeaseClock>,
}
impl Database {
    #[must_use]
    pub const fn workflows(&self) -> WorkflowRepo<'_> {
        WorkflowRepo {
            db: self,
            clock: None,
        }
    }
}
// Ban writers acquire this reservation before their list lock, serializing
// list-scoped ban changes with public join admission.
pub(crate) async fn lock(tx: &mut Transaction<'_, Any>) -> Result<()> {
    sqlx::query("UPDATE subscription_rate SET requests=requests WHERE id=1")
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    Ok(())
}
impl<'a> WorkflowRepo<'a> {
    /// Use the canonical post-lock clock for command lease authority.
    #[must_use]
    pub fn with_clock(mut self, clock: &'a dyn crate::mail_queue::LeaseClock) -> Self {
        self.clock = Some(clock);
        self
    }
    /// Runtime commands sample wall time after pool, row and business waits.
    #[must_use]
    pub fn live(self) -> Self {
        self.with_clock(&crate::mail_queue::SystemLeaseClock)
    }
    fn queue(&self) -> crate::mail_queue::MailQueueRepo<'a> {
        let queue = self.db.mail_queue();
        self.clock.map_or(queue, |clock| queue.with_clock(clock))
    }
    async fn command_time(
        &self,
        tx: &mut Transaction<'_, Any>,
        lease: Option<&Lease>,
        now_ms: i64,
    ) -> Result<i64> {
        if let Some(lease) = lease {
            self.queue().lock_time(tx, lease, now_ms).await
        } else {
            Ok(now_ms)
        }
    }
    // Final authority check is after all potentially blocking business writes.
    // A stale lease rolls back notice, token, membership, cooldown and audit together.
    async fn commit_command(
        &self,
        mut tx: Transaction<'_, Any>,
        lease: Option<&Lease>,
        now_ms: i64,
    ) -> Result<()> {
        if let Some(lease) = lease {
            // command_time already reserved this row before any business writes.
            // Preserve renewed authority before ACK clears the stored lease.
            let deadline =
                crate::mail_queue::MailQueueRepo::locked_deadline(&mut tx, lease).await?;
            ack_leased_job(&mut tx, lease, self.queue().time(now_ms)).await?;
            self.queue().check_final_deadline(Some(deadline), now_ms)?;
        }
        tx.commit().await.map_err(db_error)
    }
    /// Authenticate notice provenance independently of untrusted spool context.
    /// # Errors
    /// Returns database errors.
    pub async fn is_notice(&self, job: crate::mail_queue::JobId) -> Result<bool> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workflow_notices n JOIN queue_jobs q ON q.id=n.job_id WHERE n.job_id=$1 AND q.queue='out'")
            .bind(job.0.to_string()).fetch_one(self.db.pool()).await.map_err(db_error)?;
        Ok(count == 1)
    }

    /// Generic success includes unknown lists and throttled requests. At most
    /// 100 notices/minute globally and one/list/address/hour, across processes.
    /// # Errors
    /// Invalid mailbox or database failure. Never returns the secret token.
    pub async fn request(
        &self,
        list: &ListId,
        email: &str,
        action: SubscriptionAction,
        now_ms: i64,
    ) -> Result<()> {
        self.request_owned(list, email, action, None, now_ms, std::time::Instant::now())
            .await
    }

    /// Consume an inbound list command and create its notice atomically.
    /// # Errors
    /// Invalid context, stale lease, invalid mailbox or database/audit failure.
    pub async fn request_from_lease(&self, lease: &Lease, now_ms: i64) -> Result<()> {
        let started = std::time::Instant::now();
        if lease.job.queue != Queue::In {
            return Err(Error::Validation("inbound command lease required".into()));
        }
        let message = self.db.mail_queue().message(lease.job.message_id).await?;
        let context: serde_json::Value =
            serde_json::from_str(&message.context).map_err(db_error)?;
        let list: ListId = context["list_id"]
            .as_str()
            .ok_or_else(|| Error::Validation("missing command list".into()))?
            .parse()?;
        let email = context["envelope_sender"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::Validation("command requires reply mailbox".into()))?;
        let command: EmailCommand = serde_json::from_value(context["subscription_command"].clone())
            .map_err(|_| Error::Validation("invalid subscription command".into()))?;
        match command {
            EmailCommand::Help => self.help_owned(&list, email, lease, now_ms, started).await,
            EmailCommand::Confirm(token) => {
                self.confirm_owned(&list, &token, Some(lease), now_ms).await
            }
            EmailCommand::Join | EmailCommand::Leave => {
                let action = if matches!(command, EmailCommand::Join) {
                    SubscriptionAction::Join
                } else {
                    SubscriptionAction::Leave
                };
                self.request_owned(&list, email, action, Some(lease), now_ms, started)
                    .await
            }
        }
    }

    async fn request_owned(
        &self,
        list: &ListId,
        email: &str,
        action: SubscriptionAction,
        lease: Option<&Lease>,
        now_ms: i64,
        started: std::time::Instant,
    ) -> Result<()> {
        let address = Address::new(email, String::new())?;
        // Support ASCII dot-atom mailboxes only: no SMTPUTF8 or quoted locals.
        let local = email.split_once('@').map_or("", |(local, _)| local);
        if !email.is_ascii()
            || !local
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".!#$%&'*+-/=?^_`{|}~".contains(&b))
        {
            return Err(Error::Validation("unsupported notice mailbox".into()));
        }
        let mut tx = self.db.pool().begin().await.map_err(db_error)?;
        lock(&mut tx).await?;
        let now_ms = if lease.is_some() {
            now_ms.saturating_add(i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX))
        } else {
            now_ms
        };
        let now_ms = self.command_time(&mut tx, lease, now_ms).await?;
        let expires = now_ms
            .checked_add(86_400_000)
            .ok_or_else(|| Error::Validation("invalid time".into()))?;
        let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mailing_lists WHERE list_id=$1")
            .bind(list.as_str())
            .fetch_one(&mut *tx)
            .await
            .map_err(db_error)?;
        cleanup_expired(&mut tx, now_ms).await?;
        if exists == 0
            || (matches!(action, SubscriptionAction::Join)
                && crate::bans::is_banned(&mut *tx, list, &address.original_email).await?)
        {
            return self.commit_command(tx, lease, now_ms).await;
        }
        let recent:i64=sqlx::query_scalar("SELECT COUNT(*) FROM subscription_workflows WHERE list_id=$1 AND email=$2 AND created_at>$3").bind(list.as_str()).bind(&address.email).bind(now_ms.saturating_sub(3_600_000)).fetch_one(&mut *tx).await.map_err(db_error)?;
        if recent > 0 {
            return self.commit_command(tx, lease, now_ms).await;
        }
        let updated=sqlx::query("UPDATE subscription_rate SET requests=CASE WHEN window_start<=$1 THEN 1 ELSE requests+1 END,window_start=CASE WHEN window_start<=$1 THEN $2 ELSE window_start END WHERE id=1 AND (window_start<=$1 OR requests<100)")
            .bind(now_ms.saturating_sub(60_000)).bind(now_ms).execute(&mut *tx).await.map_err(db_error)?;
        if updated.rows_affected() == 0 {
            return self.commit_command(tx, lease, now_ms).await;
        }
        let mut secret = [0_u8; 32];
        rand::rngs::OsRng
            .try_fill_bytes(&mut secret)
            .map_err(db_error)?;
        let token = URL_SAFE_NO_PAD.encode(secret);
        let hash = format!("{:x}", Sha256::digest(secret));
        let id = Uuid::now_v7().to_string();
        sqlx::query("INSERT INTO subscription_workflows(id,list_id,email,action,token_hash,created_at,expires_at,original_email) VALUES($1,$2,$3,$4,$5,$6,$7,$8)")
            .bind(&id).bind(list.as_str()).bind(&address.email).bind(action.name()).bind(hash).bind(now_ms).bind(expires).bind(&address.original_email).execute(&mut *tx).await.map_err(db_error)?;
        // The Subject must stay exactly `confirm TOKEN`: replying with it
        // intact is the email confirmation path.
        let template = match action {
            SubscriptionAction::Join => "list:user:action:subscribe",
            SubscriptionAction::Leave => "list:user:action:unsubscribe",
        };
        let confirm_address = list.address_with_suffix("confirm");
        let confirm_uri = format!("/api/v1/public/lists/{list}/confirm");
        let user_email = address.original_email.clone();
        enqueue_templated_notice(
            &mut tx,
            self.db,
            list,
            Notice {
                to: &address.original_email,
                reply_to: Some(&confirm_address),
                // Stays `confirm TOKEN` in every catalog: reply-to-confirm parses it.
                subject: "confirm-subject",
                subject_args: &[("token", &token)],
                template,
            },
            |values| {
                values
                    .set("user_email", user_email)
                    .set("token", token.clone())
                    .set("confirm_uri", confirm_uri)
            },
            now_ms,
        )
        .await?;
        Database::record_tx_with_context(
            &mut tx,
            &AuditContext::system(),
            "subscription.request",
            "list",
            list.as_str(),
            serde_json::json!({"workflow_id":id,"action":action.name()}),
        )
        .await?;
        self.commit_command(tx, lease, now_ms).await
    }

    async fn help_owned(
        &self,
        list: &ListId,
        email: &str,
        lease: &Lease,
        now_ms: i64,
        started: std::time::Instant,
    ) -> Result<()> {
        let address = Address::new(email, String::new())?;
        if !email.is_ascii() || email.bytes().any(|b| b.is_ascii_control()) {
            return Err(Error::Validation("unsupported notice mailbox".into()));
        }
        let mut tx = self.db.pool().begin().await.map_err(db_error)?;
        lock(&mut tx).await?;
        let now_ms =
            now_ms.saturating_add(i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX));
        let now_ms = self.command_time(&mut tx, Some(lease), now_ms).await?;
        sqlx::query("DELETE FROM email_help_requests WHERE (list_id,email) IN (SELECT list_id,email FROM email_help_requests WHERE requested_at<=$1 ORDER BY requested_at LIMIT 100)").bind(now_ms.saturating_sub(3_600_000)).execute(&mut *tx).await.map_err(db_error)?;
        let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mailing_lists WHERE list_id=$1")
            .bind(list.as_str())
            .fetch_one(&mut *tx)
            .await
            .map_err(db_error)?;
        let recent: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM email_help_requests WHERE list_id=$1 AND email=$2 AND requested_at>$3").bind(list.as_str()).bind(&address.email).bind(now_ms.saturating_sub(3_600_000)).fetch_one(&mut *tx).await.map_err(db_error)?;
        if exists == 0 || recent > 0 {
            return self.commit_command(tx, Some(lease), now_ms).await;
        }
        let updated=sqlx::query("UPDATE subscription_rate SET requests=CASE WHEN window_start<=$1 THEN 1 ELSE requests+1 END,window_start=CASE WHEN window_start<=$1 THEN $2 ELSE window_start END WHERE id=1 AND (window_start<=$1 OR requests<100)")
            .bind(now_ms.saturating_sub(60_000)).bind(now_ms).execute(&mut *tx).await.map_err(db_error)?;
        if updated.rows_affected() == 0 {
            return self.commit_command(tx, Some(lease), now_ms).await;
        }
        sqlx::query("INSERT INTO email_help_requests(list_id,email,requested_at) VALUES($1,$2,$3) ON CONFLICT(list_id,email) DO UPDATE SET requested_at=excluded.requested_at").bind(list.as_str()).bind(&address.email).bind(now_ms).execute(&mut *tx).await.map_err(db_error)?;
        let request_address = list.request_address();
        enqueue_templated_notice(
            &mut tx,
            self.db,
            list,
            Notice {
                to: &address.original_email,
                reply_to: Some(&request_address),
                subject: "notice-help-subject",
                subject_args: &[],
                template: "list:user:notice:help",
            },
            |values| values,
            now_ms,
        )
        .await?;
        Database::record_tx_with_context(
            &mut tx,
            &AuditContext::system(),
            "subscription.help",
            "list",
            list.as_str(),
            serde_json::json!({}),
        )
        .await?;
        self.commit_command(tx, Some(lease), now_ms).await
    }

    /// Consume a list-scoped, unexpired token and mutate only its stored mailbox.
    /// # Errors
    /// Invalid/replayed/expired/wrong-list token, or an atomic database failure.
    pub async fn confirm(&self, list: &ListId, token: &str, now_ms: i64) -> Result<()> {
        self.confirm_owned(list, token, None, now_ms).await
    }

    async fn confirm_owned(
        &self,
        list: &ListId,
        token: &str,
        lease: Option<&Lease>,
        now_ms: i64,
    ) -> Result<()> {
        let started = std::time::Instant::now();
        let secret = URL_SAFE_NO_PAD.decode(token).map_err(|_| invalid_token())?;
        if secret.len() != 32 || token.len() != 43 {
            return Err(invalid_token());
        }
        let hash = format!("{:x}", Sha256::digest(secret));
        let mut tx = self.db.pool().begin().await.map_err(db_error)?;
        lock(&mut tx).await?;
        // The supplied Unix clock is sampled at call time, not after pool or
        // writer-lock waits. Advance it monotonically before consuming a token.
        let elapsed = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
        let now_ms = now_ms.saturating_add(elapsed);
        let now_ms = self.command_time(&mut tx, lease, now_ms).await?;
        let row=sqlx::query("UPDATE subscription_workflows SET consumed=1 WHERE list_id=$1 AND token_hash=$2 AND consumed=0 AND expires_at>$3 RETURNING id,email,original_email,action")
            .bind(list.as_str()).bind(hash).bind(now_ms).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or_else(invalid_token)?;
        let email: String = row.try_get("email").map_err(db_error)?;
        let action: String = row.try_get("action").map_err(db_error)?;
        let original_email: String = row.try_get("original_email").map_err(db_error)?;
        if action == "join" && crate::bans::is_banned(&mut *tx, list, &original_email).await? {
            return Err(invalid_token());
        }
        let existing=sqlx::query("SELECT m.id,m.preferences_id FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id=$1 AND a.email=$2 AND m.role='member'")
            .bind(list.as_str()).bind(&email).fetch_optional(&mut *tx).await.map_err(db_error)?;
        if action == "join" && existing.is_none() {
            let address = Address::new(&original_email, String::new())?;
            if address.email != email {
                return Err(Error::Validation(
                    "workflow mailbox identity mismatch".into(),
                ));
            }
            crate::insert_mass_members(
                &mut tx,
                self.db,
                list,
                MemberRole::Member,
                SubscriptionMode::AsAddress,
                &[&address],
            )
            .await?;
        } else if action == "leave"
            && let Some(member) = existing
        {
            let id: String = member.try_get("id").map_err(db_error)?;
            let pref: String = member.try_get("preferences_id").map_err(db_error)?;
            crate::delete_mass_members(&mut tx, self.db, &[(id, pref)]).await?;
        }
        let id: String = row.try_get("id").map_err(db_error)?;
        enqueue_confirmation_receipt(&mut tx, self.db, list, &original_email, &action, now_ms)
            .await?;
        Database::record_tx_with_context(
            &mut tx,
            &AuditContext::system(),
            "subscription.confirm",
            "list",
            list.as_str(),
            serde_json::json!({"workflow_id":id,"action":action}),
        )
        .await?;
        self.commit_command(tx, lease, now_ms).await
    }
}
/// Called only after an actual membership INSERT, inside its audited transaction.
/// Recipient and list identity come from the stored membership, never a caller.
pub(crate) async fn welcome_new_member(
    tx: &mut Transaction<'_, Any>,
    db: &Database,
    member: listmngr_core::MemberId,
) -> Result<()> {
    let stored = sqlx::query("SELECT m.list_id,a.original_email,l.send_welcome_message,l.display_name FROM members m JOIN addresses a ON a.id=m.address_id JOIN mailing_lists l ON l.list_id=m.list_id WHERE m.id=$1 AND m.role='member'")
        .bind(member.to_string()).fetch_optional(&mut **tx).await.map_err(db_error)?;
    let Some(stored) = stored else {
        return Ok(());
    };
    let list: ListId = stored
        .try_get::<String, _>("list_id")
        .map_err(db_error)?
        .parse()?;
    let email: String = stored.try_get("original_email").map_err(db_error)?;
    if crate::bans::is_banned(&mut **tx, &list, &email).await? {
        // Suppress this optional notice without redefining administrative
        // admission. Public join workflows enforce bans before insertion.
        return Ok(());
    }
    if stored
        .try_get::<i64, _>("send_welcome_message")
        .map_err(db_error)?
        == 0
    {
        return Ok(());
    }
    let display_name: String = stored.try_get("display_name").map_err(db_error)?;
    let owner = list.owner_address();
    enqueue_templated_notice(
        tx,
        db,
        &list,
        Notice {
            to: &email,
            reply_to: Some(&owner),
            subject: "notice-welcome-subject",
            subject_args: &[("display_name", &display_name)],
            template: "list:user:notice:welcome",
        },
        |values| values.set("user_email", email.clone()),
        chrono::Utc::now().timestamp_millis(),
    )
    .await
}

/// Delete first with RETURNING so only the actual deletion winner can notify.
/// All recipient data is stored authority; the caller owns audit and commit.
pub(crate) async fn delete_member_with_goodbye(
    tx: &mut Transaction<'_, Any>,
    db: &Database,
    member: &str,
) -> Result<bool> {
    let removed = sqlx::query("DELETE FROM members WHERE id=$1 RETURNING list_id,address_id,role")
        .bind(member)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_error)?;
    let Some(removed) = removed else {
        return Ok(false);
    };
    if removed.try_get::<String, _>("role").map_err(db_error)? != "member" {
        return Ok(true);
    }
    let list: ListId = removed
        .try_get::<String, _>("list_id")
        .map_err(db_error)?
        .parse()?;
    let address: String = removed.try_get("address_id").map_err(db_error)?;
    let email: Option<String> = sqlx::query_scalar("SELECT a.original_email FROM addresses a JOIN mailing_lists l ON l.list_id=$1 WHERE a.id=$2 AND l.send_goodbye_message=1")
        .bind(list.as_str()).bind(address).fetch_optional(&mut **tx).await.map_err(db_error)?;
    if let Some(email) = email {
        enqueue_goodbye(tx, db, &list, &email).await?;
    }
    Ok(true)
}

async fn enqueue_goodbye(
    tx: &mut Transaction<'_, Any>,
    db: &Database,
    list: &ListId,
    email: &str,
) -> Result<()> {
    let snapshot = crate::notices::list_snapshot(tx, list).await?;
    let owner = list.owner_address();
    enqueue_templated_notice(
        tx,
        db,
        list,
        Notice {
            to: email,
            reply_to: Some(&owner),
            subject: "notice-goodbye-subject",
            subject_args: &[("display_name", &snapshot.display_name)],
            template: "list:user:notice:goodbye",
        },
        |values| values.set("user_email", email.to_owned()),
        chrono::Utc::now().timestamp_millis(),
    )
    .await
}

async fn enqueue_confirmation_receipt(
    tx: &mut Transaction<'_, Any>,
    db: &Database,
    list: &ListId,
    email: &str,
    action: &str,
    now_ms: i64,
) -> Result<()> {
    let outcome = match action {
        "join" => "receipt-join-outcome",
        "leave" => "receipt-leave-outcome",
        _ => return Err(Error::Validation("invalid subscription action".into())),
    };
    let owner = list.owner_address();
    let snapshot = crate::notices::list_snapshot(tx, list).await?;
    let language =
        crate::notices::recipient_language(tx, &snapshot, email, db.default_language()).await?;
    let outcome = listmngr_i18n::message(&language, outcome, &[]);
    enqueue_templated_notice(
        tx,
        db,
        list,
        Notice {
            to: email,
            reply_to: Some(&owner),
            subject: "notice-receipt-subject",
            subject_args: &[("action", action)],
            template: "list:user:notice:receipt",
        },
        |values| {
            values
                .set("outcome", outcome)
                .set("user_email", email.to_owned())
        },
        now_ms,
    )
    .await
}

/// Narrow internal producer: callers must admit the stored original envelope
/// and raw message before invoking. Never accepts arbitrary MIME or context.
/// Shares job-bound DB provenance with subscription notices; untrusted context
/// alone cannot authorize outbound's notice bypass.
pub(crate) async fn enqueue_rejection_notice(
    tx: &mut Transaction<'_, Any>,
    db: &Database,
    list: &ListId,
    sender: &str,
    reason: &str,
    now_ms: i64,
) -> Result<()> {
    // Render at most 4096 UTF-8 bytes of comment (round down to a character
    // boundary), plus a fixed truncation marker. The durable reason is untouched.
    let mut end = reason.len().min(4096);
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    let marker = if end < reason.len() {
        "\n[Comment truncated]"
    } else {
        ""
    };
    let reasons = format!("{}{marker}", &reason[..end]);
    let snapshot = crate::notices::list_snapshot(tx, list).await?;
    enqueue_templated_notice(
        tx,
        db,
        list,
        Notice {
            to: sender,
            reply_to: None,
            subject: "notice-rejected-subject",
            subject_args: &[("display_name", &snapshot.display_name)],
            template: "list:user:notice:rejected",
        },
        |values| {
            values
                .set("reasons", reasons)
                .set("sender_email", sender.to_owned())
        },
        now_ms,
    )
    .await
}

// Only the winning automatic disable calls this transaction-bound producer.
// Recipient authority is sampled by one query; later roster edits do not revoke
// already materialized jobs. EXISTS deduplicates dual owner/moderator roles.
pub(crate) async fn enqueue_disable_notice(
    tx: &mut Transaction<'_, Any>,
    db: &Database,
    list: &ListId,
    member: &str,
    now_ms: i64,
) -> Result<()> {
    enqueue_bounce_notice(tx, db, list, member, now_ms, BounceNotice::Disable).await
}

// Fresh eligible observations only; score is sampled before threshold reset.
pub(crate) async fn enqueue_increment_notice(
    tx: &mut Transaction<'_, Any>,
    db: &Database,
    list: &ListId,
    member: &str,
    now_ms: i64,
    score: f64,
) -> Result<()> {
    enqueue_bounce_notice(tx, db, list, member, now_ms, BounceNotice::Increment(score)).await
}

pub(crate) async fn enqueue_removal_notice(
    tx: &mut Transaction<'_, Any>,
    db: &Database,
    list: &ListId,
    member: &str,
    now_ms: i64,
) -> Result<()> {
    enqueue_bounce_notice(tx, db, list, member, now_ms, BounceNotice::Removal).await
}

enum BounceNotice {
    Disable,
    Increment(f64),
    Removal,
}

async fn enqueue_bounce_notice(
    tx: &mut Transaction<'_, Any>,
    db: &Database,
    list: &ListId,
    member: &str,
    now_ms: i64,
    notice: BounceNotice,
) -> Result<()> {
    let (query, subject, template, action, score) = match notice {
        BounceNotice::Disable => (
            "SELECT bounce_notify_owner_on_disable FROM mailing_lists WHERE list_id=$1",
            "notice-bounce-disable-subject",
            "list:admin:notice:disable",
            "bounce.disable_notice",
            None,
        ),
        BounceNotice::Increment(score) => (
            "SELECT bounce_notify_owner_on_bounce_increment FROM mailing_lists WHERE list_id=$1",
            "notice-bounce-increment-subject",
            "list:admin:notice:increment",
            "bounce.increment_notice",
            Some(score),
        ),
        BounceNotice::Removal => (
            "SELECT bounce_notify_owner_on_removal FROM mailing_lists WHERE list_id=$1",
            "notice-bounce-removal-subject",
            "list:admin:notice:removal",
            "bounce.removal_notice",
            None,
        ),
    };
    let posting_address = list.posting_address();
    let enabled: i64 = sqlx::query_scalar(query)
        .bind(list.as_str())
        .fetch_one(&mut **tx)
        .await
        .map_err(db_error)?;
    if enabled == 0 {
        return Ok(());
    }
    let recipients: Vec<String> = sqlx::query_scalar("SELECT a.original_email FROM addresses a WHERE EXISTS (SELECT 1 FROM members m WHERE m.address_id=a.id AND m.list_id=$1 AND m.role IN ('owner','moderator')) ORDER BY a.email")
        .bind(list.as_str()).fetch_all(&mut **tx).await.map_err(db_error)?;
    for email in &recipients {
        if listmngr_mail::owner::points_to_list(email, list)
            || !listmngr_mail::owner::safe_mailbox(member)
        {
            return Err(Error::Validation(
                "unsupported disable notice mailbox".into(),
            ));
        }
        enqueue_templated_notice(
            tx,
            db,
            list,
            Notice {
                to: email,
                reply_to: None,
                subject,
                subject_args: &[("member", member), ("listname", &posting_address)],
                template,
            },
            |values| {
                let values = values.set("member", member.to_owned());
                match score {
                    Some(score) => values.set("score", score.to_string()),
                    None => values,
                }
            },
            now_ms,
        )
        .await?;
    }
    Database::record_tx_with_context(
        tx,
        &AuditContext::system(),
        action,
        "list",
        list.as_str(),
        serde_json::json!({"recipient_count":recipients.len()}),
    )
    .await
}

pub(crate) async fn enqueue_warning_notice(
    tx: &mut Transaction<'_, Any>,
    db: &Database,
    list: &ListId,
    email: &str,
    now_ms: i64,
) -> Result<()> {
    if listmngr_mail::owner::points_to_list(email, list) {
        return Err(Error::Validation("unsupported warning mailbox".into()));
    }
    let owner = list.owner_address();
    let posting_address = list.posting_address();
    enqueue_templated_notice(
        tx,
        db,
        list,
        Notice {
            to: email,
            reply_to: Some(&owner),
            subject: "notice-warning-subject",
            subject_args: &[("listname", &posting_address)],
            template: "list:user:notice:warning",
        },
        |values| values.set("user_email", email.to_owned()),
        now_ms,
    )
    .await
}

/// Notify the poster (`list:user:notice:hold`) and the owners/moderators
/// (`list:admin:action:post`) of a held post, each gated by its list setting.
/// The poster notice goes only to a safe, non-list envelope sender, so a null
/// or list-owned reverse path never produces backscatter.
pub(crate) async fn enqueue_hold_notices(
    tx: &mut Transaction<'_, Any>,
    db: &Database,
    list: &ListId,
    sender: &str,
    subject: &str,
    reasons: &str,
    now_ms: i64,
) -> Result<()> {
    let snapshot = crate::notices::list_snapshot(tx, list).await?;
    let posting_address = list.posting_address();
    // An empty subject is shown as a per-language placeholder below.
    let subject_line = (!subject.trim().is_empty()).then(|| subject.to_owned());
    let sender_is_safe = listmngr_mail::owner::safe_mailbox(sender)
        && sender.len() <= 254
        && !listmngr_mail::owner::points_to_list(sender, list);
    if snapshot.respond_to_post_requests && sender_is_safe {
        let language =
            crate::notices::recipient_language(tx, &snapshot, sender, db.default_language())
                .await?;
        let shown_subject = subject_line
            .clone()
            .unwrap_or_else(|| listmngr_i18n::message(&language, "notice-no-subject", &[]));
        enqueue_templated_notice(
            tx,
            db,
            list,
            Notice {
                to: sender,
                reply_to: None,
                subject: "notice-hold-subject",
                subject_args: &[("listname", &posting_address)],
                template: "list:user:notice:hold",
            },
            |values| {
                values
                    .set("subject", shown_subject)
                    .set("reasons", reasons)
                    .set("sender_email", sender)
            },
            now_ms,
        )
        .await?;
    }
    if !snapshot.admin_immed_notify {
        return Ok(());
    }
    let recipients: Vec<String> = sqlx::query_scalar("SELECT a.original_email FROM addresses a WHERE EXISTS (SELECT 1 FROM members m WHERE m.address_id=a.id AND m.list_id=$1 AND m.role IN ('owner','moderator')) ORDER BY a.email")
        .bind(list.as_str()).fetch_all(&mut **tx).await.map_err(db_error)?;
    for email in &recipients {
        if listmngr_mail::owner::points_to_list(email, list) {
            continue;
        }
        let language =
            crate::notices::recipient_language(tx, &snapshot, email, db.default_language()).await?;
        let shown_sender = if sender_is_safe {
            sender.to_owned()
        } else {
            listmngr_i18n::message(&language, "notice-unknown-sender", &[])
        };
        let shown_subject = subject_line
            .clone()
            .unwrap_or_else(|| listmngr_i18n::message(&language, "notice-no-subject", &[]));
        enqueue_templated_notice(
            tx,
            db,
            list,
            Notice {
                to: email,
                reply_to: None,
                subject: "notice-admin-post-subject",
                subject_args: &[("listname", &posting_address), ("sender", &shown_sender)],
                template: "list:admin:action:post",
            },
            |values| {
                values
                    .set("subject", shown_subject)
                    .set("reasons", reasons)
                    .set("sender_email", shown_sender.clone())
            },
            now_ms,
        )
        .await?;
    }
    Ok(())
}

/// One templated notice to enqueue: who receives it, which template renders
/// the body, and which catalog message (with arguments) is its subject.
struct Notice<'a> {
    to: &'a str,
    reply_to: Option<&'a str>,
    subject: &'a str,
    subject_args: &'a [(&'a str, &'a str)],
    template: &'a str,
}

/// Render the notice in the recipient's language and enqueue it job-bound.
/// `placeholders` extends the list's own.
async fn enqueue_templated_notice(
    tx: &mut Transaction<'_, Any>,
    db: &Database,
    list: &ListId,
    notice: Notice<'_>,
    placeholders: impl FnOnce(
        listmngr_mail::templates::Placeholders,
    ) -> listmngr_mail::templates::Placeholders,
    now_ms: i64,
) -> Result<()> {
    let Notice {
        to,
        reply_to,
        subject,
        subject_args,
        template,
    } = notice;
    let snapshot = crate::notices::list_snapshot(tx, list).await?;
    let language =
        crate::notices::recipient_language(tx, &snapshot, to, db.default_language()).await?;
    let subject = listmngr_i18n::message(&language, subject, subject_args);
    let values = placeholders(crate::notices::list_placeholders(&snapshot));
    let body = crate::notices::render(tx, &snapshot, template, &language, &values).await?;
    let id = Uuid::now_v7().to_string();
    let date = chrono::DateTime::from_timestamp_millis(now_ms)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc2822();
    let host = list.mail_host().to_owned();
    let from = list.owner_address();
    let raw = crate::notices::serialize(
        &crate::notices::Envelope {
            from: &from,
            to,
            reply_to,
            subject: &subject,
            message_id_local: &id,
            mail_host: &host,
            date: &date,
        },
        &body,
    )?;
    enqueue_notice(tx, list, to, &id, raw, now_ms).await
}

// Private raw producer for generated subscription/moderation MIME only.
// The historical workflow_notices table is deliberately just job_id provenance.
async fn enqueue_notice(
    tx: &mut Transaction<'_, Any>,
    list: &ListId,
    email: &str,
    id: &str,
    raw: Vec<u8>,
    now_ms: i64,
) -> Result<()> {
    let key = format!("{:x}", Sha256::digest(&raw));
    let message_id = MessageId(Uuid::now_v7());
    sqlx::query("INSERT INTO message_blobs(store_key,raw) VALUES($1,$2)")
        .bind(&key)
        .bind(raw)
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    // Context is routing metadata only; the job-bound row below is authority.
    let context = serde_json::json!({"list_id":list.as_str()}).to_string();
    sqlx::query(
        "INSERT INTO messages(id,store_key,external_id,context,created_at) VALUES($1,$2,$3,$4,$5)",
    )
    .bind(message_id.0.to_string())
    .bind(key)
    .bind(id)
    .bind(context)
    .bind(now_ms)
    .execute(&mut **tx)
    .await
    .map_err(db_error)?;
    let job = insert_child_job(
        tx,
        message_id,
        &ChildJob {
            queue: Queue::Out,
            max_attempts: 5,
            recipients: vec![email.to_owned()],
        },
        now_ms,
    )
    .await?;
    sqlx::query("INSERT INTO workflow_notices(job_id) VALUES($1)")
        .bind(job.id.0.to_string())
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    Ok(())
}

fn invalid_token() -> Error {
    Error::Validation("invalid or expired confirmation".into())
}

// Indexed, capped, and committed with the request. Used tokens remain until
// expiry so confirming cannot bypass the address cooldown.
async fn cleanup_expired(tx: &mut Transaction<'_, Any>, now_ms: i64) -> Result<()> {
    let deleted=sqlx::query("DELETE FROM subscription_workflows WHERE id IN (SELECT id FROM subscription_workflows WHERE expires_at<=$1 ORDER BY expires_at,id LIMIT 100)")
        .bind(now_ms).execute(&mut **tx).await.map_err(db_error)?.rows_affected();
    if deleted > 0 {
        Database::record_tx_with_context(
            tx,
            &AuditContext::system(),
            "subscription.expire",
            "workflow",
            "expired",
            serde_json::json!({"deleted":deleted}),
        )
        .await?;
    }
    Ok(())
}
