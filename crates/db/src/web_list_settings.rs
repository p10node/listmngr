//! The owner's list settings beyond the configuration patch.
//!
//! Header rules, bans, templates, archivers, digest actions and deleting
//! the list. Every write checks the owner's live authority, changes the
//! rows and records the audit event in one transaction; site bans need a
//! server owner.
use crate::bans::BanRepo;
use crate::header_matches::{HeaderMatchRepo, HeaderMatchRow};
use crate::templates::{Scope, TemplateRepo};
use crate::web_sessions::WebSession;
use crate::{AuditContext, Database, ListRepo, Template, db_error};
use listmngr_core::{Error, ListId, MailingList, Result, UserId};

/// One edit of the list's header rules.
#[derive(Debug, Clone)]
pub enum HeaderMatchChange {
    Append(HeaderMatchRow),
    Update(usize, HeaderMatchRow),
    /// Move the row at `.0` to position `.1`.
    Move(usize, usize),
    Remove(usize),
}

/// A template as the editor sees it: the stored override, if any, and the
/// body the list would use right now with where it comes from.
#[derive(Debug, Clone)]
pub struct TemplateView {
    pub stored: Option<String>,
    pub effective: String,
    pub source: String,
}

impl Database {
    /// The owner's authority, checked and released: reads that follow run
    /// on the pool, as the single `SQLite` connection is the transaction.
    async fn owner_check(&self, session: &WebSession, list: &ListId) -> Result<()> {
        let mut tx = self.browser_write_tx().await?;
        Self::browser_owner_tx(&mut tx, session, list).await?;
        tx.commit().await.map_err(db_error)
    }

    async fn server_owner_check(&self, session: &WebSession) -> Result<()> {
        let mut tx = self.browser_write_tx().await?;
        Self::server_owner_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)
    }

    pub(crate) async fn server_owner_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Any>,
        session: &WebSession,
    ) -> Result<UserId> {
        let user = Self::browser_user_tx(tx, session).await?;
        let allowed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users u JOIN addresses a ON a.user_id=u.id WHERE u.id=$1 AND u.is_server_owner=1 AND a.verified_on IS NOT NULL")
            .bind(user.to_string())
            .fetch_one(&mut **tx)
            .await
            .map_err(db_error)?;
        if allowed == 0 {
            return Err(Error::Forbidden("browser server owner authority".into()));
        }
        Ok(user)
    }

    /// The list's header rules, in position order, for its owner.
    /// # Errors
    /// Rejects revoked authority and database failures.
    pub async fn browser_header_matches(
        &self,
        session: &WebSession,
        list: &ListId,
    ) -> Result<Vec<HeaderMatchRow>> {
        self.owner_check(session, list).await?;
        self.header_matches().list(list).await
    }

    /// Apply one header-rule edit with the owner's authority.
    /// # Errors
    /// Rejects revoked authority, an invalid row, a position past the end,
    /// a duplicate, and database or audit failures.
    pub async fn browser_header_match_change(
        &self,
        session: &WebSession,
        list: &ListId,
        change: HeaderMatchChange,
    ) -> Result<()> {
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_owner_tx(&mut tx, session, list).await?;
        let context = AuditContext::new(Some(user), None, None);
        let out_of_range = |position: usize| Error::NotFound(format!("header match {position}"));
        match change {
            HeaderMatchChange::Append(row) => {
                HeaderMatchRepo::edit_tx(&mut tx, list, &context, "append", None, |set| {
                    set.push(row);
                    Ok(())
                })
                .await?;
            }
            HeaderMatchChange::Update(position, row) => {
                HeaderMatchRepo::edit_tx(
                    &mut tx,
                    list,
                    &context,
                    "update",
                    Some(position),
                    |set| {
                        let slot = set
                            .get_mut(position)
                            .ok_or_else(|| out_of_range(position))?;
                        *slot = row;
                        Ok(())
                    },
                )
                .await?;
            }
            HeaderMatchChange::Move(position, target) => {
                HeaderMatchRepo::edit_tx(
                    &mut tx,
                    list,
                    &context,
                    "update",
                    Some(position),
                    |set| {
                        if position >= set.len() {
                            return Err(out_of_range(position));
                        }
                        if target >= set.len() {
                            return Err(Error::Validation(format!(
                                "header match position must be below {}",
                                set.len()
                            )));
                        }
                        let row = set.remove(position);
                        set.insert(target, row);
                        Ok(())
                    },
                )
                .await?;
            }
            HeaderMatchChange::Remove(position) => {
                HeaderMatchRepo::edit_tx(
                    &mut tx,
                    list,
                    &context,
                    "remove",
                    Some(position),
                    |set| {
                        if position >= set.len() {
                            return Err(out_of_range(position));
                        }
                        set.remove(position);
                        Ok(())
                    },
                )
                .await?;
            }
        }
        Self::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)
    }

    /// The list's bans (one page) and how many there are, for its owner.
    /// # Errors
    /// Rejects revoked authority, an invalid offset and database failures.
    pub async fn browser_list_bans(
        &self,
        session: &WebSession,
        list: &ListId,
        offset: i64,
    ) -> Result<(Vec<String>, i64)> {
        self.owner_check(session, list).await?;
        let rows = self.bans().list(list, 21, offset).await?;
        let total = self.bans().count(list).await?;
        Ok((rows, total))
    }

    /// Ban a sender on the list.
    /// # Errors
    /// Rejects revoked authority, an invalid value, a duplicate, and
    /// database or audit failures.
    pub async fn browser_list_ban_add(
        &self,
        session: &WebSession,
        list: &ListId,
        value: &str,
    ) -> Result<String> {
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_owner_tx(&mut tx, session, list).await?;
        let value = BanRepo::create_tx(
            &mut tx,
            list,
            value,
            &AuditContext::new(Some(user), None, None),
        )
        .await?;
        Self::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(value)
    }

    /// Lift a ban on the list.
    /// # Errors
    /// Rejects revoked authority, an unknown ban, and database or audit
    /// failures.
    pub async fn browser_list_ban_remove(
        &self,
        session: &WebSession,
        list: &ListId,
        value: &str,
    ) -> Result<()> {
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_owner_tx(&mut tx, session, list).await?;
        BanRepo::delete_tx(
            &mut tx,
            list,
            value,
            &AuditContext::new(Some(user), None, None),
        )
        .await?;
        Self::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)
    }

    /// The site-wide bans (one page) and their count, for a server owner.
    /// # Errors
    /// Rejects anyone but a live server owner, an invalid offset and
    /// database failures.
    pub async fn browser_site_bans(
        &self,
        session: &WebSession,
        offset: i64,
    ) -> Result<(Vec<String>, i64)> {
        self.server_owner_check(session).await?;
        let rows = self.bans().site_list(21, offset).await?;
        let total = self.bans().site_count().await?;
        Ok((rows, total))
    }

    /// Ban a sender on every list.
    /// # Errors
    /// Rejects anyone but a live server owner, an invalid or duplicate
    /// value, and database or audit failures.
    pub async fn browser_site_ban_add(&self, session: &WebSession, value: &str) -> Result<String> {
        let mut tx = self.browser_write_tx().await?;
        let user = Self::server_owner_tx(&mut tx, session).await?;
        let value =
            BanRepo::site_create_tx(&mut tx, value, &AuditContext::new(Some(user), None, None))
                .await?;
        Self::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(value)
    }

    /// Lift a site-wide ban.
    /// # Errors
    /// Rejects anyone but a live server owner, an unknown ban, and database
    /// or audit failures.
    pub async fn browser_site_ban_remove(&self, session: &WebSession, value: &str) -> Result<()> {
        let mut tx = self.browser_write_tx().await?;
        let user = Self::server_owner_tx(&mut tx, session).await?;
        BanRepo::site_delete_tx(&mut tx, value, &AuditContext::new(Some(user), None, None)).await?;
        Self::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)
    }

    /// The list's stored template overrides, for its owner.
    /// # Errors
    /// Rejects revoked authority and database failures.
    pub async fn browser_templates(
        &self,
        session: &WebSession,
        list: &ListId,
    ) -> Result<Vec<Template>> {
        self.owner_check(session, list).await?;
        self.lists().templates(list).await
    }

    /// One template as the editor shows it: the override stored for
    /// `language`, if any, and what the list would use now.
    /// # Errors
    /// Rejects revoked authority, an unknown name and database failures.
    pub async fn browser_template(
        &self,
        session: &WebSession,
        list: &ListId,
        name: &str,
        language: &str,
    ) -> Result<(MailingList, TemplateView)> {
        if !listmngr_mail::templates::is_known_name(name) {
            return Err(Error::NotFound(format!("template {name}")));
        }
        let mut tx = self.browser_write_tx().await?;
        Self::browser_owner_tx(&mut tx, session, list).await?;
        let current = crate::lock_list_for_patch(&mut tx, list).await?;
        let stored: Option<String> = sqlx::query_scalar(
            "SELECT body FROM templates WHERE scope='list' AND scope_id=$1 AND name=$2 AND language=$3 AND body IS NOT NULL",
        )
        .bind(list.as_str())
        .bind(name)
        .bind(language)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?;
        let resolved = crate::templates::resolve_tx(&mut tx, name, &current, language).await?;
        tx.commit().await.map_err(db_error)?;
        Ok((
            current,
            TemplateView {
                stored,
                effective: resolved.body,
                source: resolved.source,
            },
        ))
    }

    /// Store the list's own body for a template in one language.
    /// # Errors
    /// Rejects revoked authority, an unknown name, a bad language, an
    /// oversized body, and database or audit failures.
    pub async fn browser_template_set(
        &self,
        session: &WebSession,
        list: &ListId,
        name: &str,
        language: &str,
        body: &str,
    ) -> Result<()> {
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_owner_tx(&mut tx, session, list).await?;
        TemplateRepo::set_body_tx(
            &mut tx,
            &Scope::List(list.clone()),
            name,
            language,
            body,
            &AuditContext::new(Some(user), None, None),
        )
        .await?;
        Self::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)
    }

    /// Drop the list's own bodies for a template (every language), so the
    /// domain, site or built-in text applies again.
    /// # Errors
    /// Rejects revoked authority, an unknown name, and database or audit
    /// failures.
    pub async fn browser_template_delete(
        &self,
        session: &WebSession,
        list: &ListId,
        name: &str,
    ) -> Result<()> {
        if !listmngr_mail::templates::is_known_name(name) {
            return Err(Error::NotFound(format!("template {name}")));
        }
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_owner_tx(&mut tx, session, list).await?;
        TemplateRepo::delete_tx(
            &mut tx,
            &Scope::List(list.clone()),
            Some(name),
            &AuditContext::new(Some(user), None, None),
        )
        .await?;
        Self::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)
    }

    /// The list's archivers as stored, for its owner.
    /// # Errors
    /// Rejects revoked authority and database failures.
    pub async fn browser_archivers(
        &self,
        session: &WebSession,
        list: &ListId,
    ) -> Result<Vec<(String, bool)>> {
        self.owner_check(session, list).await?;
        self.lists().archivers(list).await
    }

    /// Switch one archiver on or off; the caller has checked the name.
    /// # Errors
    /// Rejects revoked authority and database or audit failures.
    pub async fn browser_archiver_set(
        &self,
        session: &WebSession,
        list: &ListId,
        name: &str,
        enabled: bool,
    ) -> Result<()> {
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_owner_tx(&mut tx, session, list).await?;
        ListRepo::set_archiver_tx(
            &mut tx,
            list,
            name,
            enabled,
            &AuditContext::new(Some(user), None, None),
        )
        .await?;
        Self::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)
    }

    /// Advance the digest volume and restart numbering.
    /// # Errors
    /// Rejects revoked authority, an overflowing volume, and database or
    /// audit failures.
    pub async fn browser_digest_bump(&self, session: &WebSession, list: &ListId) -> Result<()> {
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_owner_tx(&mut tx, session, list).await?;
        crate::digests::DigestRepo::bump_tx(
            &mut tx,
            list,
            &AuditContext::new(Some(user), None, None),
        )
        .await?;
        Self::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)
    }

    /// Publish whatever digest has accumulated, as `mailman digests --send`
    /// does; the owner's authority is checked first, the publication runs
    /// under the digest lease. Returns how many digests were queued.
    /// # Errors
    /// Rejects revoked authority and database failures.
    pub async fn browser_digest_send(
        &self,
        session: &WebSession,
        list: &ListId,
        now_ms: i64,
    ) -> Result<usize> {
        self.owner_check(session, list).await?;
        self.digests()
            .live()
            .flush(list, now_ms, true, crate::digests::render)
            .await
    }

    /// Delete the list and everything it owns, attributed to the owner.
    /// # Errors
    /// Rejects revoked authority and database or audit failures.
    pub async fn browser_delete_list(&self, session: &WebSession, list: &ListId) -> Result<()> {
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_owner_tx(&mut tx, session, list).await?;
        ListRepo::delete_tx(
            &mut tx,
            self,
            list,
            &AuditContext::new(Some(user), None, None),
        )
        .await?;
        Self::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)
    }
}

/// Which stored rule fires first for a header value, and what each says.
#[must_use]
pub fn header_match_outcomes(rows: &[HeaderMatchRow], header: &str, value: &str) -> Vec<bool> {
    rows.iter()
        .map(|row| {
            row.header.trim().eq_ignore_ascii_case(header.trim())
                && listmngr_pipeline::compile_header_pattern(&row.pattern)
                    .is_ok_and(|pattern| pattern.is_match(value))
        })
        .collect()
}
