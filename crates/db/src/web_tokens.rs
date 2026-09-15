//! The reader's own API tokens.
//!
//! Scope follows authority, because the API authorizes on the token alone: a
//! server owner may mint any scope, unbound; anyone else may mint only
//! list-level scopes, bound to a list they own, so a token can never do more
//! than the person who minted it. The secret is returned once and never
//! stored.
use crate::web_sessions::WebSession;
use crate::{AuditContext, Database, IssuedToken, NewToken, db_error};
use listmngr_core::{DomainId, Error, ListId, Result};
use sqlx::Row;

/// Scopes a list owner may put on a token bound to their list.
pub const LIST_SCOPES: &[&str] = &[
    "lists:read",
    "lists:write",
    "members:read",
    "members:write",
    "moderation",
];

/// Every scope, for a server owner.
pub const ALL_SCOPES: &[&str] = &[
    "system:read",
    "lists:read",
    "lists:write",
    "members:read",
    "members:write",
    "moderation",
    "users:write",
    "archive:write",
    "admin",
];

/// Longest token name accepted, in characters.
pub const NAME_MAX: usize = 64;
/// Longest lifetime a form may ask for, in days.
pub const EXPIRY_DAYS_MAX: u32 = 365;

/// One of the reader's tokens; never the secret.
#[derive(Debug, Clone)]
pub struct OwnToken {
    /// Token id, safe to put in a form.
    pub id: String,
    /// Name the reader gave it.
    pub name: String,
    /// Space-separated scopes.
    pub scopes: String,
    /// The list it is bound to, if any.
    pub list_id: Option<String>,
    /// RFC 3339 creation time.
    pub created_at: String,
    /// RFC 3339 expiry, if any.
    pub expires_at: Option<String>,
    /// RFC 3339 revocation time, if revoked.
    pub revoked_at: Option<String>,
    /// RFC 3339 last use, if ever used.
    pub last_used_at: Option<String>,
}

/// What the reader may mint.
#[derive(Debug, Clone)]
pub struct TokenAuthority {
    /// A server owner with a verified address: any scope, unbound.
    pub server_owner: bool,
    /// Lists the reader owns, as `(list id, display name)`.
    pub lists: Vec<(String, String)>,
}

impl TokenAuthority {
    /// The scopes this reader may choose from.
    #[must_use]
    pub const fn scopes(&self) -> &'static [&'static str] {
        if self.server_owner {
            ALL_SCOPES
        } else {
            LIST_SCOPES
        }
    }
}

/// What the form submits.
#[derive(Debug, Clone)]
pub struct TokenRequest {
    /// Name, 1 to [`NAME_MAX`] characters.
    pub name: String,
    /// Requested scopes.
    pub scopes: Vec<String>,
    /// List to bind to; required unless the reader is a server owner.
    pub list_id: Option<ListId>,
    /// Lifetime in days, 1 to [`EXPIRY_DAYS_MAX`]; `None` never expires.
    pub expires_days: Option<u32>,
}

const OWNED: &str = "SELECT l.list_id,substr(l.display_name,1,512) FROM mailing_lists l WHERE (EXISTS (SELECT 1 FROM users u JOIN addresses a ON a.user_id=u.id WHERE u.id=$1 AND u.is_server_owner=1 AND a.verified_on IS NOT NULL) OR EXISTS (SELECT 1 FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id=l.list_id AND m.role='owner' AND a.user_id=$1 AND a.verified_on IS NOT NULL AND (m.subscription_mode='as_address' OR m.user_id=$1)))";

async fn authority(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    user: listmngr_core::UserId,
) -> Result<TokenAuthority> {
    let server_owner: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users u JOIN addresses a ON a.user_id=u.id WHERE u.id=$1 AND u.is_server_owner=1 AND a.verified_on IS NOT NULL")
        .bind(user.to_string()).fetch_one(&mut **tx).await.map_err(db_error)?;
    let lists: Vec<(String, String)> =
        sqlx::query_as(&format!("{OWNED} ORDER BY l.list_id LIMIT 100"))
            .bind(user.to_string())
            .fetch_all(&mut **tx)
            .await
            .map_err(db_error)?;
    Ok(TokenAuthority {
        server_owner: server_owner > 0,
        lists,
    })
}

impl Database {
    /// The reader's tokens, newest first, and what they may mint.
    /// # Errors
    /// A stale session or failed CSRF binding; database errors.
    pub async fn browser_tokens(
        &self,
        session: &WebSession,
    ) -> Result<(Vec<OwnToken>, TokenAuthority)> {
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_user_tx(&mut tx, session).await?;
        let rows = sqlx::query(
            "SELECT id,name,scopes,list_id,created_at,expires_at,revoked_at,last_used_at FROM api_tokens WHERE user_id=$1 ORDER BY created_at DESC,id DESC LIMIT 200",
        )
        .bind(user.to_string())
        .fetch_all(&mut *tx)
        .await
        .map_err(db_error)?;
        let tokens = rows
            .into_iter()
            .map(|row| {
                Ok(OwnToken {
                    id: row.try_get("id").map_err(db_error)?,
                    name: row.try_get("name").map_err(db_error)?,
                    scopes: row.try_get("scopes").map_err(db_error)?,
                    list_id: row.try_get("list_id").map_err(db_error)?,
                    created_at: row.try_get("created_at").map_err(db_error)?,
                    expires_at: row.try_get("expires_at").map_err(db_error)?,
                    revoked_at: row.try_get("revoked_at").map_err(db_error)?,
                    last_used_at: row.try_get("last_used_at").map_err(db_error)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let authority = authority(&mut tx, user).await?;
        tx.commit().await.map_err(db_error)?;
        Ok((tokens, authority))
    }

    /// Mint a token within the reader's authority. The secret is in the
    /// result and nowhere else.
    /// # Errors
    /// A bad name, lifetime or scope set (validation); a list the reader does
    /// not own (forbidden); a stale session or failed CSRF binding; or audit
    /// failure.
    pub async fn browser_create_token(
        &self,
        session: &WebSession,
        request: &TokenRequest,
    ) -> Result<IssuedToken> {
        let name = request.name.trim();
        if name.is_empty() || name.chars().count() > NAME_MAX || name.chars().any(char::is_control)
        {
            return Err(Error::Validation("token name".into()));
        }
        if request.scopes.is_empty() {
            return Err(Error::Validation("choose at least one scope".into()));
        }
        if request
            .expires_days
            .is_some_and(|days| !(1..=EXPIRY_DAYS_MAX).contains(&days))
        {
            return Err(Error::Validation("token lifetime".into()));
        }
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_user_tx(&mut tx, session).await?;
        let authority = authority(&mut tx, user).await?;
        let allowed = authority.scopes();
        if request
            .scopes
            .iter()
            .any(|scope| !allowed.contains(&scope.as_str()))
        {
            return Err(Error::Validation("scope outside your authority".into()));
        }
        let domain_id: Option<DomainId> = match &request.list_id {
            Some(list) => {
                if !authority
                    .lists
                    .iter()
                    .any(|(owned, _)| owned == list.as_str())
                {
                    return Err(Error::Forbidden("list owner authority".into()));
                }
                let domain: String =
                    sqlx::query_scalar("SELECT id FROM domains WHERE mail_host=$1")
                        .bind(list.mail_host())
                        .fetch_one(&mut *tx)
                        .await
                        .map_err(db_error)?;
                Some(domain.parse().map_err(db_error)?)
            }
            None if authority.server_owner => None,
            None => {
                return Err(Error::Validation(
                    "choose a list to bind the token to".into(),
                ));
            }
        };
        if request.scopes.iter().any(|scope| scope == "admin") && domain_id.is_some() {
            return Err(Error::Validation("admin tokens must be unbound".into()));
        }
        let scopes: Vec<&str> = request.scopes.iter().map(String::as_str).collect();
        let expires = request
            .expires_days
            .map(|days| chrono::Utc::now() + chrono::Duration::days(i64::from(days)));
        let issued = crate::insert_token_tx(
            &mut tx,
            &NewToken {
                user,
                name,
                scopes: &scopes,
                list_id: request.list_id.as_ref(),
                domain_id,
                expires,
            },
            &AuditContext::new(Some(user), None, None),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(issued)
    }

    /// Revoke one of the reader's own live tokens.
    /// # Errors
    /// A token that is not the reader's or is already revoked (not found); a
    /// stale session or failed CSRF binding; or audit failure.
    pub async fn browser_revoke_token(&self, session: &WebSession, id: &str) -> Result<()> {
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_user_tx(&mut tx, session).await?;
        let revoked = sqlx::query(
            "UPDATE api_tokens SET revoked_at=$1 WHERE id=$2 AND user_id=$3 AND revoked_at IS NULL",
        )
        .bind(crate::now())
        .bind(id)
        .bind(user.to_string())
        .execute(&mut *tx)
        .await
        .map_err(db_error)?
        .rows_affected();
        if revoked != 1 {
            return Err(Error::NotFound("token".into()));
        }
        Self::record_tx_with_context(
            &mut tx,
            &AuditContext::new(Some(user), None, None),
            "token.revoke",
            "token",
            id,
            serde_json::json!({"by": "owner"}),
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }
}
