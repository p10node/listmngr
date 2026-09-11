//! Notice templates: management of template URIs and bodies per scope, and
//! resolution list → domain → site → built-in with language fallback.
//!
//! Resolution never blocks a business write: a stored template that cannot be
//! loaded (missing file, unsupported `https://` source) is logged without its
//! contents and the next candidate — ultimately the built-in English body —
//! is used instead.

use listmngr_core::{Error, ListId, MailingList, Result};
use listmngr_mail::templates::{MAX_BODY_BYTES, is_known_name, load, parse_uri};
use serde::{Deserialize, Serialize};
use sqlx::{Any, Row, Transaction};
use uuid::Uuid;

use crate::{AuditContext, Database, db_error};

/// Where a template applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    Site,
    Domain(String),
    List(ListId),
}

impl Scope {
    /// `(scope, scope_id)` as stored. The site scope stores an empty string,
    /// not NULL: SQL treats NULLs as distinct in UNIQUE constraints, which
    /// would let `ON CONFLICT` upserts duplicate site rows.
    fn column_values(&self) -> (&'static str, String) {
        match self {
            Self::Site => ("site", String::new()),
            Self::Domain(host) => ("domain", host.clone()),
            Self::List(id) => ("list", id.to_string()),
        }
    }

    fn audit_target(&self) -> (&'static str, String) {
        match self {
            Self::Site => ("site", "site".into()),
            Self::Domain(host) => ("domain", host.clone()),
            Self::List(id) => ("list", id.to_string()),
        }
    }
}

/// One managed template URI, as the Mailman `/uris` resource lists it. The
/// password is stored for the (future) fetcher and never projected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TemplateUri {
    pub name: String,
    pub uri: String,
    #[serde(default)]
    pub username: Option<String>,
}

/// A resolved template body and where it came from (`list:en`, `domain:vi`,
/// `site:en`, or `builtin`), for diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub body: String,
    pub source: String,
}

const DEFAULT_LANGUAGE: &str = "en";

fn validate_name(name: &str) -> Result<()> {
    if is_known_name(name) {
        Ok(())
    } else {
        Err(Error::Validation(format!("unknown template name: {name}")))
    }
}

fn validate_language(language: &str) -> Result<()> {
    if language.is_empty()
        || language.len() > 16
        || !language
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(Error::Validation("invalid template language".into()));
    }
    Ok(())
}

fn normalize_body(body: &str) -> Result<String> {
    if body.len() > MAX_BODY_BYTES {
        return Err(Error::Validation(
            "template body exceeds 65536 bytes".into(),
        ));
    }
    Ok(body.replace("\r\n", "\n").replace('\r', "\n"))
}

#[derive(Debug, Clone, Copy)]
pub struct TemplateRepo<'a> {
    pub(crate) db: &'a Database,
}

impl TemplateRepo<'_> {
    async fn ensure_scope(&self, scope: &Scope) -> Result<()> {
        match scope {
            Scope::Site => Ok(()),
            Scope::Domain(host) => self.db.domains().get(host).await.map(|_| ()),
            Scope::List(id) => self.db.lists().get(id).await.map(|_| ()),
        }
    }

    /// Managed URIs for a scope, by name.
    /// # Errors
    /// Returns a database error or not-found for the scope.
    pub async fn list_uris(&self, scope: &Scope) -> Result<Vec<TemplateUri>> {
        self.ensure_scope(scope).await?;
        let (kind, id) = scope.column_values();
        let rows = sqlx::query(
            "SELECT name, uri, username FROM templates WHERE scope=$1 AND scope_id=$2 AND uri IS NOT NULL ORDER BY name",
        )
        .bind(kind)
        .bind(id)
        .fetch_all(self.db.pool())
        .await
        .map_err(db_error)?;
        rows.iter()
            .map(|row| {
                Ok(TemplateUri {
                    name: row.try_get("name").map_err(db_error)?,
                    uri: row.try_get("uri").map_err(db_error)?,
                    username: row.try_get("username").map_err(db_error)?,
                })
            })
            .collect()
    }

    /// Point a template at a URI (`mailman:///`, `file:///` or `https://`).
    /// The URI row uses the default language; a body row for the same name and
    /// language is replaced.
    /// # Errors
    /// Returns validation errors for the name or URI, not-found for the
    /// scope, or a database/audit failure.
    pub async fn set_uri(
        &self,
        scope: &Scope,
        name: &str,
        uri: &str,
        username: Option<&str>,
        password: Option<&str>,
    ) -> Result<()> {
        self.set_uri_with_context(
            scope,
            name,
            uri,
            username,
            password,
            &AuditContext::system(),
        )
        .await
    }

    /// See [`Self::set_uri`].
    /// # Errors
    /// Returns validation errors for the name or URI, not-found for the
    /// scope, or a database/audit failure.
    pub async fn set_uri_with_context(
        &self,
        scope: &Scope,
        name: &str,
        uri: &str,
        username: Option<&str>,
        password: Option<&str>,
        context: &AuditContext,
    ) -> Result<()> {
        validate_name(name)?;
        parse_uri(uri).map_err(|error| Error::Validation(error.to_string()))?;
        if username.is_some_and(|u| u.is_empty() || u.len() > 254)
            || password.is_some_and(|p| p.len() > 1024)
        {
            return Err(Error::Validation("invalid template credentials".into()));
        }
        self.ensure_scope(scope).await?;
        let (kind, id) = scope.column_values();
        let mut tx = self.db.pool().begin().await.map_err(db_error)?;
        sqlx::query("INSERT INTO templates(id,name,scope,scope_id,language,uri,body,username,password) VALUES($1,$2,$3,$4,$5,$6,NULL,$7,$8) ON CONFLICT(name,scope,scope_id,language) DO UPDATE SET uri=excluded.uri, body=NULL, username=excluded.username, password=excluded.password")
            .bind(Uuid::now_v7().to_string()).bind(name).bind(kind).bind(&id).bind(DEFAULT_LANGUAGE).bind(uri).bind(username).bind(password)
            .execute(&mut *tx).await.map_err(db_error)?;
        let (target_type, target_id) = scope.audit_target();
        Database::record_tx_with_context(
            &mut tx,
            context,
            "template.set",
            target_type,
            &target_id,
            // The URI may embed nothing secret; credentials never enter audit.
            serde_json::json!({"name": name, "uri": uri, "has_credentials": password.is_some()}),
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }

    /// Store an inline body for a name and language (listmngr extension: no
    /// file or fetch needed). CRLF is normalized to LF.
    /// # Errors
    /// Returns validation errors, not-found for the scope, or a
    /// database/audit failure.
    pub async fn set_body(
        &self,
        scope: &Scope,
        name: &str,
        language: &str,
        body: &str,
    ) -> Result<()> {
        self.set_body_with_context(scope, name, language, body, &AuditContext::system())
            .await
    }

    /// See [`Self::set_body`].
    /// # Errors
    /// Returns validation errors, not-found for the scope, or a
    /// database/audit failure.
    pub async fn set_body_with_context(
        &self,
        scope: &Scope,
        name: &str,
        language: &str,
        body: &str,
        context: &AuditContext,
    ) -> Result<()> {
        validate_name(name)?;
        validate_language(language)?;
        let body = normalize_body(body)?;
        self.ensure_scope(scope).await?;
        let (kind, id) = scope.column_values();
        let mut tx = self.db.pool().begin().await.map_err(db_error)?;
        sqlx::query("INSERT INTO templates(id,name,scope,scope_id,language,uri,body,username,password) VALUES($1,$2,$3,$4,$5,NULL,$6,NULL,NULL) ON CONFLICT(name,scope,scope_id,language) DO UPDATE SET uri=NULL, body=excluded.body, username=NULL, password=NULL")
            .bind(Uuid::now_v7().to_string()).bind(name).bind(kind).bind(&id).bind(language).bind(&body)
            .execute(&mut *tx).await.map_err(db_error)?;
        let (target_type, target_id) = scope.audit_target();
        Database::record_tx_with_context(
            &mut tx,
            context,
            "template.set",
            target_type,
            &target_id,
            serde_json::json!({"name": name, "language": language, "bytes": body.len()}),
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }

    /// Delete one template (every language) or, with `None`, every template
    /// of the scope.
    /// # Errors
    /// Returns not-found for the scope or a database/audit failure.
    pub async fn delete(&self, scope: &Scope, name: Option<&str>) -> Result<()> {
        self.delete_with_context(scope, name, &AuditContext::system())
            .await
    }

    /// See [`Self::delete`].
    /// # Errors
    /// Returns not-found for the scope or a database/audit failure.
    pub async fn delete_with_context(
        &self,
        scope: &Scope,
        name: Option<&str>,
        context: &AuditContext,
    ) -> Result<()> {
        self.ensure_scope(scope).await?;
        let (kind, id) = scope.column_values();
        let mut tx = self.db.pool().begin().await.map_err(db_error)?;
        let deleted = match name {
            Some(name) => {
                sqlx::query("DELETE FROM templates WHERE scope=$1 AND scope_id=$2 AND name=$3")
                    .bind(kind)
                    .bind(&id)
                    .bind(name)
                    .execute(&mut *tx)
                    .await
            }
            None => {
                sqlx::query("DELETE FROM templates WHERE scope=$1 AND scope_id=$2")
                    .bind(kind)
                    .bind(&id)
                    .execute(&mut *tx)
                    .await
            }
        }
        .map_err(db_error)?
        .rows_affected();
        let (target_type, target_id) = scope.audit_target();
        Database::record_tx_with_context(
            &mut tx,
            context,
            "template.delete",
            target_type,
            &target_id,
            serde_json::json!({"name": name, "deleted": deleted}),
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }

    /// Resolve the body to use for `name` on `list` in `language`.
    /// # Errors
    /// Returns validation for an unknown name or a database error. A stored
    /// template that cannot be loaded is skipped, never an error.
    pub async fn resolve(
        &self,
        name: &str,
        list: &MailingList,
        language: &str,
    ) -> Result<Resolved> {
        let mut tx = self.db.pool().begin().await.map_err(db_error)?;
        let resolved = resolve_tx(&mut tx, name, list, language).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(resolved)
    }
}

/// Candidate `(scope, scope_id, language)` triples, most specific first.
fn candidates(list: &MailingList, language: &str) -> Vec<(&'static str, String, String)> {
    let mut languages = vec![language.to_owned()];
    if language != DEFAULT_LANGUAGE {
        languages.push(DEFAULT_LANGUAGE.to_owned());
    }
    let scopes: [(&'static str, String); 3] = [
        Scope::List(list.id.clone()).column_values(),
        Scope::Domain(list.id.mail_host().to_owned()).column_values(),
        Scope::Site.column_values(),
    ];
    let mut out = Vec::with_capacity(6);
    for (scope, id) in scopes {
        for lang in &languages {
            out.push((scope, id.clone(), lang.clone()));
        }
    }
    out
}

/// Transaction-bound resolution, for notice producers that already hold one.
/// # Errors
/// Returns validation for an unknown name or a database error.
pub(crate) async fn resolve_tx(
    tx: &mut Transaction<'_, Any>,
    name: &str,
    list: &MailingList,
    language: &str,
) -> Result<Resolved> {
    validate_name(name)?;
    for (scope, scope_id, lang) in candidates(list, language) {
        let row = sqlx::query(
            "SELECT uri, body FROM templates WHERE name=$1 AND scope=$2 AND scope_id=$3 AND language=$4",
        )
        .bind(name)
        .bind(scope)
        .bind(&scope_id)
        .bind(&lang)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_error)?;
        let Some(row) = row else {
            continue;
        };
        let source = format!("{scope}:{lang}");
        if let Some(body) = row.try_get::<Option<String>, _>("body").map_err(db_error)? {
            return Ok(Resolved { body, source });
        }
        let Some(uri) = row.try_get::<Option<String>, _>("uri").map_err(db_error)? else {
            continue;
        };
        match parse_uri(&uri).and_then(|parsed| load(&parsed, &lang)) {
            Ok(body) => return Ok(Resolved { body, source }),
            Err(error) => {
                // Fall through to the next candidate: a broken template must
                // never block the subscription or moderation that needs it.
                tracing::warn!(template = name, scope = %source, %error, "template unavailable, falling back");
            }
        }
    }
    let body = listmngr_mail::templates::builtin_in(name, language)
        .ok_or_else(|| Error::Validation(format!("unknown template name: {name}")))?;
    Ok(Resolved {
        body: body.to_owned(),
        source: format!("builtin:{}", listmngr_i18n::negotiate(language)),
    })
}
