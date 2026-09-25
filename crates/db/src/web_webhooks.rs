//! Webhooks from the browser: a list's, for its owner, and every one for
//! a server owner. Each write is the same repository write the API makes,
//! attributed to the signed-in user.
use crate::web_sessions::WebSession;
use crate::webhooks::{Delivery, NewWebhook, Webhook, WebhookPatch};
use crate::{AuditContext, Database, db_error};
use listmngr_core::{Error, ListId, Result, UserId, WebhookId};

/// Whose webhooks the browser is looking at.
#[derive(Debug, Clone, Copy)]
pub enum WebhookScope<'a> {
    /// One list's, for its owner.
    List(&'a ListId),
    /// Every one, for a server owner.
    Site,
}

impl Database {
    /// The signed-in user, once their authority over the scope is checked
    /// and released.
    async fn webhook_authority(
        &self,
        session: &WebSession,
        scope: WebhookScope<'_>,
    ) -> Result<UserId> {
        let mut tx = self.browser_write_tx().await?;
        let user = match scope {
            WebhookScope::List(list) => Self::browser_owner_tx(&mut tx, session, list).await?,
            WebhookScope::Site => Self::server_owner_tx(&mut tx, session).await?,
        };
        tx.commit().await.map_err(db_error)?;
        Ok(user)
    }

    /// A webhook the scope may see; another list's is, to a list owner,
    /// not there.
    async fn webhook_in_scope(&self, id: WebhookId, scope: WebhookScope<'_>) -> Result<Webhook> {
        let webhook = self.webhooks().get(id).await?;
        if let WebhookScope::List(list) = scope
            && webhook.list_id.as_ref() != Some(list)
        {
            return Err(Error::NotFound(format!("webhook {id}")));
        }
        Ok(webhook)
    }

    /// The scope's webhooks, oldest first.
    /// # Errors
    /// Rejects revoked authority and database failures.
    pub async fn browser_webhooks(
        &self,
        session: &WebSession,
        scope: WebhookScope<'_>,
    ) -> Result<Vec<Webhook>> {
        self.webhook_authority(session, scope).await?;
        match scope {
            WebhookScope::List(list) => self.webhooks().list(Some(list)).await,
            WebhookScope::Site => self.webhooks().list(None).await,
        }
    }

    /// Create a webhook in the scope — bound to the list, or site-wide —
    /// and return it with its secret, shown this once.
    /// # Errors
    /// Rejects revoked authority, an invalid URL or events, and database
    /// or audit failures.
    pub async fn browser_webhook_create(
        &self,
        session: &WebSession,
        scope: WebhookScope<'_>,
        url: &str,
        events: Vec<String>,
        description: &str,
    ) -> Result<(Webhook, String)> {
        let user = self.webhook_authority(session, scope).await?;
        let list_id = match scope {
            WebhookScope::List(list) => Some(list.clone()),
            WebhookScope::Site => None,
        };
        self.webhooks()
            .create_with_context(
                NewWebhook {
                    url: url.to_owned(),
                    events,
                    list_id,
                    description: description.to_owned(),
                },
                &AuditContext::new(Some(user), None, None),
            )
            .await
    }

    /// Switch a webhook on or off.
    /// # Errors
    /// Rejects revoked authority, a webhook outside the scope, and
    /// database or audit failures.
    pub async fn browser_webhook_set_enabled(
        &self,
        session: &WebSession,
        scope: WebhookScope<'_>,
        id: WebhookId,
        enabled: bool,
    ) -> Result<Webhook> {
        let user = self.webhook_authority(session, scope).await?;
        self.webhook_in_scope(id, scope).await?;
        self.webhooks()
            .update_with_context(
                id,
                WebhookPatch {
                    enabled: Some(enabled),
                    ..WebhookPatch::default()
                },
                &AuditContext::new(Some(user), None, None),
            )
            .await
    }

    /// Delete a webhook and the deliveries it was owed.
    /// # Errors
    /// As [`Self::browser_webhook_set_enabled`].
    pub async fn browser_webhook_remove(
        &self,
        session: &WebSession,
        scope: WebhookScope<'_>,
        id: WebhookId,
    ) -> Result<()> {
        let user = self.webhook_authority(session, scope).await?;
        self.webhook_in_scope(id, scope).await?;
        self.webhooks()
            .delete_with_context(id, &AuditContext::new(Some(user), None, None))
            .await
    }

    /// Give a webhook a new secret and return it, shown this once.
    /// # Errors
    /// As [`Self::browser_webhook_set_enabled`].
    pub async fn browser_webhook_rotate(
        &self,
        session: &WebSession,
        scope: WebhookScope<'_>,
        id: WebhookId,
    ) -> Result<(Webhook, String)> {
        let user = self.webhook_authority(session, scope).await?;
        self.webhook_in_scope(id, scope).await?;
        let secret = self
            .webhooks()
            .rotate_with_context(id, &AuditContext::new(Some(user), None, None))
            .await?;
        Ok((self.webhooks().get(id).await?, secret))
    }

    /// Queue a `ping` delivery.
    /// # Errors
    /// As [`Self::browser_webhook_set_enabled`].
    pub async fn browser_webhook_ping(
        &self,
        session: &WebSession,
        scope: WebhookScope<'_>,
        id: WebhookId,
    ) -> Result<Delivery> {
        let user = self.webhook_authority(session, scope).await?;
        self.webhook_in_scope(id, scope).await?;
        self.webhooks()
            .ping_with_context(id, &AuditContext::new(Some(user), None, None))
            .await
    }

    /// A webhook and what it was owed, newest first.
    /// # Errors
    /// As [`Self::browser_webhook_set_enabled`].
    pub async fn browser_webhook_deliveries(
        &self,
        session: &WebSession,
        scope: WebhookScope<'_>,
        id: WebhookId,
    ) -> Result<(Webhook, Vec<Delivery>)> {
        self.webhook_authority(session, scope).await?;
        let webhook = self.webhook_in_scope(id, scope).await?;
        let deliveries = self.webhooks().deliveries(id, 100).await?;
        Ok((webhook, deliveries))
    }
}
