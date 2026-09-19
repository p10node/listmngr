//! Mail the site sends to a person outside any list: address verification,
//! password reset and whatever else an account needs before it belongs to a
//! list.
//!
//! The message is scoped to the site — `From:` and the Message-ID domain are
//! the configured site owner, templates resolve at the site scope then the
//! built-in, and the out runner signs for the owner's domain when a DKIM key
//! exists for it. Like every other generated notice it leaves with a null
//! reverse path, carries no `List-*` header, and binds no list delivery
//! authority. The queue row and its `workflow_notices` row commit in the
//! caller's transaction, so a producer's business write and its mail are one
//! unit.
use crate::{AuditContext, Database, db_error};
use listmngr_core::{Address, Error, Result};
use listmngr_mail::templates::{Placeholders, expand};
use sqlx::{Any, Transaction};
use uuid::Uuid;

/// One message to one person, described by catalog ids.
#[derive(Debug, Clone, Copy)]
pub struct SiteNotice<'a> {
    /// Recipient mailbox.
    pub to: &'a str,
    /// Language the recipient reads; a shipped catalog is negotiated from it.
    pub language: &'a str,
    /// Subject message id in `listmngr_i18n`.
    pub subject: &'a str,
    /// Subject arguments.
    pub subject_args: &'a [(&'a str, &'a str)],
    /// Template name, a `site:*` name the catalog knows.
    pub template: &'a str,
}

/// Producer handle; see [`Database::site_notices`].
#[derive(Debug, Clone, Copy)]
pub struct SiteNoticeRepo<'a> {
    pub(crate) db: &'a Database,
}

impl SiteNoticeRepo<'_> {
    /// The placeholders every site notice can use: `$site_name`, `$domain`
    /// (the site owner's mail host) and `$base_url`.
    #[must_use]
    pub fn placeholders(&self) -> Placeholders {
        let domain = self
            .db
            .site_owner()
            .rsplit_once('@')
            .map_or("", |(_, host)| host)
            .to_owned();
        Placeholders::new()
            .set("site_name", self.db.site_name().to_owned())
            .set("domain", domain)
            .set(
                "base_url",
                self.db.base_url().unwrap_or_default().to_owned(),
            )
    }

    /// Enqueue in a transaction of its own, with a `site.notice` audit event.
    /// # Errors
    /// An invalid recipient or site owner mailbox, an unknown template, or a
    /// database failure; nothing is enqueued on error.
    pub async fn enqueue(
        &self,
        notice: &SiteNotice<'_>,
        placeholders: Placeholders,
        now_ms: i64,
    ) -> Result<()> {
        let mut tx = self.db.write_tx().await?;
        self.enqueue_tx(&mut tx, notice, placeholders, now_ms)
            .await?;
        Database::record_tx_with_context(
            &mut tx,
            &AuditContext::system(),
            "site.notice",
            "site",
            notice.template,
            serde_json::json!({"template": notice.template}),
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }

    /// Enqueue inside the caller's transaction, so the notice commits with the
    /// business write that needs it. The caller records its own audit event.
    /// # Errors
    /// As [`Self::enqueue`].
    pub async fn enqueue_tx(
        &self,
        tx: &mut Transaction<'_, Any>,
        notice: &SiteNotice<'_>,
        placeholders: Placeholders,
        now_ms: i64,
    ) -> Result<()> {
        let recipient = Address::new(notice.to, String::new())?;
        crate::workflows::notice_mailbox(&recipient.email)?;
        let owner = Address::new(self.db.site_owner(), String::new())
            .map_err(|_| Error::Validation("site.site_owner is not a mailbox".into()))?;
        crate::workflows::notice_mailbox(&owner.email)
            .map_err(|_| Error::Validation("site.site_owner is not a notice mailbox".into()))?;
        let host = owner
            .email
            .rsplit_once('@')
            .map(|(_, host)| host.to_owned())
            .ok_or_else(|| Error::Validation("site.site_owner has no domain".into()))?;
        let language = listmngr_i18n::negotiate(notice.language);
        let subject = listmngr_i18n::message(language, notice.subject, notice.subject_args);
        let resolved = crate::templates::resolve_site_tx(tx, notice.template, language).await?;
        let values = placeholders.with_defaults(self.placeholders());
        let body = expand(&resolved.body, &values);
        let id = Uuid::now_v7().to_string();
        let date = chrono::DateTime::from_timestamp_millis(now_ms)
            .unwrap_or_else(chrono::Utc::now)
            .to_rfc2822();
        let raw = crate::notices::serialize(
            &crate::notices::Envelope {
                from: &owner.email,
                to: &recipient.original_email,
                reply_to: None,
                subject: &subject,
                message_id_local: &id,
                mail_host: &host,
                date: &date,
                auto_submitted: "auto-generated",
            },
            &body,
        )?;
        // No `list_id`: the out runner sends the bytes as they are on the
        // strength of the notice row, and signs for the site domain.
        let context = serde_json::json!({"site": true, "site_mail_host": host}).to_string();
        crate::workflows::enqueue_raw_notice(
            tx,
            &context,
            &recipient.original_email,
            &id,
            raw,
            now_ms,
            None,
        )
        .await
    }
}
