//! The server owner's domain administration.
//!
//! Domains, their owners and their template overrides. Every read and write runs under live browser
//! session authority; a write that needs a repository's own transaction
//! (create, delete, owner changes) checks authority first and lets the
//! repository commit the business write with its audit event.
use crate::{
    AuditContext, Database, db_error, templates::Scope, templates::TemplateRepo,
    web_list_settings::TemplateView, web_sessions::WebSession,
};
use listmngr_core::{Domain, Error, Result, UserId};
use sqlx::Row as _;

/// One domain as the index lists it.
#[derive(Debug)]
pub struct DomainSummary {
    pub domain: Domain,
    /// Display names and first addresses of the owners.
    pub owners: Vec<DomainOwner>,
    /// How many lists the domain carries.
    pub lists: i64,
}

/// One owner of a domain.
#[derive(Debug, Clone)]
pub struct DomainOwner {
    pub user: UserId,
    pub display_name: String,
    /// The owner's first address, if any.
    pub email: String,
}

impl Database {
    async fn site_owner_check(&self, session: &WebSession) -> Result<UserId> {
        let mut tx = self.browser_write_tx().await?;
        let user = Self::server_owner_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(user)
    }

    async fn owners_of(&self, domain: &Domain) -> Result<Vec<DomainOwner>> {
        let rows = sqlx::query("SELECT u.id, substr(u.display_name,1,256) AS display_name, (SELECT a.email FROM addresses a WHERE a.user_id=u.id ORDER BY a.verified_on IS NULL, a.registered_on, a.email LIMIT 1) AS email FROM users u JOIN domain_owners o ON o.user_id=u.id WHERE o.domain_id=$1 ORDER BY u.created_at")
            .bind(domain.id.to_string())
            .fetch_all(self.pool())
            .await
            .map_err(db_error)?;
        rows.iter()
            .map(|row| {
                Ok(DomainOwner {
                    user: row
                        .try_get::<String, _>("id")
                        .map_err(db_error)?
                        .parse()
                        .map_err(db_error)?,
                    display_name: row.try_get("display_name").map_err(db_error)?,
                    email: row
                        .try_get::<Option<String>, _>("email")
                        .map_err(db_error)?
                        .unwrap_or_default(),
                })
            })
            .collect()
    }

    async fn summary(&self, domain: Domain) -> Result<DomainSummary> {
        let lists: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM mailing_lists WHERE mail_host=$1")
                .bind(&domain.mail_host)
                .fetch_one(self.pool())
                .await
                .map_err(db_error)?;
        Ok(DomainSummary {
            owners: self.owners_of(&domain).await?,
            domain,
            lists,
        })
    }

    /// Every domain with its owners and list count, for a server owner.
    /// # Errors
    /// Rejects a reader who is not a verified server owner; database failures.
    pub async fn browser_domains(&self, session: &WebSession) -> Result<Vec<DomainSummary>> {
        self.site_owner_check(session).await?;
        let mut out = Vec::new();
        for domain in self.domains().list().await? {
            out.push(self.summary(domain).await?);
        }
        Ok(out)
    }

    /// One domain with its owners and list count, for a server owner.
    /// # Errors
    /// Rejects a reader who is not a verified server owner; `NotFound` for an
    /// unknown host; database failures.
    pub async fn browser_domain(&self, session: &WebSession, host: &str) -> Result<DomainSummary> {
        self.site_owner_check(session).await?;
        let domain = self.domains().get(host).await?;
        self.summary(domain).await
    }

    /// Creates a domain, attributed to the server owner.
    /// # Errors
    /// Authority, validation (`Validation`), a taken host (`Conflict`), and
    /// database or audit failures.
    pub async fn browser_domain_create(
        &self,
        session: &WebSession,
        host: &str,
        description: &str,
        alias: Option<&str>,
    ) -> Result<Domain> {
        let user = self.site_owner_check(session).await?;
        self.domains()
            .create_with_context(
                host,
                description,
                alias,
                &AuditContext::new(Some(user), None, None),
            )
            .await
    }

    /// Deletes an empty domain, attributed to the server owner.
    /// # Errors
    /// Authority, `Conflict` while lists remain, `NotFound`, and database or
    /// audit failures.
    pub async fn browser_domain_delete(&self, session: &WebSession, host: &str) -> Result<()> {
        let user = self.site_owner_check(session).await?;
        self.domains()
            .delete_with_context(host, &AuditContext::new(Some(user), None, None))
            .await
    }

    /// Seats the account behind `email` as an owner of the domain.
    /// # Errors
    /// Authority, `NotFound` for an unknown domain or an address without an
    /// account, `Conflict` when already an owner, database or audit failures.
    pub async fn browser_domain_owner_add(
        &self,
        session: &WebSession,
        host: &str,
        email: &str,
    ) -> Result<UserId> {
        let actor = self.site_owner_check(session).await?;
        let owner = self.users().get_by_email(email.trim()).await?;
        self.domains()
            .add_owner_with_context(host, owner.id, &AuditContext::new(Some(actor), None, None))
            .await?;
        Ok(owner.id)
    }

    /// Removes one owner of the domain.
    /// # Errors
    /// Authority, `NotFound` for an unknown domain or a user who is not an
    /// owner, database or audit failures.
    pub async fn browser_domain_owner_remove(
        &self,
        session: &WebSession,
        host: &str,
        user: UserId,
    ) -> Result<()> {
        let actor = self.site_owner_check(session).await?;
        self.domains()
            .remove_owner_with_context(host, user, &AuditContext::new(Some(actor), None, None))
            .await
    }

    /// Whether the reader is a verified server owner (no error when not).
    /// # Errors
    /// A stale session or database failures.
    pub async fn browser_is_server_owner(&self, session: &WebSession) -> Result<bool> {
        let mut tx = self.browser_write_tx().await?;
        let allowed = match Self::server_owner_tx(&mut tx, session).await {
            Ok(_) => true,
            Err(Error::Forbidden(_)) => false,
            Err(error) => return Err(error),
        };
        tx.commit().await.map_err(db_error)?;
        Ok(allowed)
    }

    /// The template names the domain stores its own body for, with the
    /// languages, for a server owner.
    /// # Errors
    /// Authority, `NotFound` for an unknown host, database failures.
    pub async fn browser_domain_templates(
        &self,
        session: &WebSession,
        host: &str,
    ) -> Result<Vec<(String, String)>> {
        self.site_owner_check(session).await?;
        self.browser_domain_templates_unchecked(host).await
    }

    /// The stored domain template names and languages; the caller has
    /// checked authority on this request already.
    /// # Errors
    /// `NotFound` for an unknown host, database failures.
    pub async fn browser_domain_templates_unchecked(
        &self,
        host: &str,
    ) -> Result<Vec<(String, String)>> {
        self.domains().get(host).await?;
        let rows = sqlx::query("SELECT name, language FROM templates WHERE scope='domain' AND scope_id=$1 AND body IS NOT NULL ORDER BY name, language")
            .bind(host.to_ascii_lowercase())
            .fetch_all(self.pool())
            .await
            .map_err(db_error)?;
        rows.iter()
            .map(|row| {
                Ok((
                    row.try_get("name").map_err(db_error)?,
                    row.try_get("language").map_err(db_error)?,
                ))
            })
            .collect()
    }

    /// One domain template as the editor shows it: the override stored for
    /// `language`, if any, and what a list on the domain without its own
    /// override would use now.
    /// # Errors
    /// Authority, an unknown name or host (`NotFound`), database failures.
    pub async fn browser_domain_template(
        &self,
        session: &WebSession,
        host: &str,
        name: &str,
        language: &str,
    ) -> Result<TemplateView> {
        if !listmngr_mail::templates::is_known_name(name) {
            return Err(Error::NotFound(format!("template {name}")));
        }
        let host = self.domains().get(host).await?.mail_host;
        let mut tx = self.browser_write_tx().await?;
        Self::server_owner_tx(&mut tx, session).await?;
        let stored: Option<String> = sqlx::query_scalar(
            "SELECT body FROM templates WHERE scope='domain' AND scope_id=$1 AND name=$2 AND language=$3 AND body IS NOT NULL",
        )
        .bind(&host)
        .bind(name)
        .bind(language)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?;
        let resolved = crate::templates::resolve_domain_tx(&mut tx, name, &host, language).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(TemplateView {
            stored,
            effective: resolved.body,
            source: resolved.source,
        })
    }

    /// Store the domain's own body for a template in one language.
    /// # Errors
    /// Authority, an unknown name or host, a bad language, an oversized
    /// body, database or audit failures.
    pub async fn browser_domain_template_set(
        &self,
        session: &WebSession,
        host: &str,
        name: &str,
        language: &str,
        body: &str,
    ) -> Result<()> {
        let host = self.domains().get(host).await?.mail_host;
        let mut tx = self.browser_write_tx().await?;
        let user = Self::server_owner_tx(&mut tx, session).await?;
        TemplateRepo::set_body_tx(
            &mut tx,
            &Scope::Domain(host),
            name,
            language,
            body,
            &AuditContext::new(Some(user), None, None),
        )
        .await?;
        Self::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)
    }

    /// Drop the domain's own bodies for a template (every language).
    /// # Errors
    /// Authority, an unknown name or host, database or audit failures.
    pub async fn browser_domain_template_delete(
        &self,
        session: &WebSession,
        host: &str,
        name: &str,
    ) -> Result<()> {
        if !listmngr_mail::templates::is_known_name(name) {
            return Err(Error::NotFound(format!("template {name}")));
        }
        let host = self.domains().get(host).await?.mail_host;
        let mut tx = self.browser_write_tx().await?;
        let user = Self::server_owner_tx(&mut tx, session).await?;
        TemplateRepo::delete_tx(
            &mut tx,
            &Scope::Domain(host),
            Some(name),
            &AuditContext::new(Some(user), None, None),
        )
        .await?;
        Self::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)
    }
}
