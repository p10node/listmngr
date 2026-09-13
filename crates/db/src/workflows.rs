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
    fn parse(name: &str) -> Result<Self> {
        match name {
            "join" => Ok(Self::Join),
            "leave" => Ok(Self::Leave),
            _ => Err(Error::Validation("invalid subscription action".into())),
        }
    }
}
pub use listmngr_core::EmailCommand;

/// Mailman's `token_owner`: whose move it is on a waiting request.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TokenOwner {
    /// The requester still has to confirm the address (a token is out).
    Subscriber,
    /// A moderator has to decide.
    Moderator,
}

impl TokenOwner {
    const fn state(self) -> &'static str {
        match self {
            Self::Subscriber => "pending_confirmation",
            Self::Moderator => "pending_moderation",
        }
    }
    fn from_state(state: &str) -> Result<Self> {
        match state {
            "pending_confirmation" => Ok(Self::Subscriber),
            "pending_moderation" => Ok(Self::Moderator),
            _ => Err(Error::Validation("request is not pending".into())),
        }
    }
}

/// A subscription request that has not been decided.
#[derive(Clone, Debug, Serialize)]
pub struct PendingRequest {
    pub id: String,
    pub list_id: ListId,
    /// The mailbox as the requester wrote it.
    pub email: String,
    /// The display name an operator supplied; empty for a public request.
    pub display_name: String,
    pub action: SubscriptionAction,
    pub token_owner: TokenOwner,
    pub requested_at: i64,
}

/// An operator subscribing somebody through the API, with Mailman's three
/// "already done" flags and its invitation switch.
///
/// The flags stay separate booleans because they are Mailman's own REST
/// field names, each supplying one independent step of the workflow.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug)]
pub struct AdminSubscription<'a> {
    pub list: &'a ListId,
    pub email: &'a str,
    pub display_name: &'a str,
    /// The address is known to belong to the subscriber.
    pub pre_verified: bool,
    /// The subscriber has already asked to join.
    pub pre_confirmed: bool,
    /// A moderator has already approved.
    pub pre_approved: bool,
    /// Invite instead of subscribing: the address must accept, and the
    /// invitation itself is the owner's approval.
    pub invitation: bool,
}

/// What an administrative subscription did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubscriptionOutcome {
    /// The member exists now.
    Subscribed,
    /// A request is waiting; `token` addresses it (`/requests/{token}`).
    Held {
        token: String,
        token_owner: TokenOwner,
    },
}

/// Which pending requests to list; `None` means every value.
#[derive(Clone, Copy, Debug, Default)]
pub struct RequestFilter {
    pub token_owner: Option<TokenOwner>,
    pub action: Option<SubscriptionAction>,
}

/// Mailman's moderator verbs for a held subscription request.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RequestDecision {
    /// Apply the membership change the requester asked for.
    Accept,
    /// Refuse it: the request is closed without the change.
    Reject,
    /// Refuse it silently, leaving no request behind.
    Discard,
    /// Leave it waiting for a later decision.
    Defer,
}

impl RequestDecision {
    const fn audit(self) -> &'static str {
        match self {
            Self::Accept => "subscription.accept",
            Self::Reject => "subscription.reject",
            Self::Discard => "subscription.discard",
            Self::Defer => "subscription.defer",
        }
    }
}

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
    /// The envelope sender a notice was given (a probe's one-time bounce
    /// address); `None` for every other notice, which goes out with a null
    /// reverse path.
    /// # Errors
    /// Returns database errors.
    pub async fn notice_sender(&self, job: crate::mail_queue::JobId) -> Result<Option<String>> {
        let sender: Option<Option<String>> = sqlx::query_scalar("SELECT n.mail_from FROM workflow_notices n JOIN queue_jobs q ON q.id=n.job_id WHERE n.job_id=$1 AND q.queue='out'")
            .bind(job.0.to_string())
            .fetch_optional(self.db.pool())
            .await
            .map_err(db_error)?;
        Ok(sender.flatten())
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
            EmailCommand::Help => {
                self.bot_reply(&list, email, &BotReply::Help, lease, now_ms, started)
                    .await
            }
            EmailCommand::Echo(text) => {
                self.bot_reply(&list, email, &BotReply::Echo(&text), lease, now_ms, started)
                    .await
            }
            // Mailman's halt: the message carried no command to run.
            EmailCommand::End => {
                let mut tx = self.db.pool().begin().await.map_err(db_error)?;
                lock(&mut tx).await?;
                let now_ms = now_ms.saturating_add(
                    i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX),
                );
                let now_ms = self.command_time(&mut tx, Some(lease), now_ms).await?;
                self.commit_command(tx, Some(lease), now_ms).await
            }
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
        notice_mailbox(email)?;
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
        let policy_column = match action {
            SubscriptionAction::Join => "subscription_policy",
            SubscriptionAction::Leave => "unsubscription_policy",
        };
        let policy: Option<String> = sqlx::query_scalar(&format!(
            "SELECT {policy_column} FROM mailing_lists WHERE list_id=$1"
        ))
        .bind(list.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?;
        cleanup_expired(&mut tx, now_ms).await?;
        let Some(policy) = policy else {
            return self.commit_command(tx, lease, now_ms).await;
        };
        if matches!(action, SubscriptionAction::Join)
            && crate::bans::is_banned(&mut *tx, list, &address.original_email).await?
        {
            return self.commit_command(tx, lease, now_ms).await;
        }
        // The per-address hourly cooldown bounds what an unauthenticated
        // request can produce: a confirmation mail, or a row in the
        // moderator's queue. An `open` list produces neither — it acts on
        // the roster — and a member who joins must be able to leave again
        // within the hour, so the cooldown does not apply there.
        if policy != "open" {
            let recent:i64=sqlx::query_scalar("SELECT COUNT(*) FROM subscription_workflows WHERE list_id=$1 AND email=$2 AND created_at>$3").bind(list.as_str()).bind(&address.email).bind(now_ms.saturating_sub(3_600_000)).fetch_one(&mut *tx).await.map_err(db_error)?;
            if recent > 0 {
                return self.commit_command(tx, lease, now_ms).await;
            }
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
        // Mailman's policy vocabulary decides what the request becomes. Only
        // the two confirming policies ever put the token in the mail; the
        // other rows keep its hash so the unique column stays populated by a
        // secret nobody was given.
        let state = match policy.as_str() {
            "open" => "closed",
            "moderate" => "pending_moderation",
            _ => "pending_confirmation",
        };
        sqlx::query("INSERT INTO subscription_workflows(id,list_id,email,action,token_hash,created_at,expires_at,original_email,state,display_name,pre_approved) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,'',0)")
            .bind(&id).bind(list.as_str()).bind(&address.email).bind(action.name()).bind(hash).bind(now_ms).bind(expires).bind(&address.original_email).bind(state).execute(&mut *tx).await.map_err(db_error)?;
        if state != "pending_confirmation" {
            record_unconfirmed(
                &mut tx,
                self.db,
                &Unconfirmed {
                    list,
                    address: &address,
                    action,
                    id: &id,
                    policy: &policy,
                    immediate: state == "closed",
                },
            )
            .await?;
            return self.commit_command(tx, lease, now_ms).await;
        }
        enqueue_confirmation(
            &mut tx,
            self.db,
            list,
            &address,
            confirmation_template(action, false),
            &token,
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

    /// One bounded reply from the command bot, under the same per-address
    /// hourly budget whichever verb asked for it.
    async fn bot_reply(
        &self,
        list: &ListId,
        email: &str,
        reply: &BotReply<'_>,
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
        let echoed = reply.echoed().to_owned();
        enqueue_templated_notice(
            &mut tx,
            self.db,
            list,
            Notice {
                to: &address.original_email,
                reply_to: Some(&request_address),
                subject: reply.subject(),
                subject_args: &[],
                template: reply.template(),
            },
            |values| values.set("echo", echoed),
            now_ms,
        )
        .await?;
        Database::record_tx_with_context(
            &mut tx,
            &AuditContext::system(),
            reply.audit(),
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
        let row=sqlx::query("UPDATE subscription_workflows SET consumed=1 WHERE list_id=$1 AND token_hash=$2 AND consumed=0 AND expires_at>$3 RETURNING id,email,original_email,display_name,action,pre_approved")
            .bind(list.as_str()).bind(hash).bind(now_ms).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or_else(invalid_token)?;
        let email: String = row.try_get("email").map_err(db_error)?;
        let action: String = row.try_get("action").map_err(db_error)?;
        let original_email: String = row.try_get("original_email").map_err(db_error)?;
        if action == "join" && crate::bans::is_banned(&mut *tx, list, &original_email).await? {
            return Err(invalid_token());
        }

        let id: String = row.try_get("id").map_err(db_error)?;
        let display_name: String = row.try_get("display_name").map_err(db_error)?;
        let pre_approved: i64 = row.try_get("pre_approved").map_err(db_error)?;
        let action = SubscriptionAction::parse(&action)?;
        let address = Address::new(&original_email, display_name)?;
        if address.email != email {
            return Err(Error::Validation(
                "workflow mailbox identity mismatch".into(),
            ));
        }
        // `confirm_then_moderate` proves the address here and hands the
        // decision to a moderator; the token is spent either way.
        let moderated =
            pre_approved == 0 && policy_of(&mut tx, list, action).await? == "confirm_then_moderate";
        let state = if moderated {
            "pending_moderation"
        } else {
            "closed"
        };
        sqlx::query("UPDATE subscription_workflows SET state=$1 WHERE id=$2")
            .bind(state)
            .bind(&id)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        if !moderated {
            apply_membership(&mut tx, self.db, list, &address, action).await?;
            enqueue_confirmation_receipt(
                &mut tx,
                self.db,
                list,
                &original_email,
                action.name(),
                now_ms,
            )
            .await?;
        }
        Database::record_tx_with_context(
            &mut tx,
            &AuditContext::system(),
            "subscription.confirm",
            "list",
            list.as_str(),
            serde_json::json!({"workflow_id":id,"action":action.name(),"state":state}),
        )
        .await?;
        self.commit_command(tx, lease, now_ms).await
    }

    /// Subscribe `request.email` the way Mailman's registrar does: the
    /// list's `subscription_policy` decides what is still missing, and each
    /// `pre_*` flag supplies one of those steps in advance. An invitation
    /// always asks the address to accept, and counts as the approval.
    /// # Errors
    /// Returns `NotFound` for an unknown list, a validation error for an
    /// invalid or banned address, and database errors.
    pub async fn subscribe(
        &self,
        request: &AdminSubscription<'_>,
        context: &AuditContext,
        now_ms: i64,
    ) -> Result<SubscriptionOutcome> {
        let address = Address::new(request.email, request.display_name.to_owned())?;
        let mut tx = self.db.pool().begin().await.map_err(db_error)?;
        lock(&mut tx).await?;
        let policy: Option<String> =
            sqlx::query_scalar("SELECT subscription_policy FROM mailing_lists WHERE list_id=$1")
                .bind(request.list.as_str())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_error)?;
        let policy = policy.ok_or_else(|| Error::NotFound("list".into()))?;
        if crate::bans::is_banned(&mut *tx, request.list, &address.original_email).await? {
            return Err(Error::Validation("address is banned".into()));
        }
        cleanup_expired(&mut tx, now_ms).await?;
        let confirming = matches!(policy.as_str(), "confirm" | "confirm_then_moderate");
        let needs_token =
            request.invitation || !request.pre_verified || (confirming && !request.pre_confirmed);
        let needs_moderation = !request.invitation
            && !request.pre_approved
            && matches!(policy.as_str(), "moderate" | "confirm_then_moderate");
        if !needs_token && !needs_moderation {
            apply_membership(
                &mut tx,
                self.db,
                request.list,
                &address,
                SubscriptionAction::Join,
            )
            .await?;
            Database::record_tx_with_context(
                &mut tx,
                context,
                "subscription.subscribe",
                "list",
                request.list.as_str(),
                serde_json::json!({"policy":policy}),
            )
            .await?;
            tx.commit().await.map_err(db_error)?;
            return Ok(SubscriptionOutcome::Subscribed);
        }
        let mut secret = [0_u8; 32];
        rand::rngs::OsRng
            .try_fill_bytes(&mut secret)
            .map_err(db_error)?;
        let token = URL_SAFE_NO_PAD.encode(secret);
        let hash = format!("{:x}", Sha256::digest(secret));
        let id = Uuid::now_v7().to_string();
        let expires = now_ms
            .checked_add(86_400_000)
            .ok_or_else(|| Error::Validation("invalid time".into()))?;
        let token_owner = if needs_token {
            TokenOwner::Subscriber
        } else {
            TokenOwner::Moderator
        };
        // An approval already given (explicitly, or implied by inviting)
        // must survive the confirmation the subscriber still owes.
        let approved = i64::from(request.pre_approved || request.invitation || !needs_moderation);
        sqlx::query("INSERT INTO subscription_workflows(id,list_id,email,action,token_hash,created_at,expires_at,original_email,state,display_name,pre_approved) VALUES($1,$2,$3,'join',$4,$5,$6,$7,$8,$9,$10)")
            .bind(&id).bind(request.list.as_str()).bind(&address.email).bind(hash).bind(now_ms).bind(expires)
            .bind(&address.original_email).bind(token_owner.state()).bind(&address.display_name).bind(approved)
            .execute(&mut *tx).await.map_err(db_error)?;
        if needs_token {
            enqueue_confirmation(
                &mut tx,
                self.db,
                request.list,
                &address,
                confirmation_template(SubscriptionAction::Join, request.invitation),
                &token,
                now_ms,
            )
            .await?;
        }
        Database::record_tx_with_context(
            &mut tx,
            context,
            if request.invitation {
                "subscription.invite"
            } else {
                "subscription.request"
            },
            "list",
            request.list.as_str(),
            serde_json::json!({
                "workflow_id": id,
                "action": "join",
                "policy": policy,
                "token_owner": token_owner,
            }),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(SubscriptionOutcome::Held {
            token: id,
            token_owner,
        })
    }

    /// Undecided subscription requests on `list`, oldest first, narrowed by
    /// `filter`.
    /// # Errors
    /// Returns a database error.
    pub async fn pending(
        &self,
        list: &ListId,
        filter: RequestFilter,
    ) -> Result<Vec<PendingRequest>> {
        let rows = sqlx::query("SELECT id,list_id,original_email,display_name,action,state,created_at FROM subscription_workflows WHERE list_id=$1 AND state IN ('pending_confirmation','pending_moderation') AND ($2='' OR state=$2) AND ($3='' OR action=$3) ORDER BY created_at,id")
            .bind(list.as_str())
            .bind(filter.token_owner.map_or("", TokenOwner::state))
            .bind(filter.action.map_or("", SubscriptionAction::name))
            .fetch_all(self.db.pool())
            .await
            .map_err(db_error)?;
        rows.iter().map(pending_row).collect()
    }

    /// One undecided request by id.
    /// # Errors
    /// Returns `NotFound` when no undecided request has that id.
    pub async fn get(&self, id: &str) -> Result<PendingRequest> {
        let row = sqlx::query("SELECT id,list_id,original_email,display_name,action,state,created_at FROM subscription_workflows WHERE id=$1 AND state IN ('pending_confirmation','pending_moderation')")
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
            .map_err(db_error)?
            .ok_or_else(|| Error::NotFound("subscription request".into()))?;
        pending_row(&row)
    }

    /// Apply a moderator's decision to one undecided request — whether it
    /// waits for the moderator or still for the subscriber's confirmation,
    /// which acceptance then makes unnecessary. The membership change, the
    /// notices it produces and the audit event commit together; `reason`
    /// is recorded with the event.
    /// # Errors
    /// Returns `NotFound` when no request with that id is undecided, and
    /// database errors.
    pub async fn decide(
        &self,
        id: &str,
        decision: RequestDecision,
        reason: &str,
        context: &AuditContext,
    ) -> Result<()> {
        let mut tx = self.db.pool().begin().await.map_err(db_error)?;
        lock(&mut tx).await?;
        let row = sqlx::query("SELECT list_id,original_email,email,action FROM subscription_workflows WHERE id=$1 AND state IN ('pending_confirmation','pending_moderation')")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_error)?
            .ok_or_else(|| Error::NotFound("subscription request".into()))?;
        let list: ListId = row
            .try_get::<String, _>("list_id")
            .map_err(db_error)?
            .parse()?;
        let original_email: String = row.try_get("original_email").map_err(db_error)?;
        let action =
            SubscriptionAction::parse(&row.try_get::<String, _>("action").map_err(db_error)?)?;
        match decision {
            RequestDecision::Accept => {
                let address = Address::new(&original_email, String::new())?;
                if matches!(action, SubscriptionAction::Join)
                    && crate::bans::is_banned(&mut *tx, &list, &original_email).await?
                {
                    return Err(Error::Validation("address is banned".into()));
                }
                apply_membership(&mut tx, self.db, &list, &address, action).await?;
                close_request(&mut tx, id).await?;
            }
            RequestDecision::Reject => close_request(&mut tx, id).await?,
            RequestDecision::Discard => {
                sqlx::query("DELETE FROM subscription_workflows WHERE id=$1")
                    .bind(id)
                    .execute(&mut *tx)
                    .await
                    .map_err(db_error)?;
            }
            // Mailman's defer is an explicit "not yet": the request stays.
            RequestDecision::Defer => {}
        }
        Database::record_tx_with_context(
            &mut tx,
            context,
            decision.audit(),
            "list",
            list.as_str(),
            serde_json::json!({"workflow_id":id,"action":action.name(),"reason":reason}),
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }
}
/// Called only after an actual membership INSERT, inside its audited transaction.
/// Recipient and list identity come from the stored membership, never a caller.
/// Sends the member's welcome (`send_welcome_message`) and tells the
/// administrators (`admin_notify_mchanges`), each by its own switch.
pub(crate) async fn welcome_new_member(
    tx: &mut Transaction<'_, Any>,
    db: &Database,
    member: listmngr_core::MemberId,
) -> Result<()> {
    let stored = sqlx::query("SELECT m.list_id,a.original_email,l.send_welcome_message,l.admin_notify_mchanges,l.display_name FROM members m JOIN addresses a ON a.id=m.address_id JOIN mailing_lists l ON l.list_id=m.list_id WHERE m.id=$1 AND m.role='member'")
        .bind(member.to_string()).fetch_optional(&mut **tx).await.map_err(db_error)?;
    let Some(stored) = stored else {
        return Ok(());
    };
    let list: ListId = stored
        .try_get::<String, _>("list_id")
        .map_err(db_error)?
        .parse()?;
    let email: String = stored.try_get("original_email").map_err(db_error)?;
    let display_name: String = stored.try_get("display_name").map_err(db_error)?;
    // A banned address gets no welcome, without redefining administrative
    // admission (public join workflows enforce bans before insertion); the
    // administrators still learn of the membership.
    let welcome = stored
        .try_get::<i64, _>("send_welcome_message")
        .map_err(db_error)?
        != 0
        && !crate::bans::is_banned(&mut **tx, &list, &email).await?;
    if welcome {
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
        .await?;
    }
    if stored
        .try_get::<i64, _>("admin_notify_mchanges")
        .map_err(db_error)?
        != 0
    {
        notify_administrators_of_membership_change(tx, db, &list, &email, true).await?;
    }
    Ok(())
}

/// Mailman's `admin_notify_mchanges`: `list:admin:notice:subscribe` or
/// `list:admin:notice:unsubscribe` to every owner and moderator, each in
/// their own language, naming the member.
async fn notify_administrators_of_membership_change(
    tx: &mut Transaction<'_, Any>,
    db: &Database,
    list: &ListId,
    member: &str,
    subscribed: bool,
) -> Result<()> {
    let (subject, template) = if subscribed {
        (
            "notice-admin-subscribe-subject",
            "list:admin:notice:subscribe",
        )
    } else {
        (
            "notice-admin-unsubscribe-subject",
            "list:admin:notice:unsubscribe",
        )
    };
    let snapshot = crate::notices::list_snapshot(tx, list).await?;
    let recipients: Vec<String> = sqlx::query_scalar("SELECT a.original_email FROM addresses a WHERE EXISTS (SELECT 1 FROM members m WHERE m.address_id=a.id AND m.list_id=$1 AND m.role IN ('owner','moderator')) ORDER BY a.email")
        .bind(list.as_str()).fetch_all(&mut **tx).await.map_err(db_error)?;
    let now_ms = chrono::Utc::now().timestamp_millis();
    for email in &recipients {
        if listmngr_mail::owner::points_to_list(email, list) {
            continue;
        }
        enqueue_templated_notice(
            tx,
            db,
            list,
            Notice {
                to: email,
                reply_to: None,
                subject,
                subject_args: &[("display_name", &snapshot.display_name)],
                template,
            },
            |values| values.set("member", member.to_owned()),
            now_ms,
        )
        .await?;
    }
    Ok(())
}

/// Delete first with RETURNING so only the actual deletion winner can notify.
/// All recipient data is stored authority; the caller owns audit and commit.
/// Sends the goodbye (`send_goodbye_message`) and tells the administrators
/// (`admin_notify_mchanges`), each by its own switch.
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
    let stored = sqlx::query("SELECT a.original_email,l.send_goodbye_message,l.admin_notify_mchanges FROM addresses a JOIN mailing_lists l ON l.list_id=$1 WHERE a.id=$2")
        .bind(list.as_str()).bind(address).fetch_optional(&mut **tx).await.map_err(db_error)?;
    let Some(stored) = stored else {
        return Ok(true);
    };
    let email: String = stored.try_get("original_email").map_err(db_error)?;
    if stored
        .try_get::<i64, _>("send_goodbye_message")
        .map_err(db_error)?
        != 0
    {
        enqueue_goodbye(tx, db, &list, &email).await?;
    }
    if stored
        .try_get::<i64, _>("admin_notify_mchanges")
        .map_err(db_error)?
        != 0
    {
        notify_administrators_of_membership_change(tx, db, &list, &email, false).await?;
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

/// Mailman's probe: a notice to the member from a one-time VERP address,
/// so that its bounce — and only that — disables delivery.
pub(crate) async fn enqueue_probe(
    tx: &mut Transaction<'_, Any>,
    db: &Database,
    list: &ListId,
    member: &str,
    sender: &str,
    now_ms: i64,
) -> Result<()> {
    let snapshot = crate::notices::list_snapshot(tx, list).await?;
    let language =
        crate::notices::recipient_language(tx, &snapshot, member, db.default_language()).await?;
    let subject = listmngr_i18n::message(
        &language,
        "notice-probe-subject",
        &[("listname", &list.posting_address())],
    );
    let values =
        crate::notices::list_placeholders(&snapshot).set("sender_email", member.to_owned());
    let body =
        crate::notices::render(tx, &snapshot, "list:user:notice:probe", &language, &values).await?;
    let id = Uuid::now_v7().to_string();
    let date = chrono::DateTime::from_timestamp_millis(now_ms)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc2822();
    let host = list.mail_host().to_owned();
    let raw = crate::notices::serialize(
        &crate::notices::Envelope {
            from: sender,
            to: member,
            reply_to: None,
            subject: &subject,
            message_id_local: &id,
            mail_host: &host,
            date: &date,
            auto_submitted: "auto-generated",
        },
        &body,
    )?;
    enqueue_notice_from(tx, list, member, &id, raw, now_ms, Some(sender)).await
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

/// Mailman's `acknowledge` handler: when the poster is a member whose
/// resolved `acknowledge_posts` preference is on, send
/// `list:user:notice:post` from the list's bounces address.
pub(crate) async fn enqueue_post_acknowledgement(
    tx: &mut Transaction<'_, Any>,
    db: &Database,
    list: &ListId,
    sender: &str,
    subject: &str,
    now_ms: i64,
) -> Result<()> {
    let Ok(author) = listmngr_core::Address::new(sender, String::new()) else {
        return Ok(());
    };
    if listmngr_mail::owner::points_to_list(sender, list) {
        return Ok(());
    }
    // The member's own layer, then the address layer, then the user layer:
    // the same precedence `PreferencesRepo::resolve_member` applies.
    let wants: Option<i64> = sqlx::query_scalar(
        "SELECT COALESCE(pm.acknowledge_posts, pa.acknowledge_posts, pu.acknowledge_posts) FROM members m JOIN addresses a ON a.id=m.address_id LEFT JOIN preferences pm ON pm.id=m.preferences_id LEFT JOIN preferences pa ON pa.id=a.preferences_id LEFT JOIN users u ON u.id=m.user_id LEFT JOIN preferences pu ON pu.id=u.preferences_id WHERE m.list_id=$1 AND a.email=$2 AND m.role='member' LIMIT 1",
    )
    .bind(list.as_str())
    .bind(&author.email)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_error)?
    .flatten();
    if wants != Some(1) {
        return Ok(());
    }
    let snapshot = crate::notices::list_snapshot(tx, list).await?;
    let language =
        crate::notices::recipient_language(tx, &snapshot, sender, db.default_language()).await?;
    let shown_subject = if subject.trim().is_empty() {
        listmngr_i18n::message(&language, "notice-no-subject", &[])
    } else {
        subject.to_owned()
    };
    let subject_line = listmngr_i18n::message(
        &language,
        "notice-post-ack-subject",
        &[("display_name", &snapshot.display_name)],
    );
    let values = crate::notices::list_placeholders(&snapshot).set("subject", shown_subject);
    let body =
        crate::notices::render(tx, &snapshot, "list:user:notice:post", &language, &values).await?;
    let id = Uuid::now_v7().to_string();
    let date = chrono::DateTime::from_timestamp_millis(now_ms)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc2822();
    let host = list.mail_host().to_owned();
    let from = list.bounces_address();
    let raw = crate::notices::serialize(
        &crate::notices::Envelope {
            from: &from,
            to: sender,
            reply_to: None,
            subject: &subject_line,
            message_id_local: &id,
            mail_host: &host,
            date: &date,
            auto_submitted: "auto-generated",
        },
        &body,
    )?;
    enqueue_notice(tx, list, sender, &id, raw, now_ms).await
}

/// Mailman's content-filter `forward` notice: the moderators (the owners
/// when the list has none, so the only copy is never lost) receive a short
/// explanation with the unfiltered original attached as `message/rfc822`.
pub(crate) async fn enqueue_content_filter_forward(
    tx: &mut Transaction<'_, Any>,
    db: &Database,
    list: &ListId,
    message_id: crate::mail_queue::MessageId,
    now_ms: i64,
) -> Result<()> {
    let snapshot = crate::notices::list_snapshot(tx, list).await?;
    let original: Vec<u8> = sqlx::query_scalar("SELECT b.raw FROM messages m JOIN message_blobs b ON b.store_key=m.store_key WHERE m.id=$1")
        .bind(message_id.0.to_string()).fetch_one(&mut **tx).await.map_err(db_error)?;
    let mut recipients: Vec<String> = sqlx::query_scalar("SELECT a.original_email FROM addresses a WHERE EXISTS (SELECT 1 FROM members m WHERE m.address_id=a.id AND m.list_id=$1 AND m.role='moderator') ORDER BY a.email")
        .bind(list.as_str()).fetch_all(&mut **tx).await.map_err(db_error)?;
    if recipients.is_empty() {
        recipients = sqlx::query_scalar("SELECT a.original_email FROM addresses a WHERE EXISTS (SELECT 1 FROM members m WHERE m.address_id=a.id AND m.list_id=$1 AND m.role='owner') ORDER BY a.email")
            .bind(list.as_str()).fetch_all(&mut **tx).await.map_err(db_error)?;
    }
    let date = chrono::DateTime::from_timestamp_millis(now_ms)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc2822();
    let host = list.mail_host().to_owned();
    let from = list.owner_address();
    for email in &recipients {
        if listmngr_mail::owner::points_to_list(email, list) {
            continue;
        }
        let language =
            crate::notices::recipient_language(tx, &snapshot, email, db.default_language()).await?;
        let subject = listmngr_i18n::message(&language, "notice-content-filter-subject", &[]);
        let body = listmngr_i18n::message(
            &language,
            "content-filter-forward-body",
            &[("display_name", &snapshot.display_name)],
        );
        let id = Uuid::now_v7().to_string();
        let raw = crate::notices::serialize_with_message(
            &crate::notices::Envelope {
                from: &from,
                to: email,
                reply_to: None,
                subject: &subject,
                message_id_local: &id,
                mail_host: &host,
                date: &date,
                auto_submitted: "auto-generated",
            },
            &body,
            &original,
        )?;
        enqueue_notice(tx, list, email, &id, raw, now_ms).await?;
    }
    Ok(())
}

/// One templated notice to enqueue: who receives it, which template renders
/// the body, and which catalog message (with arguments) is its subject.
pub(crate) struct Notice<'a> {
    pub(crate) to: &'a str,
    pub(crate) reply_to: Option<&'a str>,
    pub(crate) subject: &'a str,
    pub(crate) subject_args: &'a [(&'a str, &'a str)],
    pub(crate) template: &'a str,
}

/// Render the notice in the recipient's language and enqueue it job-bound.
/// `placeholders` extends the list's own.
pub(crate) async fn enqueue_templated_notice(
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
            auto_submitted: "auto-generated",
        },
        &body,
    )?;
    enqueue_notice(tx, list, to, &id, raw, now_ms).await
}

// Private raw producer for generated subscription/moderation MIME only.
// The historical workflow_notices table is deliberately just job_id provenance.
pub(crate) async fn enqueue_notice(
    tx: &mut Transaction<'_, Any>,
    list: &ListId,
    email: &str,
    id: &str,
    raw: Vec<u8>,
    now_ms: i64,
) -> Result<()> {
    enqueue_notice_from(tx, list, email, id, raw, now_ms, None).await
}

/// [`enqueue_notice`] with an explicit envelope sender (a probe's one-time
/// bounce address); `None` keeps the null reverse path notices use.
pub(crate) async fn enqueue_notice_from(
    tx: &mut Transaction<'_, Any>,
    list: &ListId,
    email: &str,
    id: &str,
    raw: Vec<u8>,
    now_ms: i64,
    mail_from: Option<&str>,
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
    sqlx::query("INSERT INTO workflow_notices(job_id,mail_from) VALUES($1,$2)")
        .bind(job.id.0.to_string())
        .bind(mail_from)
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
    let deleted=sqlx::query("DELETE FROM subscription_workflows WHERE id IN (SELECT id FROM subscription_workflows WHERE expires_at<=$1 AND state<>'pending_moderation' ORDER BY expires_at,id LIMIT 100)")
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

/// The list's policy for this action, or `confirm` when the list is gone.
async fn policy_of(
    tx: &mut Transaction<'_, Any>,
    list: &ListId,
    action: SubscriptionAction,
) -> Result<String> {
    let column = match action {
        SubscriptionAction::Join => "subscription_policy",
        SubscriptionAction::Leave => "unsubscription_policy",
    };
    let policy: Option<String> = sqlx::query_scalar(&format!(
        "SELECT {column} FROM mailing_lists WHERE list_id=$1"
    ))
    .bind(list.as_str())
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_error)?;
    Ok(policy.unwrap_or_else(|| "confirm".to_owned()))
}

/// Subscribe or unsubscribe the request's own stored mailbox, with the
/// welcome/goodbye notices the list configures. Idempotent: a join for an
/// existing member and a leave for a stranger both do nothing.
async fn apply_membership(
    tx: &mut Transaction<'_, Any>,
    db: &Database,
    list: &ListId,
    address: &Address,
    action: SubscriptionAction,
) -> Result<()> {
    let existing=sqlx::query("SELECT m.id,m.preferences_id FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id=$1 AND a.email=$2 AND m.role='member'")
        .bind(list.as_str()).bind(&address.email).fetch_optional(&mut **tx).await.map_err(db_error)?;
    match (action, existing) {
        (SubscriptionAction::Join, None) => {
            crate::insert_mass_members(
                tx,
                db,
                list,
                MemberRole::Member,
                SubscriptionMode::AsAddress,
                &[address],
            )
            .await
        }
        (SubscriptionAction::Leave, Some(member)) => {
            let id: String = member.try_get("id").map_err(db_error)?;
            let pref: String = member.try_get("preferences_id").map_err(db_error)?;
            crate::delete_mass_members(tx, db, &[(id, pref)]).await
        }
        _ => Ok(()),
    }
}

/// A decided request keeps its row (and therefore its address cooldown)
/// until the expiry sweep removes it.
async fn close_request(tx: &mut Transaction<'_, Any>, id: &str) -> Result<()> {
    sqlx::query("UPDATE subscription_workflows SET state='closed', consumed=1 WHERE id=$1")
        .bind(id)
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    Ok(())
}

/// A request that never issues a confirmation token: the `open` policy acts
/// on the roster at once, the `moderate` policy waits for a moderator.
struct Unconfirmed<'a> {
    list: &'a ListId,
    address: &'a Address,
    action: SubscriptionAction,
    id: &'a str,
    policy: &'a str,
    immediate: bool,
}

async fn record_unconfirmed(
    tx: &mut Transaction<'_, Any>,
    db: &Database,
    request: &Unconfirmed<'_>,
) -> Result<()> {
    if request.immediate {
        apply_membership(tx, db, request.list, request.address, request.action).await?;
    }
    Database::record_tx_with_context(
        tx,
        &AuditContext::system(),
        if request.immediate {
            "subscription.open"
        } else {
            "subscription.request"
        },
        "list",
        request.list.as_str(),
        serde_json::json!({
            "workflow_id": request.id,
            "action": request.action.name(),
            "policy": request.policy,
        }),
    )
    .await
}

/// The confirmation mail carrying the token. Its Subject must stay exactly
/// `confirm TOKEN`: replying with it intact is the email confirmation path.
/// Mailman's template for the mail that carries a confirmation token.
const fn confirmation_template(action: SubscriptionAction, invitation: bool) -> &'static str {
    match (action, invitation) {
        (_, true) => "list:user:action:invite",
        (SubscriptionAction::Join, false) => "list:user:action:subscribe",
        (SubscriptionAction::Leave, false) => "list:user:action:unsubscribe",
    }
}

async fn enqueue_confirmation(
    tx: &mut Transaction<'_, Any>,
    db: &Database,
    list: &ListId,
    address: &Address,
    template: &str,
    token: &str,
    now_ms: i64,
) -> Result<()> {
    let confirm_address = list.address_with_suffix("confirm");
    let confirm_uri = format!("/api/v1/public/lists/{list}/confirm");
    let user_email = address.original_email.clone();
    let token = token.to_owned();
    enqueue_templated_notice(
        tx,
        db,
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
    .await
}

fn pending_row(row: &sqlx::any::AnyRow) -> Result<PendingRequest> {
    Ok(PendingRequest {
        id: row.try_get("id").map_err(db_error)?,
        list_id: row
            .try_get::<String, _>("list_id")
            .map_err(db_error)?
            .parse()?,
        email: row.try_get("original_email").map_err(db_error)?,
        display_name: row.try_get("display_name").map_err(db_error)?,
        action: SubscriptionAction::parse(&row.try_get::<String, _>("action").map_err(db_error)?)?,
        token_owner: TokenOwner::from_state(&row.try_get::<String, _>("state").map_err(db_error)?)?,
        requested_at: row.try_get("created_at").map_err(db_error)?,
    })
}

/// Support ASCII dot-atom mailboxes only for notices: no SMTPUTF8, no
/// quoted local parts.
fn notice_mailbox(email: &str) -> Result<()> {
    let local = email.split_once('@').map_or("", |(local, _)| local);
    if !email.is_ascii()
        || !local
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".!#$%&'*+-/=?^_`{|}~".contains(&b))
    {
        return Err(Error::Validation("unsupported notice mailbox".into()));
    }
    Ok(())
}

/// What the command bot is about to send back.
enum BotReply<'a> {
    Help,
    Echo(&'a str),
}

impl BotReply<'_> {
    const fn subject(&self) -> &'static str {
        match self {
            Self::Help => "notice-help-subject",
            Self::Echo(_) => "notice-echo-subject",
        }
    }
    const fn template(&self) -> &'static str {
        match self {
            Self::Help => "list:user:notice:help",
            Self::Echo(_) => "list:user:notice:echo",
        }
    }
    const fn audit(&self) -> &'static str {
        match self {
            Self::Help => "subscription.help",
            Self::Echo(_) => "subscription.echo",
        }
    }
    const fn echoed(&self) -> &str {
        match self {
            Self::Help => "",
            Self::Echo(text) => text,
        }
    }
}
