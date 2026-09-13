//! Own-membership departure; preview and mutation use the same live DB authority.
use super::WebSession;
use crate::{AuditContext, Database, db_error};
use listmngr_core::{Error, ListId, MemberId, Result};
use sqlx::Row;

impl Database {
    /// Recover an owned bounce-disabled membership.
    /// # Errors
    /// Rejects ineligible membership or stale authority.
    pub async fn browser_recover(&self, session: &WebSession, member: MemberId) -> Result<ListId> {
        self.browser_bounce_recovery(session, member, true)
            .await
            .map(|(list, _)| list)
    }

    /// Preview eligibility without changing membership or audit history.
    /// # Errors
    /// Rejects stale sessions and any status other than direct member `by_bounces`.
    pub async fn browser_recover_preview(
        &self,
        session: &WebSession,
        member: MemberId,
    ) -> Result<(ListId, String)> {
        self.browser_bounce_recovery(session, member, false).await
    }

    async fn browser_bounce_recovery(
        &self,
        session: &WebSession,
        member: MemberId,
        confirmed: bool,
    ) -> Result<(ListId, String)> {
        // Reuse the NOWAIT/retry table barrier, rather than adding a new row lock
        // order against the scorer and maintenance member/address/preference writes.
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_user_tx(&mut tx, session).await?;
        let row = sqlx::query("SELECT m.list_id,m.preferences_id,a.email FROM members m JOIN addresses a ON a.id=m.address_id JOIN preferences p ON p.id=m.preferences_id WHERE m.id=$1 AND m.role='member' AND a.user_id=$2 AND a.verified_on IS NOT NULL AND (m.subscription_mode='as_address' OR m.user_id=$2) AND p.delivery_status='by_bounces'")
            .bind(member.to_string()).bind(user.to_string()).fetch_optional(&mut *tx).await.map_err(db_error)?
            .ok_or_else(|| Error::Forbidden("browser bounce recovery eligibility".into()))?;
        let list: ListId = row
            .try_get::<String, _>("list_id")
            .map_err(db_error)?
            .parse()?;
        let email: String = row.try_get("email").map_err(db_error)?;
        if confirmed {
            let preference: String = row.try_get("preferences_id").map_err(db_error)?;
            sqlx::query("UPDATE preferences SET delivery_status='enabled',delivery_generation=delivery_generation+1 WHERE id=$1")
                .bind(preference)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
            sqlx::query("UPDATE members SET bounce_score=0,last_bounce_received=NULL,total_warnings_sent=0,last_warning_sent=NULL WHERE id=$1")
                .bind(member.to_string()).execute(&mut *tx).await.map_err(db_error)?;
            Self::record_tx_with_context(
                &mut tx,
                &AuditContext::new(Some(user), None, None),
                "bounce.recover",
                "member",
                &member.to_string(),
                serde_json::json!({"source":"browser","list_id":list,"delivery_status":"enabled"}),
            )
            .await?;
        }
        Self::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)?;
        Ok((list, email))
    }

    /// Preview the list and canonical address of an owned, verified subscription.
    /// # Errors
    /// Rejects stale sessions, unverified/foreign identities and non-member roles.
    pub async fn browser_leave_preview(
        &self,
        session: &WebSession,
        member: MemberId,
    ) -> Result<(ListId, String)> {
        self.browser_membership_departure(session, member, false)
            .await
    }

    /// Remove only the owned subscription and its preferences, atomically audited.
    /// # Errors
    /// Rejects stale authority, replay, non-member roles and database/audit errors.
    pub async fn browser_leave(&self, session: &WebSession, member: MemberId) -> Result<ListId> {
        self.browser_membership_departure(session, member, true)
            .await
            .map(|(list, _)| list)
    }

    async fn browser_membership_departure(
        &self,
        session: &WebSession,
        member: MemberId,
        confirmed: bool,
    ) -> Result<(ListId, String)> {
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_user_tx(&mut tx, session).await?;
        let row = sqlx::query("SELECT m.list_id,m.preferences_id,a.email FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.id=$1 AND m.role='member' AND a.user_id=$2 AND a.verified_on IS NOT NULL AND (m.subscription_mode='as_address' OR m.user_id=$2)")
            .bind(member.to_string()).bind(user.to_string()).fetch_optional(&mut *tx).await.map_err(db_error)?
            .ok_or_else(|| Error::Forbidden("browser member ownership".into()))?;
        let list: ListId = row
            .try_get::<String, _>("list_id")
            .map_err(db_error)?
            .parse()?;
        let email: String = row.try_get("email").map_err(db_error)?;
        if confirmed {
            let preferences: String = row.try_get("preferences_id").map_err(db_error)?;
            crate::delete_mass_members(&mut tx, self, &[(member.to_string(), preferences)]).await?;
            Self::record_tx_with_context(
                &mut tx,
                &AuditContext::new(Some(user), None, None),
                "member.delete",
                "member",
                &member.to_string(),
                serde_json::json!({"source":"browser","list_id":list}),
            )
            .await?;
        }
        // Audit and deletion may also wait; session credentials are unchanged here.
        Self::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)?;
        Ok((list, email))
    }
}
