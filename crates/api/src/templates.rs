//! Mailman's template URI resource (`/uris`) on the list, domain and site
//! scopes, plus the listmngr inline-body extension under `/templates`.
//!
//! Wire shape follows Mailman 3.3 so `mailmanclient`'s `templates` /
//! `set_template` work unchanged: `GET` lists `{name, uri, self_link}`
//! entries, `PATCH` takes `name=uri` pairs (with optional `username` and
//! `password` for `https://` sources), `PUT` replaces the whole set, `DELETE`
//! clears it, and `/uris/{name}` addresses one template.
use crate::{
    ApiError, ApiFlavor, ApiResult, AppState, ErrorResponse, JsonOrForm, PageQuery, audit_context,
    authorize, authorize_domain, authorize_list, page_response, page_window, parse_list_path, peer,
};
use axum::{
    Json, Router,
    extract::{ConnectInfo, Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, put},
};
use listmngr_core::Error;
use listmngr_db::templates::Scope;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::net::SocketAddr;

/// Template routes on every scope, mounted on both prefixes.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/domains/{host}/uris",
            get(domain_uris)
                .patch(patch_domain_uris)
                .put(put_domain_uris)
                .delete(delete_domain_uris),
        )
        .route(
            "/domains/{host}/uris/{name}",
            get(get_domain_uri)
                .patch(set_domain_uri)
                .delete(delete_domain_uri),
        )
        .route(
            "/uris",
            get(site_uris)
                .patch(patch_site_uris)
                .put(put_site_uris)
                .delete(delete_site_uris),
        )
        .route(
            "/uris/{name}",
            get(get_site_uri)
                .patch(set_site_uri)
                .delete(delete_site_uri),
        )
        .route(
            "/lists/{id}/uris",
            get(list_uris)
                .patch(patch_list_uris)
                .put(put_list_uris)
                .delete(delete_list_uris),
        )
        .route(
            "/lists/{id}/uris/{name}",
            get(get_list_uri)
                .patch(set_list_uri)
                .delete(delete_list_uri),
        )
        .route(
            "/lists/{id}/templates/{name}",
            put(put_list_template_body).delete(delete_list_template_body),
        )
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct TemplateUriResponse {
    pub name: String,
    pub uri: String,
    pub self_link: String,
}

/// Template name to URI pairs, plus optional credentials for `https://`
/// sources. Mailman's `/uris` PATCH/PUT payload.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct TemplateUrisInput {
    /// Basic-auth username for `https://` sources.
    pub username: Option<String>,
    /// Basic-auth password for `https://` sources; never projected back.
    pub password: Option<String>,
    /// `template name → uri` assignments.
    #[serde(flatten)]
    pub uris: std::collections::BTreeMap<String, String>,
}

/// One template URI, Mailman's `/uris/{name}` PATCH payload.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TemplateUriInput {
    pub uri: String,
    pub username: Option<String>,
    pub password: Option<String>,
}

/// Inline body for one template name (listmngr extension).
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TemplateBodyInput {
    /// Language tag the body is written in, e.g. `en` or `vi`.
    pub language: String,
    pub body: String,
}

const fn prefix(flavor: ApiFlavor) -> &'static str {
    match flavor {
        ApiFlavor::V1 => "/api/v1",
        ApiFlavor::Compat31 => "/3.1",
    }
}

fn scope_path(flavor: ApiFlavor, scope: &Scope) -> String {
    match scope {
        Scope::Site => format!("{}/uris", prefix(flavor)),
        Scope::Domain(host) => format!("{}/domains/{host}/uris", prefix(flavor)),
        Scope::List(id) => format!("{}/lists/{id}/uris", prefix(flavor)),
    }
}

fn entry(flavor: ApiFlavor, scope: &Scope, name: &str, uri: &str) -> Value {
    json!(TemplateUriResponse {
        name: name.into(),
        uri: uri.into(),
        self_link: format!("{}/{name}", scope_path(flavor, scope)),
    })
}

/// Resolve the scope from the route and authorize the caller for it.
async fn scoped(
    s: &AppState,
    h: &HeaderMap,
    addr: SocketAddr,
    write: bool,
    list: Option<&str>,
    domain: Option<&str>,
) -> ApiResult<(Scope, crate::TokenAuth)> {
    if let Some(id) = list {
        let id = parse_list_path(s.flavor, id)?;
        let scope = if write { "lists:write" } else { "lists:read" };
        let auth = authorize_list(s, h, addr, scope, &id).await?;
        return Ok((Scope::List(id), auth));
    }
    if let Some(host) = domain {
        let scope = if write { "lists:write" } else { "lists:read" };
        let auth = authorize_domain(s, h, addr, scope, host).await?;
        return Ok((Scope::Domain(host.to_owned()), auth));
    }
    let auth = authorize(s, h, addr, "admin").await?;
    Ok((Scope::Site, auth))
}

fn assignments(input: &TemplateUrisInput) -> Vec<(String, String)> {
    input
        .uris
        .iter()
        .map(|(name, uri)| (name.clone(), uri.clone()))
        .collect()
}

async fn list_scope(
    s: &AppState,
    h: &HeaderMap,
    addr: SocketAddr,
    q: &PageQuery,
    list: Option<&str>,
    domain: Option<&str>,
) -> ApiResult<Json<Value>> {
    let (scope, _) = scoped(s, h, addr, false, list, domain).await?;
    let rows = s.db.templates().list_uris(&scope).await?;
    let entries: Vec<Value> = rows
        .iter()
        .map(|row| entry(s.flavor, &scope, &row.name, &row.uri))
        .collect();
    let (start, end) = page_window(entries.len(), q)?;
    Ok(Json(page_response(
        s.flavor,
        &entries[start..end],
        start,
        entries.len(),
    )))
}

async fn assign_scope(
    s: &AppState,
    h: &HeaderMap,
    addr: SocketAddr,
    list: Option<&str>,
    domain: Option<&str>,
    input: &TemplateUrisInput,
    replace: bool,
) -> ApiResult<StatusCode> {
    let (scope, auth) = scoped(s, h, addr, true, list, domain).await?;
    let assignments = assignments(input);
    let username = input.username.clone();
    let password = input.password.clone();
    let context = audit_context(&auth, addr);
    // Validate every pair before writing any, so a bad entry changes nothing.
    for (name, uri) in &assignments {
        if !listmngr_mail::templates::is_known_name(name) {
            return Err(ApiError(Error::Validation(format!(
                "unknown template name: {name}"
            ))));
        }
        listmngr_mail::templates::parse_uri(uri)
            .map_err(|error| ApiError(Error::Validation(format!("{name}: {error}"))))?;
    }
    if replace {
        s.db.templates()
            .delete_with_context(&scope, None, &context)
            .await?;
    }
    for (name, uri) in &assignments {
        s.db.templates()
            .set_uri_with_context(
                &scope,
                name,
                uri,
                username.as_deref(),
                password.as_deref(),
                &context,
            )
            .await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_scope(
    s: &AppState,
    h: &HeaderMap,
    addr: SocketAddr,
    list: Option<&str>,
    domain: Option<&str>,
    name: Option<&str>,
) -> ApiResult<StatusCode> {
    let (scope, auth) = scoped(s, h, addr, true, list, domain).await?;
    if let Some(name) = name {
        let exists =
            s.db.templates()
                .list_uris(&scope)
                .await?
                .iter()
                .any(|row| row.name == name);
        if !exists {
            return Err(ApiError(Error::NotFound(format!("template {name}"))));
        }
    }
    s.db.templates()
        .delete_with_context(&scope, name, &audit_context(&auth, addr))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_one(
    s: &AppState,
    h: &HeaderMap,
    addr: SocketAddr,
    list: Option<&str>,
    domain: Option<&str>,
    name: &str,
) -> ApiResult<Json<Value>> {
    let (scope, _) = scoped(s, h, addr, false, list, domain).await?;
    let row =
        s.db.templates()
            .list_uris(&scope)
            .await?
            .into_iter()
            .find(|row| row.name == name)
            .ok_or_else(|| ApiError(Error::NotFound(format!("template {name}"))))?;
    Ok(Json(entry(s.flavor, &scope, &row.name, &row.uri)))
}

async fn set_one(
    s: &AppState,
    h: &HeaderMap,
    addr: SocketAddr,
    list: Option<&str>,
    domain: Option<&str>,
    name: &str,
    input: &TemplateUriInput,
) -> ApiResult<StatusCode> {
    let mut uris = std::collections::BTreeMap::new();
    uris.insert(name.to_owned(), input.uri.clone());
    let assignment = TemplateUrisInput {
        username: input.username.clone(),
        password: input.password.clone(),
        uris,
    };
    assign_scope(s, h, addr, list, domain, &assignment, false).await
}

// ---- list scope -----------------------------------------------------------

#[utoipa::path(get, path = "/api/v1/lists/{id}/uris", params(("id" = String, Path), PageQuery),
    responses((status = 200, description = "Managed template URIs", body = TemplateUriPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn list_uris(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    list_scope(&s, &h, peer(c), &q, Some(&id), None).await
}

#[utoipa::path(patch, path = "/api/v1/lists/{id}/uris", params(("id" = String, Path)),
    request_body(content((TemplateUrisInput = "application/json"), (TemplateUrisInput = "application/x-www-form-urlencoded")), description = "Template name to URI pairs, plus optional username/password"),
    responses((status = 204, description = "Templates assigned"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn patch_list_uris(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(body): JsonOrForm<TemplateUrisInput>,
) -> ApiResult<StatusCode> {
    assign_scope(&s, &h, peer(c), Some(&id), None, &body, false).await
}

#[utoipa::path(put, path = "/api/v1/lists/{id}/uris", params(("id" = String, Path)),
    request_body(content((TemplateUrisInput = "application/json"), (TemplateUrisInput = "application/x-www-form-urlencoded")), description = "Complete replacement set of template name to URI pairs"),
    responses((status = 204, description = "Templates replaced"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn put_list_uris(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(body): JsonOrForm<TemplateUrisInput>,
) -> ApiResult<StatusCode> {
    assign_scope(&s, &h, peer(c), Some(&id), None, &body, true).await
}

#[utoipa::path(delete, path = "/api/v1/lists/{id}/uris", params(("id" = String, Path)),
    responses((status = 204, description = "Templates cleared"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn delete_list_uris(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<StatusCode> {
    delete_scope(&s, &h, peer(c), Some(&id), None, None).await
}

#[utoipa::path(get, path = "/api/v1/lists/{id}/uris/{name}", params(("id" = String, Path), ("name" = String, Path)),
    responses((status = 200, description = "One managed template URI", body = TemplateUriResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn get_list_uri(
    State(s): State<AppState>,
    Path((id, name)): Path<(String, String)>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    get_one(&s, &h, peer(c), Some(&id), None, &name).await
}

#[utoipa::path(patch, path = "/api/v1/lists/{id}/uris/{name}", params(("id" = String, Path), ("name" = String, Path)),
    request_body(content((TemplateUriInput = "application/json"), (TemplateUriInput = "application/x-www-form-urlencoded")), description = "`uri` plus optional username/password"),
    responses((status = 204, description = "Template assigned"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn set_list_uri(
    State(s): State<AppState>,
    Path((id, name)): Path<(String, String)>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(body): JsonOrForm<TemplateUriInput>,
) -> ApiResult<StatusCode> {
    set_one(&s, &h, peer(c), Some(&id), None, &name, &body).await
}

#[utoipa::path(delete, path = "/api/v1/lists/{id}/uris/{name}", params(("id" = String, Path), ("name" = String, Path)),
    responses((status = 204, description = "Template removed"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn delete_list_uri(
    State(s): State<AppState>,
    Path((id, name)): Path<(String, String)>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<StatusCode> {
    delete_scope(&s, &h, peer(c), Some(&id), None, Some(&name)).await
}

// ---- domain scope ---------------------------------------------------------

#[utoipa::path(get, path = "/api/v1/domains/{host}/uris", params(("host" = String, Path), PageQuery),
    responses((status = 200, description = "Managed template URIs", body = TemplateUriPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn domain_uris(
    State(s): State<AppState>,
    Path(host): Path<String>,
    Query(q): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    list_scope(&s, &h, peer(c), &q, None, Some(&host)).await
}

#[utoipa::path(patch, path = "/api/v1/domains/{host}/uris", params(("host" = String, Path)),
    request_body(content((TemplateUrisInput = "application/json"), (TemplateUrisInput = "application/x-www-form-urlencoded"))),
    responses((status = 204, description = "Templates assigned"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn patch_domain_uris(
    State(s): State<AppState>,
    Path(host): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(body): JsonOrForm<TemplateUrisInput>,
) -> ApiResult<StatusCode> {
    assign_scope(&s, &h, peer(c), None, Some(&host), &body, false).await
}

#[utoipa::path(put, path = "/api/v1/domains/{host}/uris", params(("host" = String, Path)),
    request_body(content((TemplateUrisInput = "application/json"), (TemplateUrisInput = "application/x-www-form-urlencoded"))),
    responses((status = 204, description = "Templates replaced"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn put_domain_uris(
    State(s): State<AppState>,
    Path(host): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(body): JsonOrForm<TemplateUrisInput>,
) -> ApiResult<StatusCode> {
    assign_scope(&s, &h, peer(c), None, Some(&host), &body, true).await
}

#[utoipa::path(delete, path = "/api/v1/domains/{host}/uris", params(("host" = String, Path)),
    responses((status = 204, description = "Templates cleared"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn delete_domain_uris(
    State(s): State<AppState>,
    Path(host): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<StatusCode> {
    delete_scope(&s, &h, peer(c), None, Some(&host), None).await
}

#[utoipa::path(get, path = "/api/v1/domains/{host}/uris/{name}", params(("host" = String, Path), ("name" = String, Path)),
    responses((status = 200, description = "One managed template URI", body = TemplateUriResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn get_domain_uri(
    State(s): State<AppState>,
    Path((host, name)): Path<(String, String)>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    get_one(&s, &h, peer(c), None, Some(&host), &name).await
}

#[utoipa::path(patch, path = "/api/v1/domains/{host}/uris/{name}", params(("host" = String, Path), ("name" = String, Path)),
    request_body(content((TemplateUriInput = "application/json"), (TemplateUriInput = "application/x-www-form-urlencoded"))),
    responses((status = 204, description = "Template assigned"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn set_domain_uri(
    State(s): State<AppState>,
    Path((host, name)): Path<(String, String)>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(body): JsonOrForm<TemplateUriInput>,
) -> ApiResult<StatusCode> {
    set_one(&s, &h, peer(c), None, Some(&host), &name, &body).await
}

#[utoipa::path(delete, path = "/api/v1/domains/{host}/uris/{name}", params(("host" = String, Path), ("name" = String, Path)),
    responses((status = 204, description = "Template removed"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn delete_domain_uri(
    State(s): State<AppState>,
    Path((host, name)): Path<(String, String)>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<StatusCode> {
    delete_scope(&s, &h, peer(c), None, Some(&host), Some(&name)).await
}

// ---- site scope -----------------------------------------------------------

#[utoipa::path(get, path = "/api/v1/uris", params(PageQuery),
    responses((status = 200, description = "Managed site template URIs", body = TemplateUriPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn site_uris(
    State(s): State<AppState>,
    Query(q): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    list_scope(&s, &h, peer(c), &q, None, None).await
}

#[utoipa::path(patch, path = "/api/v1/uris",
    request_body(content((TemplateUrisInput = "application/json"), (TemplateUrisInput = "application/x-www-form-urlencoded"))),
    responses((status = 204, description = "Templates assigned"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn patch_site_uris(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(body): JsonOrForm<TemplateUrisInput>,
) -> ApiResult<StatusCode> {
    assign_scope(&s, &h, peer(c), None, None, &body, false).await
}

#[utoipa::path(put, path = "/api/v1/uris",
    request_body(content((TemplateUrisInput = "application/json"), (TemplateUrisInput = "application/x-www-form-urlencoded"))),
    responses((status = 204, description = "Templates replaced"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn put_site_uris(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(body): JsonOrForm<TemplateUrisInput>,
) -> ApiResult<StatusCode> {
    assign_scope(&s, &h, peer(c), None, None, &body, true).await
}

#[utoipa::path(delete, path = "/api/v1/uris",
    responses((status = 204, description = "Templates cleared"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn delete_site_uris(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<StatusCode> {
    delete_scope(&s, &h, peer(c), None, None, None).await
}

#[utoipa::path(get, path = "/api/v1/uris/{name}", params(("name" = String, Path)),
    responses((status = 200, description = "One managed site template URI", body = TemplateUriResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn get_site_uri(
    State(s): State<AppState>,
    Path(name): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    get_one(&s, &h, peer(c), None, None, &name).await
}

#[utoipa::path(patch, path = "/api/v1/uris/{name}", params(("name" = String, Path)),
    request_body(content((TemplateUriInput = "application/json"), (TemplateUriInput = "application/x-www-form-urlencoded"))),
    responses((status = 204, description = "Template assigned"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn set_site_uri(
    State(s): State<AppState>,
    Path(name): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(body): JsonOrForm<TemplateUriInput>,
) -> ApiResult<StatusCode> {
    set_one(&s, &h, peer(c), None, None, &name, &body).await
}

#[utoipa::path(delete, path = "/api/v1/uris/{name}", params(("name" = String, Path)),
    responses((status = 204, description = "Template removed"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn delete_site_uri(
    State(s): State<AppState>,
    Path(name): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<StatusCode> {
    delete_scope(&s, &h, peer(c), None, None, Some(&name)).await
}

// ---- inline bodies (listmngr extension) -----------------------------------

#[utoipa::path(put, path = "/api/v1/lists/{id}/templates/{name}", params(("id" = String, Path), ("name" = String, Path)),
    request_body(content((TemplateBodyInput = "application/json"), (TemplateBodyInput = "application/x-www-form-urlencoded"))),
    responses((status = 204, description = "Inline template body stored"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn put_list_template_body(
    State(s): State<AppState>,
    Path((id, name)): Path<(String, String)>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(input): JsonOrForm<TemplateBodyInput>,
) -> ApiResult<StatusCode> {
    let id = parse_list_path(s.flavor, &id)?;
    let addr = peer(c);
    let auth = authorize_list(&s, &h, addr, "lists:write", &id).await?;
    s.db.templates()
        .set_body_with_context(
            &Scope::List(id),
            &name,
            &input.language,
            &input.body,
            &audit_context(&auth, addr),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(delete, path = "/api/v1/lists/{id}/templates/{name}", params(("id" = String, Path), ("name" = String, Path)),
    responses((status = 204, description = "Template removed in every language"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)), security(("bearerAuth" = [])))]
pub async fn delete_list_template_body(
    State(s): State<AppState>,
    Path((id, name)): Path<(String, String)>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<StatusCode> {
    let id = parse_list_path(s.flavor, &id)?;
    let addr = peer(c);
    let auth = authorize_list(&s, &h, addr, "lists:write", &id).await?;
    s.db.templates()
        .delete_with_context(&Scope::List(id), Some(&name), &audit_context(&auth, addr))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Page of managed template URIs.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct TemplateUriPageResponse {
    pub items: Vec<TemplateUriResponse>,
    pub total: usize,
    pub start: usize,
    pub count: usize,
    pub next_cursor: Option<String>,
}
