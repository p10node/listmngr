//! The owner's member management: rosters of every role, one member's
//! options and bounce state, mass subscription and removal, and the export.
//!
//! Every write checks the owner's live authority, changes the rows and
//! records the audit event in one transaction; the mass subscription runs
//! the registrar's own workflow per address so the list's policy and the
//! `pre_*` flags mean what they mean on the REST API.
use crate::web_sessions::WebSession;
use crate::workflows::{AdminSubscription, SubscriptionOutcome};
use crate::{AuditContext, Database, NewMember, PreferencesRepo, db_error, workflows};
use listmngr_core::{
    Address, Error, ListId, Member, MemberId, MemberRole, ModerationAction, Preferences, Result,
    SubscriptionMode,
};
use sqlx::Row;

/// One row of a roster page.
#[derive(Debug, Clone)]
pub struct RosterRow {
    pub id: MemberId,
    pub email: String,
    pub display_name: String,
    pub moderation_action: Option<ModerationAction>,
    /// The member-level delivery mode, if one is set.
    pub delivery_mode: Option<String>,
    /// The member-level delivery status, if one is set.
    pub delivery_status: Option<String>,
    pub bounce_score: f64,
}

/// One member as the options page shows them.
#[derive(Debug, Clone)]
pub struct MemberDetail {
    pub member: Member,
    pub email: String,
    /// The member-level preferences as stored (unset values are inherited).
    pub preferences: Preferences,
    /// What the member effectively gets, every layer resolved.
    pub resolved: Preferences,
}

/// What the options form sets.
#[derive(Debug, Clone)]
pub struct MemberOptions {
    pub moderation_action: Option<ModerationAction>,
    pub display_name: String,
    pub role: MemberRole,
    pub preferences: Preferences,
}

/// The workflow flags of a mass subscription (Mailman's REST names).
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, Default)]
pub struct MassFlags {
    pub pre_verified: bool,
    pub pre_confirmed: bool,
    pub pre_approved: bool,
    pub invitation: bool,
}

/// What happened to one address of a mass subscription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MassOutcome {
    Subscribed,
    /// A request waits: for the address (confirmation or invitation) or a
    /// moderator.
    Held,
    AlreadyMember,
    /// The line could not be read as an address.
    Invalid,
    /// The registrar refused it (banned, or another validation).
    Refused(String),
}

/// One exported row.
#[derive(Debug, Clone)]
pub struct ExportRow {
    pub email: String,
    pub display_name: String,
    pub role: String,
    pub subscription_mode: String,
    pub delivery_mode: String,
    pub delivery_status: String,
    pub moderation_action: String,
    pub bounce_score: f64,
    pub last_bounce_received: String,
    pub created_at: String,
}

/// The most rows an export carries.
pub const EXPORT_LIMIT: i64 = 10_000;

fn escape_like(query: &str) -> String {
    format!(
        "%{}%",
        query
            .to_lowercase()
            .replace('!', "!!")
            .replace('%', "!%")
            .replace('_', "!_")
    )
}

impl Database {
    /// One page (21 rows, the last a hint of more) of a role's roster,
    /// narrowed by an email substring, for the list's owner.
    /// # Errors
    /// Rejects revoked authority, an overlong query, a bad offset and
    /// database failures.
    pub async fn browser_roster(
        &self,
        session: &WebSession,
        list: &ListId,
        role: MemberRole,
        offset: i64,
        query: &str,
    ) -> Result<Vec<RosterRow>> {
        crate::web_admin::valid_offset(offset)?;
        if query.len() > 320 || query.chars().any(char::is_control) {
            return Err(Error::Validation("invalid member search".into()));
        }
        let mut tx = self.browser_write_tx().await?;
        Self::browser_owner_tx(&mut tx, session, list).await?;
        let rows = sqlx::query("SELECT m.id,substr(a.email,1,320) AS email,substr(m.display_name,1,256) AS display_name,m.moderation_action,p.delivery_mode,p.delivery_status,m.bounce_score FROM members m JOIN addresses a ON a.id=m.address_id JOIN preferences p ON p.id=m.preferences_id WHERE m.list_id=$1 AND m.role=$2 AND a.email LIKE $4 ESCAPE '!' ORDER BY a.email,m.id LIMIT 21 OFFSET $3")
            .bind(list.as_str()).bind(role.as_str()).bind(offset).bind(escape_like(query))
            .fetch_all(&mut *tx).await.map_err(db_error)?;
        let members = rows
            .iter()
            .map(|row| {
                Ok(RosterRow {
                    id: row
                        .try_get::<String, _>("id")
                        .map_err(db_error)?
                        .parse()
                        .map_err(db_error)?,
                    email: row.try_get("email").map_err(db_error)?,
                    display_name: row.try_get("display_name").map_err(db_error)?,
                    moderation_action: row
                        .try_get::<Option<String>, _>("moderation_action")
                        .map_err(db_error)?
                        .map(|value| value.parse())
                        .transpose()?,
                    delivery_mode: row.try_get("delivery_mode").map_err(db_error)?,
                    delivery_status: row.try_get("delivery_status").map_err(db_error)?,
                    bounce_score: row.try_get("bounce_score").map_err(db_error)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Self::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(members)
    }

    /// One member of the list, with stored and effective preferences.
    /// # Errors
    /// Rejects revoked authority; not found for a member of another list.
    pub async fn browser_member_detail(
        &self,
        session: &WebSession,
        list: &ListId,
        member: MemberId,
    ) -> Result<MemberDetail> {
        let mut tx = self.browser_write_tx().await?;
        Self::browser_owner_tx(&mut tx, session, list).await?;
        tx.commit().await.map_err(db_error)?;
        let stored = self.members().get(member).await?;
        if &stored.list_id != list {
            return Err(Error::NotFound("member".into()));
        }
        let email: String = sqlx::query_scalar("SELECT email FROM addresses WHERE id=$1")
            .bind(stored.address_id.to_string())
            .fetch_one(self.pool())
            .await
            .map_err(db_error)?;
        let preferences = self.preferences().get(stored.preferences_id).await?;
        let resolved = self
            .preferences()
            .resolve_member(member, self.default_language())
            .await?;
        Ok(MemberDetail {
            member: stored,
            email,
            preferences,
            resolved,
        })
    }

    /// Set a member's posting override, display name, role and member-level
    /// preferences in one audited transaction.
    /// # Errors
    /// Rejects revoked authority, a member of another list, and database
    /// or audit failures.
    pub async fn browser_member_options(
        &self,
        session: &WebSession,
        list: &ListId,
        member: MemberId,
        options: &MemberOptions,
    ) -> Result<()> {
        if options.display_name.chars().count() > 256
            || options.display_name.chars().any(char::is_control)
        {
            return Err(Error::Validation("display_name".into()));
        }
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_owner_tx(&mut tx, session, list).await?;
        let preferences_id: Option<String> =
            sqlx::query_scalar("SELECT preferences_id FROM members WHERE id=$1 AND list_id=$2")
                .bind(member.to_string())
                .bind(list.as_str())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_error)?;
        let preferences_id = preferences_id.ok_or_else(|| Error::NotFound("member".into()))?;
        sqlx::query(
            "UPDATE members SET moderation_action=$1,display_name=$2,role=$3 WHERE id=$4 AND list_id=$5",
        )
        .bind(options.moderation_action.map(|a| a.to_string()))
        .bind(&options.display_name)
        .bind(options.role.as_str())
        .bind(member.to_string())
        .bind(list.as_str())
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        PreferencesRepo::set_tx(
            &mut tx,
            preferences_id.parse().map_err(db_error)?,
            &options.preferences,
        )
        .await?;
        Self::record_tx_with_context(
            &mut tx,
            &AuditContext::new(Some(user), None, None),
            "member.update",
            "member",
            &member.to_string(),
            serde_json::json!({
                "moderation_action": options.moderation_action,
                "display_name": options.display_name,
                "role": options.role,
                "preferences": options.preferences,
                "list_id": list,
                "source": "browser-admin",
            }),
        )
        .await?;
        Self::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)
    }

    /// Re-enable delivery and forget the bounce history of a member, as the
    /// owner.
    /// # Errors
    /// Rejects revoked authority, a member of another list, and database
    /// or audit failures.
    pub async fn browser_member_bounce_reset(
        &self,
        session: &WebSession,
        list: &ListId,
        member: MemberId,
    ) -> Result<()> {
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_owner_tx(&mut tx, session, list).await?;
        let preferences_id: Option<String> =
            sqlx::query_scalar("SELECT preferences_id FROM members WHERE id=$1 AND list_id=$2")
                .bind(member.to_string())
                .bind(list.as_str())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_error)?;
        let preferences_id = preferences_id.ok_or_else(|| Error::NotFound("member".into()))?;
        sqlx::query("UPDATE preferences SET delivery_status=CASE WHEN delivery_status='by_bounces' THEN 'enabled' ELSE delivery_status END,delivery_generation=delivery_generation+1 WHERE id=$1")
            .bind(preferences_id)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        sqlx::query("UPDATE members SET bounce_score=0,last_bounce_received=NULL,total_warnings_sent=0,last_warning_sent=NULL WHERE id=$1")
            .bind(member.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        Self::record_tx_with_context(
            &mut tx,
            &AuditContext::new(Some(user), None, None),
            "bounce.recover",
            "member",
            &member.to_string(),
            serde_json::json!({"source":"owner","list_id":list,"delivery_status":"enabled"}),
        )
        .await?;
        Self::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)
    }

    /// Subscribe each `(email, display name)` as the owner: members go
    /// through the registrar's workflow with the list's policy and the
    /// flags; other roles are added directly. Returns one outcome per entry.
    /// # Errors
    /// Rejects revoked authority and database failures; a refused address
    /// is an outcome, not an error.
    pub async fn browser_mass_subscribe(
        &self,
        session: &WebSession,
        list: &ListId,
        entries: &[(String, String)],
        role: MemberRole,
        flags: MassFlags,
        now_ms: i64,
    ) -> Result<Vec<(String, MassOutcome)>> {
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_owner_tx(&mut tx, session, list).await?;
        tx.commit().await.map_err(db_error)?;
        let context = AuditContext::new(Some(user), None, None);
        let mut outcomes = Vec::with_capacity(entries.len());
        for (email, display_name) in entries {
            let outcome = match Address::new(email, display_name.clone()) {
                Err(_) => MassOutcome::Invalid,
                Ok(address) => {
                    let existing: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id=$1 AND m.role=$2 AND a.email=$3")
                        .bind(list.as_str()).bind(role.as_str()).bind(&address.email)
                        .fetch_one(self.pool()).await.map_err(db_error)?;
                    if existing > 0 {
                        MassOutcome::AlreadyMember
                    } else if role == MemberRole::Member {
                        let request = AdminSubscription {
                            list,
                            email: &address.original_email,
                            display_name,
                            pre_verified: flags.pre_verified,
                            pre_confirmed: flags.pre_confirmed,
                            pre_approved: flags.pre_approved,
                            invitation: flags.invitation,
                        };
                        match self.workflows().subscribe(&request, &context, now_ms).await {
                            Ok(SubscriptionOutcome::Subscribed) => MassOutcome::Subscribed,
                            Ok(SubscriptionOutcome::Held { .. }) => MassOutcome::Held,
                            Err(Error::Validation(message)) => MassOutcome::Refused(message),
                            Err(error) => return Err(error),
                        }
                    } else {
                        let new = NewMember {
                            list_id: list.clone(),
                            email: address.original_email.clone(),
                            role,
                            subscription_mode: SubscriptionMode::AsAddress,
                            display_name: display_name.clone(),
                        };
                        match self
                            .members()
                            .subscribe_with_context(new, flags.pre_verified, &context)
                            .await
                        {
                            Ok(_) => MassOutcome::Subscribed,
                            Err(Error::Validation(message) | Error::Conflict(message)) => {
                                MassOutcome::Refused(message)
                            }
                            Err(error) => return Err(error),
                        }
                    }
                }
            };
            outcomes.push((email.clone(), outcome));
        }
        Ok(outcomes)
    }

    /// Remove the given members of the list — by id, or by the address of
    /// a member-role subscription — each with its own `member.delete`
    /// event, all in one transaction. Returns how many went.
    /// # Errors
    /// Rejects revoked authority and database or audit failures; an id or
    /// address that is not on this list is skipped.
    pub async fn browser_mass_remove(
        &self,
        session: &WebSession,
        list: &ListId,
        ids: &[MemberId],
        emails: &[String],
    ) -> Result<usize> {
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_owner_tx(&mut tx, session, list).await?;
        let mut targets: Vec<(String, String)> = Vec::new();
        for id in ids {
            let row =
                sqlx::query("SELECT id,preferences_id FROM members WHERE id=$1 AND list_id=$2")
                    .bind(id.to_string())
                    .bind(list.as_str())
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(db_error)?;
            if let Some(row) = row {
                targets.push((
                    row.try_get("id").map_err(db_error)?,
                    row.try_get("preferences_id").map_err(db_error)?,
                ));
            }
        }
        for email in emails {
            let Ok(address) = Address::new(email.trim(), String::new()) else {
                continue;
            };
            let row = sqlx::query("SELECT m.id,m.preferences_id FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id=$1 AND m.role='member' AND a.email=$2")
                .bind(list.as_str())
                .bind(&address.email)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_error)?;
            if let Some(row) = row {
                targets.push((
                    row.try_get("id").map_err(db_error)?,
                    row.try_get("preferences_id").map_err(db_error)?,
                ));
            }
        }
        targets.sort();
        targets.dedup();
        let context = AuditContext::new(Some(user), None, None);
        for (id, preferences_id) in &targets {
            if !workflows::delete_member_with_goodbye(&mut tx, self, id).await? {
                continue;
            }
            sqlx::query("DELETE FROM preferences WHERE id=$1")
                .bind(preferences_id)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
            Self::record_tx_with_context(
                &mut tx,
                &context,
                "member.delete",
                "member",
                id,
                serde_json::json!({"list_id": list, "source": "browser-admin"}),
            )
            .await?;
        }
        Self::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(targets.len())
    }

    /// A role's roster for export, at most [`EXPORT_LIMIT`] rows.
    /// # Errors
    /// Rejects revoked authority and database failures.
    pub async fn browser_export_members(
        &self,
        session: &WebSession,
        list: &ListId,
        role: MemberRole,
    ) -> Result<Vec<ExportRow>> {
        let mut tx = self.browser_write_tx().await?;
        Self::browser_owner_tx(&mut tx, session, list).await?;
        tx.commit().await.map_err(db_error)?;
        let rows = sqlx::query("SELECT a.email,m.display_name,m.role,m.subscription_mode,p.delivery_mode,p.delivery_status,m.moderation_action,m.bounce_score,m.last_bounce_received,m.created_at FROM members m JOIN addresses a ON a.id=m.address_id JOIN preferences p ON p.id=m.preferences_id WHERE m.list_id=$1 AND m.role=$2 ORDER BY a.email,m.id LIMIT $3")
            .bind(list.as_str()).bind(role.as_str()).bind(EXPORT_LIMIT)
            .fetch_all(self.pool()).await.map_err(db_error)?;
        rows.iter()
            .map(|row| {
                let text = |column: &str| -> Result<String> {
                    Ok(row
                        .try_get::<Option<String>, _>(column)
                        .map_err(db_error)?
                        .unwrap_or_default())
                };
                Ok(ExportRow {
                    email: text("email")?,
                    display_name: text("display_name")?,
                    role: text("role")?,
                    subscription_mode: text("subscription_mode")?,
                    delivery_mode: text("delivery_mode")?,
                    delivery_status: text("delivery_status")?,
                    moderation_action: text("moderation_action")?,
                    bounce_score: row.try_get("bounce_score").map_err(db_error)?,
                    last_bounce_received: text("last_bounce_received")?,
                    created_at: text("created_at")?,
                })
            })
            .collect()
    }
}
