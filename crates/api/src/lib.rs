#![forbid(unsafe_code)]

//! Axum REST API shared by Mailman-compatible `/3.1` and typed `/api/v1` routes.

use axum::{
    Json, Router,
    body::Bytes,
    extract::{ConnectInfo, FromRequest, Path, Query, Request, State},
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use base64::Engine;
use dashmap::DashMap;
use listmngr_core::{
    Config, Error, ListId, MemberId, MemberRole, Preferences, SubscriptionMode, UserId,
    builtin_styles,
};
use listmngr_db::{Database, NewList, NewMember, NewUser, TokenAuth};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};
use utoipa::OpenApi;

#[derive(Debug)]
struct JsonOrForm<T>(T);

impl<S, T> FromRequest<S> for JsonOrForm<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = Response;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let json = request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("application/json"));
        let bytes = Bytes::from_request(request, state)
            .await
            .map_err(IntoResponse::into_response)?;
        let value = if json {
            serde_json::from_slice(&bytes)
        } else {
            serde_urlencoded::from_bytes(&bytes)
                .map_err(|error| serde_json::Error::io(std::io::Error::other(error)))
        }
        .map_err(|_| StatusCode::BAD_REQUEST.into_response())?;
        Ok(Self(value))
    }
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct ErrorResponse {
    code: &'static str,
    correlation_id: uuid::Uuid,
    title: &'static str,
    detail: &'static str,
}

struct SecurityAddon;
impl utoipa::Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "bearerAuth",
                SecurityScheme::Http(
                    HttpBuilder::new()
                        .scheme(HttpAuthScheme::Bearer)
                        .bearer_format("lm_<uuid>_<secret>")
                        .build(),
                ),
            );
        }
    }
}

#[derive(OpenApi)]
#[openapi(
    info(title = "listmngr API", version = "0.1.0"),
    paths(
        system_versions,
        system_config,
        system_config_section,
        system_preferences,
        system_pipelines,
        system_chains,
        domains_list,
        domains_create,
        domains_get,
        domains_delete,
        domain_lists,
        domain_owners,
        domain_uris,
        lists_list,
        lists_create,
        styles,
        lists_get,
        lists_delete,
        list_config,
        list_config_put,
        list_config_patch,
        list_config_attr,
        list_config_attr_put,
        list_config_attr_patch,
        list_archivers,
        list_uris,
        list_templates,
        roster,
        list_member,
        members_create,
        members_mass,
        members_find,
        members_get,
        member_patch,
        members_delete,
        member_preferences,
        member_preferences_put,
        member_preferences_patch,
        member_all_preferences,
        users_list,
        users_create,
        users_get,
        users_patch,
        users_delete,
        user_addresses,
        user_address_link,
        user_preferences,
        user_preferences_put,
        user_preferences_patch,
        user_all_preferences,
        user_login,
        address_get,
        address_verify,
        address_unverify,
        address_user,
        address_link,
        address_unlink,
        address_memberships,
        address_preferences,
        address_preferences_put,
        address_preferences_patch,
        address_all_preferences,
        owners
    ),
    components(schemas(ErrorResponse)),
    modifiers(&SecurityAddon)
)]
struct ApiDoc;

#[derive(Debug, Clone)]
pub struct AppState {
    db: Database,
    config: Config,
    rate: Arc<RateLimiter>,
    flavor: ApiFlavor,
}

#[derive(Debug, Clone, Copy)]
enum ApiFlavor {
    Compat31,
    V1,
}

#[derive(Debug)]
struct RateLimiter {
    limit: u32,
    hits: DashMap<String, (Instant, u32)>,
}
impl RateLimiter {
    fn check(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut hit = self.hits.entry(key.to_owned()).or_insert((now, 0));
        if now.duration_since(hit.0) >= Duration::from_secs(60) {
            *hit = (now, 0);
        }
        hit.1 += 1;
        hit.1 <= self.limit
    }
}

#[derive(Debug)]
struct ApiError(Error);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self.0 {
            Error::Authentication => StatusCode::UNAUTHORIZED,
            Error::Forbidden(_) => StatusCode::FORBIDDEN,
            Error::NotFound(_) => StatusCode::NOT_FOUND,
            Error::Conflict(_) => StatusCode::CONFLICT,
            Error::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Error::Validation(_) | Error::InvalidListId(_) => StatusCode::BAD_REQUEST,
            Error::Config(_) | Error::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let code = match self.0 {
            Error::Authentication => "authentication",
            Error::Forbidden(_) => "forbidden",
            Error::NotFound(_) => "not_found",
            Error::Conflict(_) => "conflict",
            Error::RateLimited => "rate_limited",
            Error::Validation(_) | Error::InvalidListId(_) => "validation",
            Error::Config(_) | Error::Database(_) => "internal",
        };
        let correlation_id = uuid::Uuid::now_v7();
        let title = status.canonical_reason().unwrap_or("request failed");
        let detail = if status.is_server_error() {
            "request failed; quote the correlation_id to an administrator"
        } else {
            title
        };
        (
            status,
            Json(ErrorResponse {
                code,
                correlation_id,
                title,
                detail,
            }),
        )
            .into_response()
    }
}
impl From<Error> for ApiError {
    fn from(value: Error) -> Self {
        Self(value)
    }
}
type ApiResult<T> = Result<T, ApiError>;

pub fn router(db: Database, config: Config, rate_per_minute: u32) -> Router {
    let state = AppState {
        db,
        config,
        rate: Arc::new(RateLimiter {
            limit: rate_per_minute,
            hits: DashMap::new(),
        }),
        flavor: ApiFlavor::V1,
    };
    let mut compat_state = state.clone();
    compat_state.flavor = ApiFlavor::Compat31;
    Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route("/metrics", get(metrics))
        .route("/openapi.json", get(openapi))
        .route("/api/docs", get(swagger))
        .nest("/3.1", phase_one_routes().with_state(compat_state))
        .nest("/api/v1", phase_one_routes().with_state(state.clone()))
        .with_state(state)
}

fn phase_one_routes() -> Router<AppState> {
    Router::new()
        .route("/system/versions", get(system_versions))
        .route("/system/configuration", get(system_config))
        .route(
            "/system/configuration/{section}",
            get(system_config_section),
        )
        .route("/system/preferences", get(system_preferences))
        .route("/system/pipelines", get(system_pipelines))
        .route("/system/chains", get(system_chains))
        .route("/domains", get(domains_list).post(domains_create))
        .route("/domains/{host}", get(domains_get).delete(domains_delete))
        .route("/domains/{host}/lists", get(domain_lists))
        .route("/domains/{host}/owners", get(domain_owners))
        .route("/domains/{host}/uris", get(domain_uris))
        .route("/lists", get(lists_list).post(lists_create))
        .route("/lists/styles", get(styles))
        .route("/lists/{id}", get(lists_get).delete(lists_delete))
        .route(
            "/lists/{id}/config",
            get(list_config)
                .put(list_config_put)
                .patch(list_config_patch),
        )
        .route(
            "/lists/{id}/config/{attr}",
            get(list_config_attr)
                .put(list_config_attr_put)
                .patch(list_config_attr_patch),
        )
        .route("/lists/{id}/archivers", get(list_archivers))
        .route("/lists/{id}/uris", get(list_uris))
        .route("/lists/{id}/templates", get(list_templates))
        .route("/lists/{id}/roster/{role}", get(roster))
        .route("/lists/{id}/member/{email}", get(list_member))
        .route("/members", post(members_create))
        .route("/members/mass", post(members_mass))
        .route("/members/find", post(members_find))
        .route(
            "/members/{id}",
            get(members_get).patch(member_patch).delete(members_delete),
        )
        .route(
            "/members/{id}/preferences",
            get(member_preferences)
                .put(member_preferences_put)
                .patch(member_preferences_patch),
        )
        .route("/members/{id}/all/preferences", get(member_all_preferences))
        .route("/users", get(users_list).post(users_create))
        .route(
            "/users/{id}",
            get(users_get).patch(users_patch).delete(users_delete),
        )
        .route(
            "/users/{id}/addresses",
            get(user_addresses).post(user_address_link),
        )
        .route(
            "/users/{id}/preferences",
            get(user_preferences)
                .put(user_preferences_put)
                .patch(user_preferences_patch),
        )
        .route("/users/{id}/all/preferences", get(user_all_preferences))
        .route("/users/{id}/login", post(user_login))
        .route("/addresses/{email}", get(address_get))
        .route("/addresses/{email}/verify", post(address_verify))
        .route("/addresses/{email}/unverify", post(address_unverify))
        .route(
            "/addresses/{email}/user",
            get(address_user).post(address_link).delete(address_unlink),
        )
        .route("/addresses/{email}/memberships", get(address_memberships))
        .route(
            "/addresses/{email}/preferences",
            get(address_preferences)
                .put(address_preferences_put)
                .patch(address_preferences_patch),
        )
        .route(
            "/addresses/{email}/all/preferences",
            get(address_all_preferences),
        )
        .route("/owners", get(owners))
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({"status":"ok"})))
}
async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    match sqlx::query("SELECT 1").execute(state.db.pool()).await {
        Ok(_) => (StatusCode::OK, Json(json!({"status":"ready"}))),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"status":"not-ready"})),
        ),
    }
}
async fn metrics() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        "# HELP listmngr_up Service readiness.\n# TYPE listmngr_up gauge\nlistmngr_up 1\n",
    )
}
async fn openapi() -> Json<Value> {
    Json(serde_json::to_value(ApiDoc::openapi()).expect("OpenAPI serializes"))
}
async fn swagger() -> Html<&'static str> {
    Html(
        r#"<!doctype html><html><head><title>listmngr API</title><link rel="stylesheet" href="https://unpkg.com/swagger-ui-dist@5/swagger-ui.css"></head><body><div id="swagger-ui"></div><script src="https://unpkg.com/swagger-ui-dist@5/swagger-ui-bundle.js"></script><script>SwaggerUIBundle({url:'/openapi.json',dom_id:'#swagger-ui'});</script></body></html>"#,
    )
}

async fn authorize(
    state: &AppState,
    headers: &HeaderMap,
    addr: SocketAddr,
    scope: &str,
) -> ApiResult<TokenAuth> {
    let peer_key = addr.ip().to_string();
    if !state.rate.check(&peer_key) {
        return Err(ApiError(Error::RateLimited));
    }
    let value = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or(ApiError(Error::Authentication))?;
    let token = if let Some(token) = value.strip_prefix("Bearer ") {
        token.to_owned()
    } else if let Some(encoded) = value.strip_prefix("Basic ") {
        if !state.config.api.compat_basic_auth {
            return Err(ApiError(Error::Authentication));
        }
        let ip = addr.ip();
        if !state
            .config
            .api
            .compat_basic_auth_allow
            .iter()
            .any(|network| network.contains(&ip))
        {
            return Err(ApiError(Error::Authentication));
        }
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| ApiError(Error::Authentication))?;
        let decoded = String::from_utf8(decoded).map_err(|_| ApiError(Error::Authentication))?;
        let (id, secret) = decoded
            .split_once(':')
            .ok_or(ApiError(Error::Authentication))?;
        format!("lm_{id}_{secret}")
    } else {
        return Err(ApiError(Error::Authentication));
    };
    let auth = state.db.tokens().authenticate(&token).await?;
    if !auth.has_scope(scope) {
        return Err(ApiError(Error::Forbidden(scope.into())));
    }
    Ok(auth)
}
async fn authorize_domain(
    state: &AppState,
    headers: &HeaderMap,
    addr: SocketAddr,
    scope: &str,
    host: &str,
) -> ApiResult<TokenAuth> {
    let auth = authorize(state, headers, addr, scope).await?;
    let domain = state.db.domains().get(host).await?;
    let list_matches = auth
        .list_id
        .as_ref()
        .is_none_or(|list| list.mail_host() == domain.mail_host);
    if !auth.allows_domain(domain.id) || !list_matches {
        return Err(ApiError(Error::Forbidden(scope.into())));
    }
    Ok(auth)
}
async fn authorize_list(
    state: &AppState,
    headers: &HeaderMap,
    addr: SocketAddr,
    scope: &str,
    list: &ListId,
) -> ApiResult<TokenAuth> {
    let auth = authorize(state, headers, addr, scope).await?;
    let domain = state.db.domains().get(list.mail_host()).await?;
    if !auth.allows_list(list, domain.id) {
        return Err(ApiError(Error::Forbidden(scope.into())));
    }
    Ok(auth)
}
async fn authorize_member(
    state: &AppState,
    headers: &HeaderMap,
    addr: SocketAddr,
    scope: &str,
    member_id: MemberId,
) -> ApiResult<(TokenAuth, listmngr_core::Member)> {
    let auth = authorize(state, headers, addr, scope).await?;
    let member = state.db.members().get(member_id).await?;
    let domain = state.db.domains().get(member.list_id.mail_host()).await?;
    if !auth.allows_list(&member.list_id, domain.id) {
        return Err(ApiError(Error::Forbidden(scope.into())));
    }
    Ok((auth, member))
}
const fn peer(connect: ConnectInfo<SocketAddr>) -> SocketAddr {
    connect.0
}
fn page<T: Serialize>(flavor: ApiFlavor, entries: T) -> Value {
    let entries = serde_json::to_value(entries).expect("page entries serialize");
    let total_size = entries.as_array().map_or(0, Vec::len);
    match flavor {
        ApiFlavor::Compat31 => {
            json!({"entries":entries,"start":0,"total_size":total_size,"http_etag":"phase1"})
        }
        ApiFlavor::V1 => json!({"items":entries,"next_cursor":null}),
    }
}

fn domain_value(flavor: ApiFlavor, domain: &listmngr_core::Domain) -> Value {
    let mut value = serde_json::to_value(domain).expect("domain serializes");
    if matches!(flavor, ApiFlavor::Compat31) {
        value.as_object_mut().expect("domain is an object").insert(
            "self_link".into(),
            json!(format!("/3.1/domains/{}", domain.mail_host)),
        );
    }
    value
}

#[utoipa::path(
    get, path = "/api/v1/system/versions",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn system_versions(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "system:read").await?;
    Ok(Json(
        json!({"listmngr_version":env!("CARGO_PKG_VERSION"),"api_version":"3.1"}),
    ))
}
#[utoipa::path(
    get, path = "/api/v1/system/configuration",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn system_config(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "system:read").await?;
    Ok(Json(s.config.redacted_json()))
}
#[utoipa::path(
    get, path = "/api/v1/system/configuration/{section}",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn system_config_section(
    State(s): State<AppState>,
    Path(section): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "system:read").await?;
    let value = s.config.redacted_json();
    Ok(Json(
        value
            .get(&section)
            .cloned()
            .ok_or(ApiError(Error::NotFound(section)))?,
    ))
}
#[utoipa::path(
    get, path = "/api/v1/system/preferences",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn system_preferences(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "system:read").await?;
    Ok(Json(
        serde_json::to_value(Preferences::system_defaults(s.config.site.default_language))
            .expect("serialize"),
    ))
}
#[utoipa::path(
    get, path = "/api/v1/system/pipelines",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn system_pipelines(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "system:read").await?;
    Ok(Json(page(s.flavor, Vec::<Value>::new())))
}
#[utoipa::path(
    get, path = "/api/v1/system/chains",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn system_chains(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "system:read").await?;
    Ok(Json(page(s.flavor, Vec::<Value>::new())))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct DomainInput {
    mail_host: String,
    #[serde(default)]
    description: String,
    alias_domain: Option<String>,
}
#[utoipa::path(
    post, path = "/api/v1/domains",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn domains_create(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(v): JsonOrForm<DomainInput>,
) -> ApiResult<Response> {
    authorize(&s, &h, peer(c), "lists:write").await?;
    let domain =
        s.db.domains()
            .create(&v.mail_host, &v.description, v.alias_domain.as_deref())
            .await?;
    let value = domain_value(s.flavor, &domain);
    let response = match s.flavor {
        ApiFlavor::Compat31 => (
            StatusCode::CREATED,
            [(
                header::LOCATION,
                format!("/3.1/domains/{}", domain.mail_host),
            )],
            Json(value),
        )
            .into_response(),
        ApiFlavor::V1 => (StatusCode::CREATED, Json(value)).into_response(),
    };
    Ok(response)
}
#[utoipa::path(
    get, path = "/api/v1/domains",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn domains_list(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "lists:read").await?;
    Ok(Json(page(s.flavor, s.db.domains().list().await?)))
}
#[utoipa::path(
    get, path = "/api/v1/domains/{host}",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn domains_get(
    State(s): State<AppState>,
    Path(host): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize_domain(&s, &h, peer(c), "lists:read", &host).await?;
    Ok(Json(domain_value(
        s.flavor,
        &s.db.domains().get(&host).await?,
    )))
}
#[utoipa::path(
    delete, path = "/api/v1/domains/{host}",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn domains_delete(
    State(s): State<AppState>,
    Path(host): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<StatusCode> {
    authorize_domain(&s, &h, peer(c), "lists:write", &host).await?;
    s.db.domains().delete(&host).await?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(
    get, path = "/api/v1/domains/{host}/lists",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn domain_lists(
    State(s): State<AppState>,
    Path(host): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize_domain(&s, &h, peer(c), "lists:read", &host).await?;
    Ok(Json(page(s.flavor, s.db.lists().by_domain(&host).await?)))
}
#[utoipa::path(
    get, path = "/api/v1/domains/{host}/owners",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn domain_owners(
    State(s): State<AppState>,
    Path(host): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize_domain(&s, &h, peer(c), "lists:read", &host).await?;
    Ok(Json(page(s.flavor, s.db.domains().owners(&host).await?)))
}
#[utoipa::path(
    get, path = "/api/v1/domains/{host}/uris",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn domain_uris(
    State(s): State<AppState>,
    Path(host): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize_domain(&s, &h, peer(c), "lists:read", &host).await?;
    Ok(Json(json!({"self_link":format!("/api/v1/domains/{host}")})))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct ListQuery {
    advertised: Option<bool>,
}
#[utoipa::path(
    get, path = "/api/v1/lists",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn lists_list(
    State(s): State<AppState>,
    Query(q): Query<ListQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "lists:read").await?;
    Ok(Json(page(s.flavor, s.db.lists().list(q.advertised).await?)))
}
#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct ListInput {
    list_id: Option<ListId>,
    fqdn_listname: Option<String>,
    display_name: Option<String>,
    style: Option<String>,
    style_name: Option<String>,
}

impl ListInput {
    fn into_new_list(self) -> Result<NewList, Error> {
        let list_id = match (self.list_id, self.fqdn_listname) {
            (Some(list_id), _) => list_id,
            (None, Some(fqdn)) => {
                let (name, host) = fqdn
                    .split_once('@')
                    .ok_or_else(|| Error::InvalidListId(fqdn.clone()))?;
                format!("{name}.{host}").parse()?
            }
            (None, None) => return Err(Error::Validation("list_id is required".into())),
        };
        let display_name = self
            .display_name
            .unwrap_or_else(|| list_id.list_name().to_owned());
        Ok(NewList {
            list_id,
            display_name,
            style: self
                .style
                .or(self.style_name)
                .unwrap_or_else(|| "legacy-default".into()),
        })
    }
}

fn list_value(flavor: ApiFlavor, list: &listmngr_core::MailingList) -> Value {
    let mut value = serde_json::to_value(list).expect("list serializes");
    if matches!(flavor, ApiFlavor::Compat31) {
        let object = value.as_object_mut().expect("list is an object");
        object.insert("list_id".into(), json!(list.id));
        object.insert("fqdn_listname".into(), json!(list.fqdn_listname()));
        object.insert("list_name".into(), json!(list.id.list_name()));
        object.insert("mail_host".into(), json!(list.id.mail_host()));
        object.insert("self_link".into(), json!(format!("/3.1/lists/{}", list.id)));
    }
    value
}

fn parse_list_path(flavor: ApiFlavor, value: &str) -> Result<ListId, Error> {
    if matches!(flavor, ApiFlavor::Compat31) {
        if let Some((name, host)) = value.split_once('@') {
            return format!("{name}.{host}").parse();
        }
    }
    value.parse()
}

#[utoipa::path(
    post, path = "/api/v1/lists",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn lists_create(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(v): JsonOrForm<ListInput>,
) -> ApiResult<Response> {
    authorize(&s, &h, peer(c), "lists:write").await?;
    let list = s.db.lists().create(v.into_new_list()?).await?;
    let value = list_value(s.flavor, &list);
    let response = match s.flavor {
        ApiFlavor::Compat31 => (
            StatusCode::CREATED,
            [(header::LOCATION, format!("/3.1/lists/{}", list.id))],
            Json(value),
        )
            .into_response(),
        ApiFlavor::V1 => (StatusCode::CREATED, Json(value)).into_response(),
    };
    Ok(response)
}
#[utoipa::path(
    get, path = "/api/v1/lists/{id}",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn lists_get(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id: ListId = id.parse()?;
    authorize_list(&s, &h, peer(c), "lists:read", &id).await?;
    Ok(Json(list_value(s.flavor, &s.db.lists().get(&id).await?)))
}
#[utoipa::path(
    delete, path = "/api/v1/lists/{id}",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn lists_delete(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<StatusCode> {
    let id = id.parse()?;
    authorize_list(&s, &h, peer(c), "lists:write", &id).await?;
    s.db.lists().delete(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(
    get, path = "/api/v1/lists/styles",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn styles(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "lists:read").await?;
    Ok(Json(page(
        s.flavor,
        builtin_styles()
            .iter()
            .map(|v| v.name())
            .collect::<Vec<_>>(),
    )))
}
#[utoipa::path(
    get, path = "/api/v1/lists/{id}/config",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn list_config(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = parse_list_path(s.flavor, &id)?;
    authorize_list(&s, &h, peer(c), "lists:read", &id).await?;
    Ok(Json(list_config_value(&s.db.lists().get(&id).await?)))
}

fn list_config_value(list: &listmngr_core::MailingList) -> Value {
    let mut value = serde_json::to_value(list).expect("list config serializes");
    let object = value.as_object_mut().expect("list config is an object");
    object.insert("mail_host".into(), json!(list.id.mail_host()));
    object.insert("list_name".into(), json!(list.id.list_name()));
    object.insert("list_id".into(), json!(list.id));
    object.insert("fqdn_listname".into(), json!(list.id.posting_address()));
    object.insert("posting_address".into(), json!(list.id.posting_address()));
    object.insert("bounces_address".into(), json!(list.id.bounces_address()));
    object.insert("join_address".into(), json!(list.id.join_address()));
    object.insert("leave_address".into(), json!(list.id.leave_address()));
    object.insert("owner_address".into(), json!(list.id.owner_address()));
    object.insert("request_address".into(), json!(list.id.request_address()));
    object.insert(
        "no_reply_address".into(),
        json!(list.id.address_with_suffix("noreply")),
    );
    value
}
#[utoipa::path(
    put, path = "/api/v1/lists/{id}/config",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn list_config_put(
    state: State<AppState>,
    path: Path<String>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    body: JsonOrForm<Value>,
) -> ApiResult<Json<Value>> {
    list_config_write(state, path, headers, connect, body).await
}
#[utoipa::path(
    patch, path = "/api/v1/lists/{id}/config",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn list_config_patch(
    state: State<AppState>,
    path: Path<String>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    body: JsonOrForm<Value>,
) -> ApiResult<Json<Value>> {
    list_config_write(state, path, headers, connect, body).await
}
async fn list_config_write(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(v): JsonOrForm<Value>,
) -> ApiResult<Json<Value>> {
    let id = parse_list_path(s.flavor, &id)?;
    authorize_list(&s, &h, peer(c), "lists:write", &id).await?;
    Ok(Json(
        serde_json::to_value(s.db.lists().update(&id, &v).await?).expect("serialize"),
    ))
}
#[utoipa::path(
    get, path = "/api/v1/lists/{id}/config/{attr}",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn list_config_attr(
    State(s): State<AppState>,
    Path((id, attr)): Path<(String, String)>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = id.parse()?;
    authorize_list(&s, &h, peer(c), "lists:read", &id).await?;
    let v = list_config_value(&s.db.lists().get(&id).await?);
    Ok(Json(
        v.get(&attr)
            .cloned()
            .ok_or(ApiError(Error::NotFound(attr)))?,
    ))
}
#[utoipa::path(
    put, path = "/api/v1/lists/{id}/config/{attr}",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn list_config_attr_put(
    state: State<AppState>,
    path: Path<(String, String)>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    body: Json<Value>,
) -> ApiResult<Json<Value>> {
    list_config_attr_write(state, path, headers, connect, body).await
}
#[utoipa::path(
    patch, path = "/api/v1/lists/{id}/config/{attr}",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn list_config_attr_patch(
    state: State<AppState>,
    path: Path<(String, String)>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    body: Json<Value>,
) -> ApiResult<Json<Value>> {
    list_config_attr_write(state, path, headers, connect, body).await
}
async fn list_config_attr_write(
    State(s): State<AppState>,
    Path((id, attr)): Path<(String, String)>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    Json(v): Json<Value>,
) -> ApiResult<Json<Value>> {
    let id = id.parse()?;
    authorize_list(&s, &h, peer(c), "lists:write", &id).await?;
    let value = if v.is_object() {
        v.get(&attr)
            .cloned()
            .ok_or(ApiError(Error::Validation(format!("missing {attr}"))))?
    } else {
        v
    };
    Ok(Json(
        serde_json::to_value(s.db.lists().update(&id, &json!({attr:value})).await?)
            .expect("serialize"),
    ))
}
#[utoipa::path(
    get, path = "/api/v1/lists/{id}/archivers",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn list_archivers(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = id.parse()?;
    authorize_list(&s, &h, peer(c), "lists:read", &id).await?;
    Ok(Json(page(s.flavor, s.db.lists().archivers(&id).await?)))
}
#[utoipa::path(
    get, path = "/api/v1/lists/{id}/uris",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn list_uris(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id: ListId = id.parse()?;
    authorize_list(&s, &h, peer(c), "lists:read", &id).await?;
    Ok(Json(
        json!({"posting_address":id.posting_address(),"bounces_address":id.bounces_address(),"join_address":id.join_address(),"leave_address":id.leave_address(),"owner_address":id.owner_address(),"request_address":id.request_address()}),
    ))
}
#[utoipa::path(
    get, path = "/api/v1/lists/{id}/templates",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn list_templates(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = id.parse()?;
    authorize_list(&s, &h, peer(c), "lists:read", &id).await?;
    Ok(Json(page(s.flavor, s.db.lists().templates(&id).await?)))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct MemberInput {
    list_id: ListId,
    subscriber: String,
    #[serde(default = "member_role")]
    role: MemberRole,
    #[serde(default)]
    display_name: String,
    #[serde(flatten)]
    confirmation: ConfirmationInput,
    #[serde(flatten)]
    workflow: WorkflowInput,
}
#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
struct ConfirmationInput {
    #[serde(default, deserialize_with = "mailman_bool")]
    pre_verified: bool,
    #[serde(default, deserialize_with = "mailman_bool")]
    pre_confirmed: bool,
    #[serde(default, deserialize_with = "mailman_bool")]
    pre_approved: bool,
}

fn mailman_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct BoolVisitor;

    impl serde::de::Visitor<'_> for BoolVisitor {
        type Value = bool;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a boolean or case-insensitive boolean string")
        }

        fn visit_bool<E>(self, value: bool) -> Result<bool, E> {
            Ok(value)
        }

        fn visit_str<E>(self, value: &str) -> Result<bool, E>
        where
            E: serde::de::Error,
        {
            match value.to_ascii_lowercase().as_str() {
                "true" => Ok(true),
                "false" => Ok(false),
                _ => Err(E::invalid_value(serde::de::Unexpected::Str(value), &self)),
            }
        }
    }

    deserializer.deserialize_any(BoolVisitor)
}

async fn member_value(state: &AppState, member: &listmngr_core::Member) -> Result<Value, Error> {
    let mut value = serde_json::to_value(member).expect("member serializes");
    if matches!(state.flavor, ApiFlavor::Compat31) {
        let email = state
            .db
            .addresses()
            .get_by_id(member.address_id)
            .await?
            .email;
        let object = value.as_object_mut().expect("member is an object");
        object.insert("email".into(), json!(email));
        object.insert("address".into(), json!(format!("/3.1/addresses/{email}")));
        object.insert(
            "self_link".into(),
            json!(format!("/3.1/members/{}", member.id)),
        );
        object.insert("member_id".into(), json!(member.id));
    }
    Ok(value)
}
#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
struct WorkflowInput {
    #[serde(default)]
    invitation: bool,
}
#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct MassMemberRow {
    subscriber: String,
}
#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct MassMemberInput {
    operation: String,
    list_id: ListId,
    members: Vec<MassMemberRow>,
}
#[utoipa::path(
    post, path = "/api/v1/members/mass",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn members_mass(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    Json(v): Json<MassMemberInput>,
) -> ApiResult<impl IntoResponse> {
    authorize_list(&s, &h, peer(c), "members:write", &v.list_id).await?;
    if !matches!(v.operation.as_str(), "subscribe" | "unsubscribe" | "sync") {
        return Err(ApiError(Error::Validation(
            "operation must be subscribe, unsubscribe, or sync".into(),
        )));
    }

    let emails = v
        .members
        .iter()
        .map(|row| row.subscriber.clone())
        .collect::<Vec<_>>();
    let count =
        s.db.members()
            .mass(&v.list_id, &v.operation, &emails)
            .await?;
    Ok((StatusCode::OK, Json(json!({"processed":count}))))
}
const fn member_role() -> MemberRole {
    MemberRole::Member
}
#[utoipa::path(
    post, path = "/api/v1/members",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn members_create(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(v): JsonOrForm<MemberInput>,
) -> ApiResult<Response> {
    authorize_list(&s, &h, peer(c), "members:write", &v.list_id).await?;
    if v.workflow.invitation
        || !v.confirmation.pre_verified
        || !v.confirmation.pre_confirmed
        || !v.confirmation.pre_approved
    {
        return Err(ApiError(Error::Validation(
            "Phase 1 subscriptions require pre_verified, pre_confirmed and pre_approved".into(),
        )));
    }
    let member =
        s.db.members()
            .create(NewMember {
                list_id: v.list_id,
                email: v.subscriber.clone(),
                role: v.role,
                subscription_mode: SubscriptionMode::AsAddress,
                display_name: v.display_name,
            })
            .await?;
    s.db.addresses().verify(&v.subscriber, true).await?;
    let value = member_value(&s, &member).await?;
    let response = match s.flavor {
        ApiFlavor::Compat31 => (
            StatusCode::CREATED,
            [(header::LOCATION, format!("/3.1/members/{}", member.id))],
            Json(value),
        )
            .into_response(),
        ApiFlavor::V1 => (StatusCode::CREATED, Json(value)).into_response(),
    };
    Ok(response)
}
#[utoipa::path(
    get, path = "/api/v1/lists/{id}/roster/{role}",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn roster(
    State(s): State<AppState>,
    Path((id, role)): Path<(String, String)>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = id.parse()?;
    authorize_list(&s, &h, peer(c), "members:read", &id).await?;
    let members = s.db.members().roster(&id, role.parse()?).await?;
    let mut entries = Vec::with_capacity(members.len());
    for member in members {
        entries.push(member_value(&s, &member).await?);
    }
    Ok(Json(page(s.flavor, entries)))
}
#[utoipa::path(
    get, path = "/api/v1/lists/{id}/member/{email}",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn list_member(
    State(s): State<AppState>,
    Path((id, email)): Path<(String, String)>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = id.parse()?;
    authorize_list(&s, &h, peer(c), "members:read", &id).await?;
    let values = s.db.members().find(&email).await?;
    Ok(Json(
        serde_json::to_value(
            values
                .iter()
                .find(|member| member.list_id == id)
                .ok_or(ApiError(Error::NotFound(email)))?,
        )
        .expect("serialize"),
    ))
}
#[utoipa::path(
    get, path = "/api/v1/members/{id}",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn members_get(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = id
        .parse()
        .map_err(|_| ApiError(Error::Validation("member id".into())))?;
    let (_, member) = authorize_member(&s, &h, peer(c), "members:read", id).await?;
    Ok(Json(member_value(&s, &member).await?))
}
#[utoipa::path(
    delete, path = "/api/v1/members/{id}",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn members_delete(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<StatusCode> {
    let id = id
        .parse()
        .map_err(|_| ApiError(Error::Validation("member id".into())))?;
    authorize_member(&s, &h, peer(c), "members:write", id).await?;
    s.db.members().delete(id).await?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(
    patch, path = "/api/v1/members/{id}",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn member_patch(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    Json(v): Json<Value>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "members:write").await?;
    let id: MemberId = id
        .parse()
        .map_err(|_| ApiError(Error::Validation("member id".into())))?;
    authorize_member(&s, &h, peer(c), "members:write", id).await?;
    let member = s.db.members().update(id, &v).await?;
    let preferences = s.db.preferences().get(member.preferences_id).await?;
    let mut value = serde_json::to_value(member).expect("serialize");
    let preference_value = serde_json::to_value(preferences).expect("serialize");
    if let (Some(target), Some(source)) = (value.as_object_mut(), preference_value.as_object()) {
        target.extend(source.clone());
    }
    Ok(Json(value))
}
#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct FindInput {
    subscriber: String,
}
#[utoipa::path(
    post, path = "/api/v1/members/find",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn members_find(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    Json(v): Json<FindInput>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "members:read").await?;
    Ok(Json(page(
        s.flavor,
        s.db.members().find(&v.subscriber).await?,
    )))
}
#[utoipa::path(
    get, path = "/api/v1/members/{id}/preferences",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn member_preferences(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id: MemberId = id
        .parse()
        .map_err(|_| ApiError(Error::Validation("member id".into())))?;
    let (_, member) = authorize_member(&s, &h, peer(c), "members:read", id).await?;
    Ok(Json(
        serde_json::to_value(s.db.preferences().get(member.preferences_id).await?)
            .expect("serialize"),
    ))
}
#[utoipa::path(
    get, path = "/api/v1/members/{id}/all/preferences",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn member_all_preferences(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id: MemberId = id
        .parse()
        .map_err(|_| ApiError(Error::Validation("member id".into())))?;
    authorize_member(&s, &h, peer(c), "members:read", id).await?;
    Ok(Json(
        serde_json::to_value(
            s.db.preferences()
                .resolve_member(id, &s.config.site.default_language)
                .await?,
        )
        .expect("serialize"),
    ))
}
#[utoipa::path(
    put, path = "/api/v1/members/{id}/preferences",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn member_preferences_put(
    state: State<AppState>,
    path: Path<String>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    body: Json<Preferences>,
) -> ApiResult<Json<Value>> {
    member_preferences_write(state, path, headers, connect, body).await
}
#[utoipa::path(
    patch, path = "/api/v1/members/{id}/preferences",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn member_preferences_patch(
    state: State<AppState>,
    path: Path<String>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    body: Json<Preferences>,
) -> ApiResult<Json<Value>> {
    member_preferences_write(state, path, headers, connect, body).await
}
async fn member_preferences_write(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    Json(v): Json<Preferences>,
) -> ApiResult<Json<Value>> {
    let id: MemberId = id
        .parse()
        .map_err(|_| ApiError(Error::Validation("member id".into())))?;
    let (_, member) = authorize_member(&s, &h, peer(c), "members:write", id).await?;
    s.db.preferences().set_member(id, v).await?;
    Ok(Json(
        serde_json::to_value(s.db.preferences().get(member.preferences_id).await?)
            .expect("serialize"),
    ))
}

#[utoipa::path(
    get, path = "/api/v1/users",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn users_list(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "system:read").await?;
    Ok(Json(page(s.flavor, s.db.users().list().await?)))
}
#[utoipa::path(
    post, path = "/api/v1/users",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn users_create(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    Json(v): Json<NewUser>,
) -> ApiResult<impl IntoResponse> {
    authorize(&s, &h, peer(c), "users:write").await?;
    Ok((StatusCode::CREATED, Json(s.db.users().create(v).await?)))
}
#[utoipa::path(
    get, path = "/api/v1/users/{id}",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn users_get(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "system:read").await?;
    let id: UserId = id
        .parse()
        .map_err(|_| ApiError(Error::Validation("user id".into())))?;
    Ok(Json(
        serde_json::to_value(s.db.users().get(id).await?).expect("serialize"),
    ))
}
#[utoipa::path(
    delete, path = "/api/v1/users/{id}",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn users_delete(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<StatusCode> {
    authorize(&s, &h, peer(c), "users:write").await?;
    s.db.users()
        .delete(
            id.parse()
                .map_err(|_| ApiError(Error::Validation("user id".into())))?,
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(
    patch, path = "/api/v1/users/{id}",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn users_patch(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    Json(v): Json<Value>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "users:write").await?;
    let id: UserId = id
        .parse()
        .map_err(|_| ApiError(Error::Validation("user id".into())))?;
    Ok(Json(
        serde_json::to_value(s.db.users().update(id, &v).await?).expect("serialize"),
    ))
}
#[utoipa::path(
    get, path = "/api/v1/users/{id}/preferences",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn user_preferences(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "system:read").await?;
    let id = id
        .parse()
        .map_err(|_| ApiError(Error::Validation("user id".into())))?;
    Ok(Json(
        serde_json::to_value(s.db.preferences().get_user(id).await?).expect("serialize"),
    ))
}
#[utoipa::path(
    get, path = "/api/v1/users/{id}/all/preferences",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn user_all_preferences(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "system:read").await?;
    let id = id
        .parse()
        .map_err(|_| ApiError(Error::Validation("user id".into())))?;
    Ok(Json(
        serde_json::to_value(
            s.db.preferences()
                .resolve_user(id, &s.config.site.default_language)
                .await?,
        )
        .expect("serialize"),
    ))
}
#[utoipa::path(
    put, path = "/api/v1/users/{id}/preferences",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn user_preferences_put(
    state: State<AppState>,
    path: Path<String>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    body: Json<Preferences>,
) -> ApiResult<Json<Value>> {
    user_preferences_write(state, path, headers, connect, body).await
}
#[utoipa::path(
    patch, path = "/api/v1/users/{id}/preferences",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn user_preferences_patch(
    state: State<AppState>,
    path: Path<String>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    body: Json<Preferences>,
) -> ApiResult<Json<Value>> {
    user_preferences_write(state, path, headers, connect, body).await
}
async fn user_preferences_write(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    Json(v): Json<Preferences>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "users:write").await?;
    s.db.preferences()
        .set_user(
            id.parse()
                .map_err(|_| ApiError(Error::Validation("user id".into())))?,
            v.clone(),
        )
        .await?;
    Ok(Json(serde_json::to_value(v).expect("serialize")))
}
#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct LoginInput {
    password: String,
}
#[utoipa::path(
    post, path = "/api/v1/users/{id}/login",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn user_login(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    Json(v): Json<LoginInput>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "users:write").await?;
    let valid =
        s.db.users()
            .verify_password(
                id.parse()
                    .map_err(|_| ApiError(Error::Validation("user id".into())))?,
                &v.password,
            )
            .await?;
    Ok(Json(json!({"success":valid})))
}
#[utoipa::path(
    get, path = "/api/v1/users/{id}/addresses",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn user_addresses(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "system:read").await?;
    let user =
        s.db.users()
            .get(
                id.parse()
                    .map_err(|_| ApiError(Error::Validation("user id".into())))?,
            )
            .await?;
    Ok(Json(json!({"entries":[user.preferred_address_id]})))
}
#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct LinkInput {
    email: String,
}
#[utoipa::path(
    post, path = "/api/v1/users/{id}/addresses",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn user_address_link(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    Json(v): Json<LinkInput>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "users:write").await?;
    Ok(Json(
        serde_json::to_value(
            s.db.addresses()
                .link(
                    &v.email,
                    Some(
                        id.parse()
                            .map_err(|_| ApiError(Error::Validation("user id".into())))?,
                    ),
                )
                .await?,
        )
        .expect("serialize"),
    ))
}

#[utoipa::path(
    get, path = "/api/v1/addresses/{email}",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn address_get(
    State(s): State<AppState>,
    Path(email): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "system:read").await?;
    Ok(Json(
        serde_json::to_value(s.db.addresses().get(&email).await?).expect("serialize"),
    ))
}
#[utoipa::path(
    post, path = "/api/v1/addresses/{email}/verify",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn address_verify(
    State(s): State<AppState>,
    Path(email): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "users:write").await?;
    Ok(Json(
        serde_json::to_value(s.db.addresses().verify(&email, true).await?).expect("serialize"),
    ))
}
#[utoipa::path(
    post, path = "/api/v1/addresses/{email}/unverify",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn address_unverify(
    State(s): State<AppState>,
    Path(email): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "users:write").await?;
    Ok(Json(
        serde_json::to_value(s.db.addresses().verify(&email, false).await?).expect("serialize"),
    ))
}
#[utoipa::path(
    get, path = "/api/v1/addresses/{email}/user",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn address_user(
    State(s): State<AppState>,
    Path(email): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "system:read").await?;
    let address = s.db.addresses().get(&email).await?;
    Ok(Json(json!({"user_id":address.user_id})))
}
#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct UserLink {
    user_id: UserId,
}
#[utoipa::path(
    post, path = "/api/v1/addresses/{email}/user",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn address_link(
    State(s): State<AppState>,
    Path(email): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    Json(v): Json<UserLink>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "users:write").await?;
    Ok(Json(
        serde_json::to_value(s.db.addresses().link(&email, Some(v.user_id)).await?)
            .expect("serialize"),
    ))
}
#[utoipa::path(
    delete, path = "/api/v1/addresses/{email}/user",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn address_unlink(
    State(s): State<AppState>,
    Path(email): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<StatusCode> {
    authorize(&s, &h, peer(c), "users:write").await?;
    s.db.addresses().link(&email, None).await?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(
    get, path = "/api/v1/addresses/{email}/memberships",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn address_memberships(
    State(s): State<AppState>,
    Path(email): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "members:read").await?;
    Ok(Json(page(s.flavor, s.db.members().find(&email).await?)))
}
#[utoipa::path(
    get, path = "/api/v1/addresses/{email}/preferences",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn address_preferences(
    State(s): State<AppState>,
    Path(email): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "system:read").await?;
    Ok(Json(
        serde_json::to_value(s.db.preferences().get_address(&email).await?).expect("serialize"),
    ))
}
#[utoipa::path(
    get, path = "/api/v1/addresses/{email}/all/preferences",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn address_all_preferences(
    State(s): State<AppState>,
    Path(email): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "system:read").await?;
    Ok(Json(
        serde_json::to_value(
            s.db.preferences()
                .resolve_address(&email, &s.config.site.default_language)
                .await?,
        )
        .expect("serialize"),
    ))
}
#[utoipa::path(
    put, path = "/api/v1/addresses/{email}/preferences",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn address_preferences_put(
    state: State<AppState>,
    path: Path<String>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    body: Json<Preferences>,
) -> ApiResult<Json<Value>> {
    address_preferences_write(state, path, headers, connect, body).await
}
#[utoipa::path(
    patch, path = "/api/v1/addresses/{email}/preferences",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn address_preferences_patch(
    state: State<AppState>,
    path: Path<String>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    body: Json<Preferences>,
) -> ApiResult<Json<Value>> {
    address_preferences_write(state, path, headers, connect, body).await
}
async fn address_preferences_write(
    State(s): State<AppState>,
    Path(email): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    Json(v): Json<Preferences>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "users:write").await?;
    s.db.preferences().set_address(&email, v.clone()).await?;
    Ok(Json(serde_json::to_value(v).expect("serialize")))
}
#[utoipa::path(
    get, path = "/api/v1/owners",
    responses((status = 200, description = "Successful operation", body = Value), (status = 401, description = "Authentication required", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn owners(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "system:read").await?;
    Ok(Json(page(
        s.flavor,
        s.db.users()
            .list()
            .await?
            .into_iter()
            .filter(|u| u.is_server_owner)
            .collect::<Vec<_>>(),
    )))
}
