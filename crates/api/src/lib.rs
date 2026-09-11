#![forbid(unsafe_code)]

//! Axum REST API shared by Mailman-compatible `/3.1` and typed `/api/v1` routes.

mod archive;
mod bans;
mod bounce_config;
mod bounces;

use axum::{
    Json, Router,
    body::{Body, Bytes, to_bytes},
    extract::{ConnectInfo, FromRequest, Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use base64::Engine;
use dashmap::DashMap;
use listmngr_core::{
    Config, DeliveryMode, DeliveryStatus, Error, ListId, MemberId, MemberRole, Preferences,
    SubscriptionMode, UserId, builtin_styles,
};
use listmngr_db::{AuditContext, Database, NewList, NewMember, NewUser, TokenAuth};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};
use utoipa::OpenApi;
mod templates;
mod webui;
mod workflows;

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

macro_rules! page_response {
    ($name:ident, $item:ty) => {
        #[derive(Debug, Serialize, utoipa::ToSchema)]
        pub struct $name {
            pub items: Vec<$item>,
            pub next_cursor: Option<String>,
            pub total: usize,
            pub start: usize,
            pub count: usize,
        }
    };
}

page_response!(StringPageResponse, String);
page_response!(BanPageResponse, bans::BanResponse);
page_response!(BouncePageResponse, listmngr_db::bounces::BounceEvent);
page_response!(CatalogPageResponse, CatalogEntry);
page_response!(DomainPageResponse, listmngr_core::Domain);
page_response!(MailingListPageResponse, listmngr_core::MailingList);
page_response!(UserPageResponse, listmngr_core::User);
page_response!(ArchiverPageResponse, ArchiverResponse);
page_response!(TemplatePageResponse, listmngr_db::Template);
page_response!(MemberPageResponse, listmngr_core::Member);
page_response!(AddressPageResponse, listmngr_core::Address);

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct CatalogEntry {
    pub name: String,
    pub phase: String,
    pub executable: bool,
    pub status: String,
}

#[derive(Debug, Default, Deserialize, utoipa::IntoParams, utoipa::ToSchema)]
#[into_params(parameter_in = Query)]
pub struct PageQuery {
    /// Opaque offset cursor returned by the previous native page.
    pub cursor: Option<String>,
    /// One-based compatibility page number. Cannot be combined with cursor.
    pub page: Option<usize>,
    /// Maximum entries to return (1..=100, default 50).
    pub count: Option<usize>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct SystemVersionsResponse {
    pub listmngr_version: String,
    pub api_version: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ConfigurationResponse {
    pub sections: std::collections::HashMap<String, String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct UriResponse {
    pub self_link: Option<String>,
    pub posting_address: Option<String>,
    pub bounces_address: Option<String>,
    pub join_address: Option<String>,
    pub leave_address: Option<String>,
    pub owner_address: Option<String>,
    pub request_address: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
// Mirrors independent persisted configuration switches on MailingList.
#[allow(clippy::struct_excessive_bools)]
pub struct ListConfigResponse {
    pub default_member_action: Option<listmngr_core::ModerationAction>,
    pub default_nonmember_action: Option<listmngr_core::ModerationAction>,
    pub display_name: String,
    pub description: String,
    pub info: String,
    pub subject_prefix: String,
    pub advertised: bool,
    pub preferred_language: String,
    pub anonymous_list: bool,
    pub send_welcome_message: bool,
    pub send_goodbye_message: bool,
    pub process_bounces: bool,
    #[schema(default = true)]
    pub bounce_notify_owner_on_disable: bool,
    #[schema(default = false)]
    pub bounce_notify_owner_on_bounce_increment: bool,
    #[schema(default = true)]
    pub bounce_notify_owner_on_removal: bool,
    #[schema(minimum = 0, maximum = 100, default = 3)]
    pub bounce_you_are_disabled_warnings: u32,
    #[schema(minimum = 0, maximum = 36500, default = 7)]
    pub bounce_you_are_disabled_warnings_interval: u32,
    #[schema(minimum = 1, maximum = 3650, default = 7)]
    pub bounce_info_stale_after: u32,
    #[schema(exclusive_minimum = 0, maximum = 1_000_000, default = 5)]
    pub bounce_score_threshold: f64,
    #[serde(flatten)]
    pub dmarc: listmngr_core::DmarcSettings,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_post_at: Option<chrono::DateTime<chrono::Utc>>,
    pub post_id: i64,
    pub volume: i32,
    pub next_digest_number: i64,
    pub digest_last_sent_at: Option<chrono::DateTime<chrono::Utc>>,
    pub emergency: bool,
    /// Original post size limit in KiB; zero disables the per-list limit.
    pub max_message_size: u32,
    /// Hold at or above this visible To/Cc mailbox count; zero disables.
    #[schema(minimum = 0, maximum = 2_147_483_647)]
    pub max_num_recipients: u32,
    pub archive_policy: listmngr_core::ArchivePolicy,
    pub archive_rendering_mode: listmngr_core::ArchiveRenderingMode,
    pub style_name: String,
    /// Hold short posts that look like email commands.
    #[schema(default = true)]
    pub administrivia: bool,
    /// Hold posts whose visible To/Cc names neither the list nor an alias.
    #[schema(default = true)]
    pub require_explicit_destination: bool,
    /// Exact addresses or `^`-anchored regexes counted as explicit destinations.
    pub acceptable_aliases: Vec<String>,
    /// Legacy nonmember action lists; exact addresses or `^`-anchored regexes.
    pub accept_these_nonmembers: Vec<String>,
    pub hold_these_nonmembers: Vec<String>,
    pub reject_these_nonmembers: Vec<String>,
    pub discard_these_nonmembers: Vec<String>,
    /// Name of the handler pipeline an accepted post runs.
    #[schema(default = "default-posting-pipeline")]
    pub posting_pipeline: String,
    /// Tell the poster when their post is held for moderation.
    #[schema(default = true)]
    pub respond_to_post_requests: bool,
    /// Tell owners and moderators immediately when a post is held.
    #[schema(default = true)]
    pub admin_immed_notify: bool,
    pub mail_host: String,
    pub list_name: String,
    pub fqdn_listname: String,
    pub list_id: ListId,
    pub posting_address: String,
    pub bounces_address: String,
    pub join_address: String,
    pub leave_address: String,
    pub owner_address: String,
    pub request_address: String,
    pub no_reply_address: String,
}

#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ListConfigInput {
    pub default_member_action: Option<listmngr_core::ModerationAction>,
    pub default_nonmember_action: Option<listmngr_core::ModerationAction>,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub info: Option<String>,
    pub subject_prefix: Option<String>,
    pub advertised: Option<bool>,
    pub preferred_language: Option<String>,
    pub anonymous_list: Option<bool>,
    /// Opt-in built-in private welcome for new Member subscriptions; default false.
    pub send_welcome_message: Option<bool>,
    /// Opt-in built-in private goodbye for actual Member removal; default false.
    pub send_goodbye_message: Option<bool>,
    pub process_bounces: Option<bool>,
    #[schema(default = true)]
    pub bounce_notify_owner_on_disable: Option<bool>,
    #[schema(default = false)]
    pub bounce_notify_owner_on_bounce_increment: Option<bool>,
    #[schema(default = true)]
    pub bounce_notify_owner_on_removal: Option<bool>,
    #[schema(minimum = 0, maximum = 100, default = 3)]
    pub bounce_you_are_disabled_warnings: Option<u32>,
    #[schema(minimum = 0, maximum = 36500, default = 7)]
    pub bounce_you_are_disabled_warnings_interval: Option<u32>,
    #[schema(minimum = 1, maximum = 3650, default = 7)]
    pub bounce_info_stale_after: Option<u32>,
    #[schema(exclusive_minimum = 0, maximum = 1_000_000, default = 5)]
    pub bounce_score_threshold: Option<f64>,
    /// `munge_from` requires unconditional=true; conditional DNS is unsupported.
    pub dmarc_mitigate_action: Option<listmngr_core::DmarcMitigateAction>,
    pub dmarc_mitigate_unconditionally: Option<bool>,
    pub next_digest_number: Option<i64>,
    pub emergency: Option<bool>,
    /// Nonnegative KiB, at most 2147483647; zero disables the per-list limit.
    pub max_message_size: Option<u32>,
    /// Hold at or above this visible To/Cc mailbox count; zero disables.
    #[schema(minimum = 0, maximum = 2_147_483_647)]
    pub max_num_recipients: Option<u32>,
    pub archive_policy: Option<listmngr_core::ArchivePolicy>,
    pub archive_rendering_mode: Option<listmngr_core::ArchiveRenderingMode>,
    /// Hold short posts that look like email commands; default true.
    pub administrivia: Option<bool>,
    /// Hold posts whose visible To/Cc names neither the list nor an alias; default true.
    pub require_explicit_destination: Option<bool>,
    /// Exact addresses or `^`-anchored regexes counted as explicit destinations.
    pub acceptable_aliases: Option<Vec<String>>,
    /// Legacy nonmember action lists; exact addresses or `^`-anchored regexes.
    pub accept_these_nonmembers: Option<Vec<String>>,
    pub hold_these_nonmembers: Option<Vec<String>>,
    pub reject_these_nonmembers: Option<Vec<String>>,
    pub discard_these_nonmembers: Option<Vec<String>>,
    /// Write-only `Approved:` posting key, stored as Argon2id; empty clears it.
    #[schema(write_only)]
    pub moderator_password: Option<String>,
    /// Name of a registered handler pipeline; see `/system/pipelines`.
    pub posting_pipeline: Option<String>,
    /// Tell the poster when their post is held; default true.
    pub respond_to_post_requests: Option<bool>,
    /// Tell owners and moderators immediately when a post is held; default true.
    pub admin_immed_notify: Option<bool>,
}

#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemberPatchInput {
    pub display_name: Option<String>,
    pub delivery_mode: Option<DeliveryMode>,
    pub delivery_status: Option<DeliveryStatus>,
}

#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UserPatchInput {
    pub display_name: Option<String>,
    pub locale: Option<String>,
    pub timezone: Option<String>,
    pub is_server_owner: Option<bool>,
}

#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EmptyMutationInput {}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ProcessedResponse {
    pub processed: usize,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct LoginResponse {
    pub success: bool,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct AddressUserResponse {
    pub user_id: Option<UserId>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub enum ListConfigAttributeValue {
    Text(String),
    Boolean(bool),
    Integer(i64),
    Number(f64),
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ArchiverResponse {
    pub name: String,
    pub enabled: bool,
}

struct SecurityAddon;
impl utoipa::Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        use utoipa::openapi::header::Header;
        use utoipa::openapi::path::Operation;
        use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
        use utoipa::openapi::{Object, RefOr, Type};

        fn add_retry_after(operation: &mut Option<Operation>) {
            let Some(operation) = operation else {
                return;
            };
            let Some(RefOr::T(response)) = operation.responses.responses.get_mut("429") else {
                return;
            };
            let mut seconds = Object::with_type(Type::Integer);
            seconds.minimum = Some(1.into());
            response
                .headers
                .insert("Retry-After".into(), Header::new(seconds));
        }

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

        for item in openapi.paths.paths.values_mut() {
            add_retry_after(&mut item.get);
            add_retry_after(&mut item.post);
            add_retry_after(&mut item.put);
            add_retry_after(&mut item.patch);
            add_retry_after(&mut item.delete);
        }
    }
}

#[derive(OpenApi)]
#[openapi(
    info(title = "listmngr API", version = "0.1.0"),
    paths(
        bans::list,
        bounces::list,
        bans::get,
        bans::create,
        bans::delete,
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
        templates::list_uris,
        templates::patch_list_uris,
        templates::put_list_uris,
        templates::delete_list_uris,
        templates::get_list_uri,
        templates::set_list_uri,
        templates::delete_list_uri,
        templates::domain_uris,
        templates::patch_domain_uris,
        templates::put_domain_uris,
        templates::delete_domain_uris,
        templates::get_domain_uri,
        templates::set_domain_uri,
        templates::delete_domain_uri,
        templates::site_uris,
        templates::patch_site_uris,
        templates::put_site_uris,
        templates::delete_site_uris,
        templates::get_site_uri,
        templates::set_site_uri,
        templates::delete_site_uri,
        templates::put_list_template_body,
        templates::delete_list_template_body,
        list_templates,
        roster,
        list_member,
        list_member_delete,
        list_held,
        list_held_count,
        list_held_get,
        list_held_moderate,
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
    components(schemas(
        ErrorResponse, StringPageResponse, CatalogPageResponse, CatalogEntry, PageQuery,
        DomainPageResponse, MailingListPageResponse,
        UserPageResponse, ArchiverPageResponse, TemplatePageResponse, MemberPageResponse,
        AddressPageResponse, SystemVersionsResponse, ConfigurationResponse, UriResponse,
        ListConfigResponse, ListConfigInput, ListConfigAttributeValue, MemberPatchInput,
        UserPatchInput, EmptyMutationInput, ProcessedResponse, LoginResponse, AddressUserResponse,
        ArchiverResponse, templates::TemplateUriResponse, templates::TemplateUriPageResponse, templates::TemplateBodyInput, templates::TemplateUrisInput, templates::TemplateUriInput,
        DomainInput, ListQuery, ListInput, MemberInput, ConfirmationInput, WorkflowInput,
        MassMemberRow, MassMemberInput, FindInput, LoginInput, LinkInput, UserLink,
        Preferences, listmngr_core::Domain, listmngr_core::MailingList, listmngr_core::Member,
        listmngr_core::User, listmngr_core::Address, listmngr_db::NewUser,
        listmngr_db::NewList, listmngr_db::NewMember, listmngr_db::MemberMassResult,
        listmngr_db::Template,
        HeldMessageResponse, HeldMessagePageResponse, CountResponse, ModerateInput
    )),
    modifiers(&SecurityAddon)
)]
struct ApiDoc;

#[derive(Debug, Clone)]
pub struct AppState {
    db: Database,
    config: Config,
    pre_auth_rate: Arc<RateLimiter>,
    post_auth_rate: Arc<RateLimiter>,
    web_login_rate: Arc<RateLimiter>,
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
    window: Duration,
    hits: DashMap<String, (Instant, u32)>,
}
impl RateLimiter {
    fn from_config(spec: &str) -> Self {
        let (limit, unit) = spec.split_once('/').unwrap_or_else(|| {
            panic!("invalid security.rate_limit.api `{spec}`: expected COUNT/WINDOW")
        });
        let limit = limit
            .parse::<u32>()
            .ok()
            .filter(|limit| *limit > 0)
            .unwrap_or_else(|| {
                panic!("invalid security.rate_limit.api `{spec}`: count must be positive")
            });
        let window = match unit {
            "s" | "sec" | "second" => Duration::from_secs(1),
            "m" | "min" | "minute" => Duration::from_secs(60),
            "h" | "hour" => Duration::from_secs(60 * 60),
            "d" | "day" => Duration::from_secs(24 * 60 * 60),
            _ => panic!("invalid security.rate_limit.api `{spec}`: unsupported window"),
        };
        Self {
            limit,
            window,
            hits: DashMap::new(),
        }
    }

    fn check(&self, key: &str) -> Result<(), u64> {
        let now = Instant::now();
        let mut hit = self.hits.entry(key.to_owned()).or_insert((now, 0));
        if now.duration_since(hit.0) >= self.window {
            *hit = (now, 0);
        }
        hit.1 += 1;
        if hit.1 <= self.limit {
            Ok(())
        } else {
            Err(self
                .window
                .saturating_sub(now.duration_since(hit.0))
                .as_secs()
                .max(1))
        }
    }
}

#[derive(Debug)]
struct ApiError(Error);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let retry_after = match &self.0 {
            Error::RateLimited { retry_after } => Some(*retry_after),
            _ => None,
        };
        let status = match &self.0 {
            Error::Authentication => StatusCode::UNAUTHORIZED,
            Error::Forbidden(_) => StatusCode::FORBIDDEN,
            Error::NotFound(_) => StatusCode::NOT_FOUND,
            Error::Conflict(_) => StatusCode::CONFLICT,
            Error::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            Error::Validation(_) | Error::InvalidListId(_) => StatusCode::BAD_REQUEST,
            Error::Config(_) | Error::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let code = match &self.0 {
            Error::Authentication => "authentication",
            Error::Forbidden(_) => "forbidden",
            Error::NotFound(_) => "not_found",
            Error::Conflict(_) => "conflict",
            Error::RateLimited { .. } => "rate_limited",
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
        let mut response = (
            status,
            Json(ErrorResponse {
                code,
                correlation_id,
                title,
                detail,
            }),
        )
            .into_response();
        if let Some(retry_after) = retry_after {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, retry_after.into());
        }
        response
    }
}
impl From<Error> for ApiError {
    fn from(value: Error) -> Self {
        Self(value)
    }
}
type ApiResult<T> = Result<T, ApiError>;

pub fn router(db: Database, config: Config) -> Router {
    let pre_auth_rate = RateLimiter::from_config(
        config
            .security
            .rate_limit
            .api_pre_auth
            .as_deref()
            .unwrap_or(&config.security.rate_limit.api),
    );
    let post_auth_rate = RateLimiter::from_config(&config.security.rate_limit.api);
    let state = AppState {
        db,
        config,
        pre_auth_rate: Arc::new(pre_auth_rate),
        post_auth_rate: Arc::new(post_auth_rate),
        web_login_rate: Arc::new(RateLimiter::from_config("5/min")),
        flavor: ApiFlavor::V1,
    };
    let mut compat_state = state.clone();
    compat_state.flavor = ApiFlavor::Compat31;
    Router::new()
        .merge(archive::routes())
        .route("/healthz", get(health))
        .merge(workflows::routes())
        .route("/readyz", get(ready))
        .route("/metrics", get(metrics))
        .route("/openapi.json", get(openapi))
        .route("/api/docs", get(swagger))
        .nest("/3.1", phase_one_routes().with_state(compat_state))
        .nest(
            "/api/v1",
            phase_one_routes()
                .layer(middleware::from_fn(typed_etag))
                .with_state(state.clone()),
        )
        .merge(webui::routes())
        .layer(middleware::from_fn(trace_request))
        .with_state(state)
}

async fn typed_etag(request: Request, next: Next) -> Response {
    let is_get = request.method() == Method::GET;
    let response = next.run(request).await;
    if !is_get || !response.status().is_success() || response.headers().contains_key(header::ETAG) {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    let Ok(bytes) = to_bytes(body, usize::MAX).await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let digest = base64::engine::general_purpose::STANDARD_NO_PAD.encode(Sha256::digest(&bytes));
    let value = HeaderValue::from_str(&format!("\"sha256-{digest}\""))
        .expect("base64 SHA-256 is a valid entity tag");
    parts.headers.insert(header::ETAG, value);
    Response::from_parts(parts, Body::from(bytes))
}

async fn trace_request(request: Request, next: Next) -> Response {
    let request_id = uuid::Uuid::now_v7();
    let method = request.method().clone();
    // Query strings can contain browser confirmation credentials.
    let uri = request.uri().path().to_owned();
    tracing::info!(%request_id, %method, %uri, "HTTP request started");
    let mut response = next.run(request).await;
    tracing::info!(%request_id, %method, %uri, status = %response.status(), "HTTP request completed");
    response.headers_mut().insert(
        header::HeaderName::from_static("x-request-id"),
        request_id
            .to_string()
            .parse()
            .expect("UUID is a header value"),
    );
    response
}

fn phase_one_routes() -> Router<AppState> {
    Router::new()
        .merge(templates::routes())
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
        .route("/lists/{id}/bans", get(bans::list).post(bans::create))
        .route("/lists/{id}/bounces", get(bounces::list))
        .route(
            "/lists/{id}/bans/{email}",
            get(bans::get).delete(bans::delete),
        )
        .route("/lists/{id}/templates", get(list_templates))
        .route("/lists/{id}/roster/{role}", get(roster))
        .route(
            "/lists/{id}/member/{email}",
            get(list_member).delete(list_member_delete),
        )
        .route("/lists/{id}/held", get(list_held))
        .route("/lists/{id}/held/count", get(list_held_count))
        .route(
            "/lists/{id}/held/{held_id}",
            get(list_held_get).post(list_held_moderate),
        )
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

async fn authenticate_for_authorization(
    state: &AppState,
    headers: &HeaderMap,
    addr: SocketAddr,
    scope: &str,
) -> ApiResult<TokenAuth> {
    let peer_key = format!("socket-ip:{}", addr.ip());
    state
        .pre_auth_rate
        .check(&peer_key)
        .map_err(|retry_after| ApiError(Error::RateLimited { retry_after }))?;
    let value = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or(ApiError(Error::Authentication))?;
    let token = if let Some(token) = value.strip_prefix("Bearer ") {
        token.to_owned()
    } else if let Some(encoded) = value.strip_prefix("Basic ") {
        if !matches!(state.flavor, ApiFlavor::Compat31) || !state.config.api.compat_basic_auth {
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
    let auth = state.db.tokens().authenticate_without_usage(&token).await?;
    let account_key = format!("account:{}:token:{}", auth.user_id, auth.id);
    state
        .post_auth_rate
        .check(&account_key)
        .map_err(|retry_after| ApiError(Error::RateLimited { retry_after }))?;
    if !auth.has_scope(scope) {
        return Err(ApiError(Error::Forbidden(scope.into())));
    }
    Ok(auth)
}

async fn finish_authorization(state: &AppState, auth: TokenAuth) -> ApiResult<TokenAuth> {
    state.db.tokens().mark_used(auth.id).await?;
    Ok(auth)
}

async fn authorize(
    state: &AppState,
    headers: &HeaderMap,
    addr: SocketAddr,
    scope: &str,
) -> ApiResult<TokenAuth> {
    let auth = authenticate_for_authorization(state, headers, addr, scope).await?;
    finish_authorization(state, auth).await
}

const fn audit_context(auth: &TokenAuth, addr: SocketAddr) -> AuditContext {
    AuditContext::new(Some(auth.user_id), Some(auth.id), Some(addr.ip()))
}

const fn is_unbound(auth: &TokenAuth) -> bool {
    auth.list_id.is_none() && auth.domain_id.is_none()
}

async fn auth_allows_list(state: &AppState, auth: &TokenAuth, list: &ListId) -> ApiResult<bool> {
    let domain = state.db.domains().get(list.mail_host()).await?;
    Ok(auth.allows_list(list, domain.id))
}

async fn filter_members_for_auth(
    state: &AppState,
    auth: &TokenAuth,
    members: Vec<listmngr_core::Member>,
) -> ApiResult<Vec<listmngr_core::Member>> {
    let mut allowed = Vec::new();
    for member in members {
        if auth_allows_list(state, auth, &member.list_id).await? {
            allowed.push(member);
        }
    }
    Ok(allowed)
}

async fn auth_allows_user(state: &AppState, auth: &TokenAuth, user: UserId) -> ApiResult<bool> {
    if is_unbound(auth) {
        return Ok(true);
    }
    let list_ids: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT m.list_id FROM members m JOIN addresses a ON a.id=m.address_id WHERE a.user_id=$1",
    )
    .bind(user.to_string())
    .fetch_all(state.db.pool())
    .await
    .map_err(|error| ApiError(Error::Database(error.to_string())))?;
    for value in list_ids {
        let list: ListId = value.parse()?;
        if auth_allows_list(state, auth, &list).await? {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn auth_allows_address(state: &AppState, auth: &TokenAuth, email: &str) -> ApiResult<bool> {
    if is_unbound(auth) {
        return Ok(true);
    }
    let members = state.db.members().find(email).await?;
    Ok(!filter_members_for_auth(state, auth, members)
        .await?
        .is_empty())
}

async fn authorize_user(
    state: &AppState,
    headers: &HeaderMap,
    addr: SocketAddr,
    scope: &str,
    user: UserId,
) -> ApiResult<TokenAuth> {
    let auth = authenticate_for_authorization(state, headers, addr, scope).await?;
    if (scope == "users:write" && (auth.list_id.is_some() || auth.domain_id.is_some()))
        || !auth_allows_user(state, &auth, user).await?
    {
        return Err(ApiError(Error::Forbidden(scope.into())));
    }
    finish_authorization(state, auth).await
}

async fn authorize_address(
    state: &AppState,
    headers: &HeaderMap,
    addr: SocketAddr,
    scope: &str,
    email: &str,
) -> ApiResult<TokenAuth> {
    let auth = authenticate_for_authorization(state, headers, addr, scope).await?;
    if (scope == "users:write" && (auth.list_id.is_some() || auth.domain_id.is_some()))
        || !auth_allows_address(state, &auth, email).await?
    {
        return Err(ApiError(Error::Forbidden(scope.into())));
    }
    finish_authorization(state, auth).await
}

async fn authorize_user_and_address(
    state: &AppState,
    headers: &HeaderMap,
    addr: SocketAddr,
    scope: &str,
    user: UserId,
    email: &str,
) -> ApiResult<TokenAuth> {
    let auth = authenticate_for_authorization(state, headers, addr, scope).await?;
    if (scope == "users:write" && (auth.list_id.is_some() || auth.domain_id.is_some()))
        || !auth_allows_user(state, &auth, user).await?
        || !auth_allows_address(state, &auth, email).await?
    {
        return Err(ApiError(Error::Forbidden(scope.into())));
    }
    finish_authorization(state, auth).await
}

async fn authorize_admin(
    state: &AppState,
    headers: &HeaderMap,
    addr: SocketAddr,
    scope: &str,
) -> ApiResult<TokenAuth> {
    let auth = authenticate_for_authorization(state, headers, addr, scope).await?;
    if !auth.scopes.contains("admin") || auth.list_id.is_some() || auth.domain_id.is_some() {
        return Err(ApiError(Error::Forbidden("admin".into())));
    }
    finish_authorization(state, auth).await
}

async fn authorize_domain(
    state: &AppState,
    headers: &HeaderMap,
    addr: SocketAddr,
    scope: &str,
    host: &str,
) -> ApiResult<TokenAuth> {
    let auth = authenticate_for_authorization(state, headers, addr, scope).await?;
    let domain = state.db.domains().get(host).await?;
    let list_matches = auth
        .list_id
        .as_ref()
        .is_none_or(|list| list.mail_host() == domain.mail_host);
    if !auth.allows_domain(domain.id) || !list_matches {
        return Err(ApiError(Error::Forbidden(scope.into())));
    }
    finish_authorization(state, auth).await
}
async fn authorize_list(
    state: &AppState,
    headers: &HeaderMap,
    addr: SocketAddr,
    scope: &str,
    list: &ListId,
) -> ApiResult<TokenAuth> {
    let auth = authenticate_for_authorization(state, headers, addr, scope).await?;
    let domain = state.db.domains().get(list.mail_host()).await?;
    if !auth.allows_list(list, domain.id) {
        return Err(ApiError(Error::Forbidden(scope.into())));
    }
    finish_authorization(state, auth).await
}
async fn authorize_member(
    state: &AppState,
    headers: &HeaderMap,
    addr: SocketAddr,
    scope: &str,
    member_id: MemberId,
) -> ApiResult<(TokenAuth, listmngr_core::Member)> {
    let auth = authenticate_for_authorization(state, headers, addr, scope).await?;
    let member = state.db.members().get(member_id).await?;
    let domain = state.db.domains().get(member.list_id.mail_host()).await?;
    if !auth.allows_list(&member.list_id, domain.id) {
        return Err(ApiError(Error::Forbidden(scope.into())));
    }
    Ok((finish_authorization(state, auth).await?, member))
}
const fn peer(connect: ConnectInfo<SocketAddr>) -> SocketAddr {
    connect.0
}
fn paged<T: Serialize>(flavor: ApiFlavor, entries: T, query: &PageQuery) -> Result<Value, Error> {
    let entries = serde_json::to_value(entries).expect("page entries serialize");
    let entries = entries
        .as_array()
        .ok_or_else(|| Error::Validation("page entries must be an array".into()))?;
    let total = entries.len();
    let (start, end) = page_window(total, query)?;
    Ok(page_response(flavor, &entries[start..end], start, total))
}

fn page_window(total: usize, query: &PageQuery) -> Result<(usize, usize), Error> {
    let count = query.count.unwrap_or(50);
    if !(1..=100).contains(&count) {
        return Err(Error::Validation("count must be between 1 and 100".into()));
    }
    if query.cursor.is_some() && query.page.is_some() {
        return Err(Error::Validation(
            "cursor and page cannot be combined".into(),
        ));
    }
    let requested_start = if let Some(cursor) = &query.cursor {
        cursor
            .parse::<usize>()
            .map_err(|_| Error::Validation("invalid cursor".into()))?
    } else if let Some(page) = query.page {
        if page == 0 {
            return Err(Error::Validation("page is one-based".into()));
        }
        (page - 1).saturating_mul(count)
    } else {
        0
    };
    let start = requested_start.min(total);
    let end = start.saturating_add(count).min(total);
    Ok((start, end))
}

fn page_response(flavor: ApiFlavor, selected: &[Value], start: usize, total: usize) -> Value {
    let end = start.saturating_add(selected.len());
    let returned = selected.len();
    let next_cursor = (end < total).then(|| end.to_string());
    match flavor {
        ApiFlavor::Compat31 => json!({
            "entries": selected,
            "start": start,
            "count": returned,
            "total_size": total,
            "http_etag": "phase1"
        }),
        ApiFlavor::V1 => json!({
            "items": selected,
            "next_cursor": next_cursor,
            "total": total,
            "start": start,
            "count": returned
        }),
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
    get,
    path = "/api/v1/system/versions",
    responses((status = 200, description = "Successful operation", body = SystemVersionsResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
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
    get,
    path = "/api/v1/system/configuration",
    responses((status = 200, description = "Successful operation", body = ConfigurationResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
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
    get,
    path = "/api/v1/system/configuration/{section}",
    params(("section" = String, Path, description = "section path parameter")),
    responses((status = 200, description = "Successful operation", body = ConfigurationResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
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
    get,
    path = "/api/v1/system/preferences",
    responses((status = 200, description = "Successful operation", body = Preferences), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
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
    get,
    path = "/api/v1/system/pipelines",
    params(PageQuery),
    responses((status = 200, description = "Successful operation", body = CatalogPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn system_pipelines(
    State(s): State<AppState>,
    Query(page_query): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "system:read").await?;
    let registry = listmngr_mail::handlers::builtin_registry();
    let entries: Vec<Value> = registry
        .pipelines()
        .map(|pipeline| {
            json!({
                "name": pipeline.name(),
                "phase": "phase2",
                "executable": registry.is_executable(pipeline),
                "status": if registry.is_executable(pipeline) { "engine" } else { "declared" },
                "handlers": pipeline.handlers(),
            })
        })
        .collect();
    Ok(Json(paged(s.flavor, entries, &page_query)?))
}
#[utoipa::path(
    get,
    path = "/api/v1/system/chains",
    params(PageQuery),
    responses((status = 200, description = "Successful operation", body = CatalogPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn system_chains(
    State(s): State<AppState>,
    Query(page_query): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "system:read").await?;
    let entries: Vec<Value> = listmngr_pipeline::builtin()
        .chains()
        .map(chain_entry)
        .collect();
    Ok(Json(paged(s.flavor, entries, &page_query)?))
}

/// Project one chain as the engine actually holds it, so operators can see the
/// real link order rather than a name list. A chain that is declared but has no
/// links yet reports `executable: false`.
fn chain_entry(chain: &listmngr_pipeline::Chain) -> Value {
    use listmngr_pipeline::{ChainKind, Link};
    let (status, rules) = match chain.kind() {
        ChainKind::Terminal(_) => ("terminal", Vec::new()),
        ChainKind::Moderation => ("moderation", Vec::new()),
        ChainKind::HeaderMatch => ("header-match", Vec::new()),
        ChainKind::Links(links) if links.is_empty() => ("declared", Vec::new()),
        ChainKind::Links(links) => ("engine", links.iter().map(Link::rule).collect()),
    };
    json!({
        "name": chain.name(),
        "phase": "phase2",
        "executable": chain.is_executable(),
        "status": status,
        "rules": rules
    })
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct DomainInput {
    mail_host: String,
    #[serde(default)]
    description: String,
    alias_domain: Option<String>,
}
#[utoipa::path(
    post,
    path = "/api/v1/domains",
    request_body(content((DomainInput = "application/json"), (DomainInput = "application/x-www-form-urlencoded"))),
    responses((status = 201, description = "Successful operation", body = listmngr_core::Domain), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn domains_create(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(v): JsonOrForm<DomainInput>,
) -> ApiResult<Response> {
    let addr = peer(c);
    let auth = authenticate_for_authorization(&s, &h, addr, "lists:write").await?;
    if !is_unbound(&auth) {
        return Err(ApiError(Error::Forbidden("lists:write".into())));
    }
    let auth = finish_authorization(&s, auth).await?;
    let domain =
        s.db.domains()
            .create_with_context(
                &v.mail_host,
                &v.description,
                v.alias_domain.as_deref(),
                &audit_context(&auth, addr),
            )
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
    get,
    path = "/api/v1/domains",
    params(PageQuery),
    responses((status = 200, description = "Successful operation", body = DomainPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn domains_list(
    State(s): State<AppState>,
    Query(page_query): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let auth = authenticate_for_authorization(&s, &h, peer(c), "lists:read").await?;
    let domains =
        s.db.domains()
            .list()
            .await?
            .into_iter()
            .filter(|domain| {
                auth.allows_domain(domain.id)
                    && auth
                        .list_id
                        .as_ref()
                        .is_none_or(|list| list.mail_host() == domain.mail_host)
            })
            .map(|domain| domain_value(s.flavor, &domain))
            .collect::<Vec<_>>();
    finish_authorization(&s, auth).await?;
    Ok(Json(paged(s.flavor, domains, &page_query)?))
}
#[utoipa::path(
    get,
    path = "/api/v1/domains/{host}",
    params(("host" = String, Path, description = "host path parameter")),
    responses((status = 200, description = "Successful operation", body = listmngr_core::Domain), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
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
    delete,
    path = "/api/v1/domains/{host}",
    params(("host" = String, Path, description = "host path parameter")),
    responses((status = 204, description = "Deleted"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn domains_delete(
    State(s): State<AppState>,
    Path(host): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<StatusCode> {
    let addr = peer(c);
    let auth = authorize_domain(&s, &h, addr, "lists:write", &host).await?;
    s.db.domains()
        .delete_with_context(&host, &audit_context(&auth, addr))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(
    get,
    path = "/api/v1/domains/{host}/lists",
    params(PageQuery, ("host" = String, Path, description = "host path parameter")),
    responses((status = 200, description = "Successful operation", body = MailingListPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn domain_lists(
    State(s): State<AppState>,
    Path(host): Path<String>,
    Query(page_query): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let auth = authenticate_for_authorization(&s, &h, peer(c), "lists:read").await?;
    let domain = s.db.domains().get(&host).await?;
    if !auth.allows_domain(domain.id) {
        return Err(ApiError(Error::Forbidden("lists:read".into())));
    }
    let mut lists = Vec::new();
    for list in s.db.lists().by_domain(&host).await? {
        if auth.allows_list(&list.id, domain.id) {
            lists.push(list_value(s.flavor, &list));
        }
    }
    finish_authorization(&s, auth).await?;
    Ok(Json(paged(s.flavor, lists, &page_query)?))
}
#[utoipa::path(
    get,
    path = "/api/v1/domains/{host}/owners",
    params(PageQuery, ("host" = String, Path, description = "host path parameter")),
    responses((status = 200, description = "Successful operation", body = UserPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn domain_owners(
    State(s): State<AppState>,
    Path(host): Path<String>,
    Query(page_query): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize_domain(&s, &h, peer(c), "lists:read", &host).await?;
    Ok(Json(paged(
        s.flavor,
        s.db.domains().owners(&host).await?,
        &page_query,
    )?))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct ListQuery {
    advertised: Option<bool>,
}
#[utoipa::path(
    get,
    path = "/api/v1/lists",
    params(PageQuery, ("advertised" = Option<bool>, Query, description = "Filter by advertised status")),
    responses((status = 200, description = "Successful operation", body = MailingListPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn lists_list(
    State(s): State<AppState>,
    Query(q): Query<ListQuery>,
    Query(page_query): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let auth = authenticate_for_authorization(&s, &h, peer(c), "lists:read").await?;
    let mut lists = Vec::new();
    for list in s.db.lists().list(q.advertised).await? {
        if auth_allows_list(&s, &auth, &list.id).await? {
            lists.push(list_value(s.flavor, &list));
        }
    }
    finish_authorization(&s, auth).await?;
    Ok(Json(paged(s.flavor, lists, &page_query)?))
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
    post,
    path = "/api/v1/lists",
    request_body(content((ListInput = "application/json"), (ListInput = "application/x-www-form-urlencoded"))),
    responses((status = 201, description = "Successful operation", body = listmngr_core::MailingList), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn lists_create(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(v): JsonOrForm<ListInput>,
) -> ApiResult<Response> {
    let new = v.into_new_list()?;
    let addr = peer(c);
    let auth = authenticate_for_authorization(&s, &h, addr, "lists:write").await?;
    if !auth_allows_list(&s, &auth, &new.list_id).await? {
        return Err(ApiError(Error::Forbidden("lists:write".into())));
    }
    let auth = finish_authorization(&s, auth).await?;
    let list =
        s.db.lists()
            .create_with_context(new, &audit_context(&auth, addr))
            .await?;
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
    get,
    path = "/api/v1/lists/{id}",
    params(("id" = String, Path, description = "id path parameter")),
    responses((status = 200, description = "Successful operation", body = listmngr_core::MailingList), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn lists_get(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = parse_list_path(s.flavor, &id)?;
    authorize_list(&s, &h, peer(c), "lists:read", &id).await?;
    Ok(Json(list_value(s.flavor, &s.db.lists().get(&id).await?)))
}
#[utoipa::path(
    delete,
    path = "/api/v1/lists/{id}",
    params(("id" = String, Path, description = "id path parameter")),
    responses((status = 204, description = "Deleted"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn lists_delete(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<StatusCode> {
    let id = id.parse()?;
    let addr = peer(c);
    let auth = authorize_list(&s, &h, addr, "lists:write", &id).await?;
    s.db.lists()
        .delete_with_context(&id, &audit_context(&auth, addr))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(
    get,
    path = "/api/v1/lists/styles",
    params(PageQuery),
    responses((status = 200, description = "Successful operation", body = StringPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn styles(
    State(s): State<AppState>,
    Query(page_query): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &h, peer(c), "lists:read").await?;
    Ok(Json(paged(
        s.flavor,
        builtin_styles()
            .iter()
            .map(|v| v.name())
            .collect::<Vec<_>>(),
        &page_query,
    )?))
}
#[utoipa::path(
    get,
    path = "/api/v1/lists/{id}/config",
    params(("id" = String, Path, description = "id path parameter")),
    responses((status = 200, description = "Successful operation", body = ListConfigResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
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
    Ok(Json(bounce_config::project(
        s.flavor,
        list_config_value(
            &s.db.lists().get(&id).await?,
            &s.config.mailman.noreply_address,
        ),
    )))
}

fn list_config_value(list: &listmngr_core::MailingList, noreply_local_part: &str) -> Value {
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
        json!(format!("{noreply_local_part}@{}", list.id.mail_host())),
    );
    value
}
#[utoipa::path(
    put,
    path = "/api/v1/lists/{id}/config",
    params(("id" = String, Path, description = "id path parameter")),
    request_body(content((ListConfigInput = "application/json"), (ListConfigInput = "application/x-www-form-urlencoded"))),
    responses((status = 200, description = "Successful operation", body = ListConfigResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn list_config_put(
    state: State<AppState>,
    path: Path<String>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    body: JsonOrForm<Value>,
) -> ApiResult<Json<Value>> {
    list_config_write(state, path, headers, connect, body, true).await
}
#[utoipa::path(
    patch,
    path = "/api/v1/lists/{id}/config",
    params(("id" = String, Path, description = "id path parameter")),
    request_body(content((ListConfigInput = "application/json"), (ListConfigInput = "application/x-www-form-urlencoded"))),
    responses((status = 200, description = "Successful operation", body = ListConfigResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn list_config_patch(
    state: State<AppState>,
    path: Path<String>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    body: JsonOrForm<Value>,
) -> ApiResult<Json<Value>> {
    list_config_write(state, path, headers, connect, body, false).await
}
async fn list_config_write(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(mut v): JsonOrForm<Value>,
    replace: bool,
) -> ApiResult<Json<Value>> {
    let id = parse_list_path(s.flavor, &id)?;
    let addr = peer(c);
    let auth = authorize_list(&s, &h, addr, "lists:write", &id).await?;
    bounce_config::normalize(s.flavor, &mut v)?;
    normalize_list_config_form(&mut v, &h)?;
    let update = if replace {
        let defaults = listmngr_core::MailingList::new(id.clone(), id.list_name().to_owned());
        let mut replacement = json!({
            "display_name": defaults.display_name,
            "description": defaults.description,
            "info": defaults.info,
            "subject_prefix": defaults.subject_prefix,
            "advertised": defaults.advertised,
            "preferred_language": defaults.preferred_language,
            "anonymous_list": defaults.anonymous_list,
            "send_welcome_message": defaults.send_welcome_message,
            "send_goodbye_message": defaults.send_goodbye_message,
            "bounce_you_are_disabled_warnings": defaults.bounce_you_are_disabled_warnings,
            "bounce_you_are_disabled_warnings_interval": defaults.bounce_you_are_disabled_warnings_interval,
            "bounce_notify_owner_on_removal": defaults.bounce_notify_owner_on_removal,
            "process_bounces": defaults.process_bounces,
            "bounce_notify_owner_on_disable": defaults.bounce_notify_owner_on_disable,
            "bounce_notify_owner_on_bounce_increment": defaults.bounce_notify_owner_on_bounce_increment,
            "bounce_info_stale_after": defaults.bounce_info_stale_after,
            "bounce_score_threshold": defaults.bounce_score_threshold,
            "dmarc_mitigate_action": defaults.dmarc.action,
            "dmarc_mitigate_unconditionally": defaults.dmarc.unconditional,
            "next_digest_number": defaults.next_digest_number,
            "emergency": defaults.emergency,
            "max_message_size": defaults.max_message_size,
            "max_num_recipients": defaults.max_num_recipients,
            "archive_policy": defaults.archive_policy,
            "archive_rendering_mode": defaults.archive_rendering_mode,
            "default_member_action": defaults.default_member_action,
            "default_nonmember_action": defaults.default_nonmember_action,
            "administrivia": defaults.administrivia,
            "require_explicit_destination": defaults.require_explicit_destination,
            "acceptable_aliases": defaults.acceptable_aliases,
            "accept_these_nonmembers": defaults.accept_these_nonmembers,
            "hold_these_nonmembers": defaults.hold_these_nonmembers,
            "reject_these_nonmembers": defaults.reject_these_nonmembers,
            "discard_these_nonmembers": defaults.discard_these_nonmembers,
            "posting_pipeline": defaults.posting_pipeline,
            "respond_to_post_requests": defaults.respond_to_post_requests,
            "admin_immed_notify": defaults.admin_immed_notify,
        });
        let supplied = v
            .as_object()
            .ok_or_else(|| ApiError(Error::Validation("list config must be an object".into())))?;
        replacement
            .as_object_mut()
            .expect("replacement is an object")
            .extend(supplied.clone());
        replacement
    } else {
        v
    };
    Ok(Json(bounce_config::project(
        s.flavor,
        serde_json::to_value(
            s.db.lists()
                .update_with_context(&id, &update, &audit_context(&auth, addr))
                .await?,
        )
        .expect("serialize"),
    )))
}
// Form values are strings; keep JSON strict and convert only known numeric settings.
fn normalize_list_config_form(value: &mut Value, headers: &HeaderMap) -> ApiResult<()> {
    if headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/json"))
    {
        return Ok(());
    }
    for field in [
        "dmarc_mitigate_unconditionally",
        "send_welcome_message",
        "send_goodbye_message",
        "process_bounces",
        "bounce_notify_owner_on_disable",
        "bounce_notify_owner_on_bounce_increment",
        "bounce_notify_owner_on_removal",
        "administrivia",
        "require_explicit_destination",
        "respond_to_post_requests",
        "admin_immed_notify",
    ] {
        if let Some(Value::String(text)) = value.get(field) {
            let enabled = text
                .to_ascii_lowercase()
                .parse::<bool>()
                .map_err(|_| ApiError(Error::Validation(field.into())))?;
            value[field] = json!(enabled);
        }
    }
    if let Some(Value::String(text)) = value.get("bounce_score_threshold") {
        let number = text
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite())
            .ok_or_else(|| ApiError(Error::Validation("bounce_score_threshold".into())))?;
        value["bounce_score_threshold"] = json!(number);
    }
    for field in [
        "max_message_size",
        "max_num_recipients",
        "bounce_info_stale_after",
        "bounce_you_are_disabled_warnings",
        "bounce_you_are_disabled_warnings_interval",
        "next_digest_number",
    ] {
        if let Some(Value::String(text)) = value.get(field) {
            let number = text
                .parse::<i64>()
                .map_err(|_| ApiError(Error::Validation(field.into())))?;
            value[field] = json!(number);
        }
    }
    Ok(())
}

#[utoipa::path(
    get,
    path = "/api/v1/lists/{id}/config/{attr}",
    params(("id" = String, Path, description = "id path parameter"), ("attr" = String, Path, description = "attr path parameter")),
    responses((status = 200, description = "Successful operation", body = ListConfigAttributeValue), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
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
    let v = bounce_config::project(
        s.flavor,
        list_config_value(
            &s.db.lists().get(&id).await?,
            &s.config.mailman.noreply_address,
        ),
    );
    Ok(Json(
        v.get(&attr)
            .cloned()
            .ok_or(ApiError(Error::NotFound(attr)))?,
    ))
}
#[utoipa::path(
    put,
    path = "/api/v1/lists/{id}/config/{attr}",
    params(("id" = String, Path, description = "id path parameter"), ("attr" = String, Path, description = "attr path parameter")),
    request_body(content = ListConfigAttributeValue, content_type = "application/json"),
    responses((status = 200, description = "Successful operation", body = listmngr_core::MailingList), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
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
    patch,
    path = "/api/v1/lists/{id}/config/{attr}",
    params(("id" = String, Path, description = "id path parameter"), ("attr" = String, Path, description = "attr path parameter")),
    request_body(content = ListConfigAttributeValue, content_type = "application/json"),
    responses((status = 200, description = "Successful operation", body = listmngr_core::MailingList), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
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
    let addr = peer(c);
    let auth = authorize_list(&s, &h, addr, "lists:write", &id).await?;
    let value = if v.is_object() {
        v.get(&attr)
            .cloned()
            .ok_or(ApiError(Error::Validation(format!("missing {attr}"))))?
    } else {
        v
    };
    let mut update = json!({attr:value});
    bounce_config::normalize(s.flavor, &mut update)?;
    Ok(Json(bounce_config::project(
        s.flavor,
        serde_json::to_value(
            s.db.lists()
                .update_with_context(&id, &update, &audit_context(&auth, addr))
                .await?,
        )
        .expect("serialize"),
    )))
}
#[utoipa::path(
    get,
    path = "/api/v1/lists/{id}/archivers",
    params(PageQuery, ("id" = String, Path, description = "id path parameter")),
    responses((status = 200, description = "Successful operation", body = ArchiverPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn list_archivers(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(page_query): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = id.parse()?;
    authorize_list(&s, &h, peer(c), "lists:read", &id).await?;
    Ok(Json(paged(
        s.flavor,
        s.db.lists().archivers(&id).await?,
        &page_query,
    )?))
}
#[utoipa::path(
    get,
    path = "/api/v1/lists/{id}/templates",
    params(PageQuery, ("id" = String, Path, description = "id path parameter")),
    responses((status = 200, description = "Successful operation", body = TemplatePageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn list_templates(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(page_query): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = id.parse()?;
    authorize_list(&s, &h, peer(c), "lists:read", &id).await?;
    Ok(Json(paged(
        s.flavor,
        s.db.lists().templates(&id).await?,
        &page_query,
    )?))
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
    post,
    path = "/api/v1/members/mass",
    request_body(content = MassMemberInput, content_type = "application/json"),
    responses((status = 200, description = "Successful operation", body = ProcessedResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn members_mass(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    Json(v): Json<MassMemberInput>,
) -> ApiResult<impl IntoResponse> {
    let addr = peer(c);
    let auth = authorize_list(&s, &h, addr, "members:write", &v.list_id).await?;
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
            .mass_with_context(
                &v.list_id,
                &v.operation,
                &emails,
                &audit_context(&auth, addr),
            )
            .await?;
    Ok((StatusCode::OK, Json(json!({"processed":count}))))
}
const fn member_role() -> MemberRole {
    MemberRole::Member
}
#[utoipa::path(
    post,
    path = "/api/v1/members",
    request_body(content((MemberInput = "application/json"), (MemberInput = "application/x-www-form-urlencoded"))),
    responses((status = 201, description = "Successful operation", body = listmngr_core::Member), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn members_create(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(v): JsonOrForm<MemberInput>,
) -> ApiResult<Response> {
    let addr = peer(c);
    let auth = authorize_list(&s, &h, addr, "members:write", &v.list_id).await?;
    // mailmanclient's administrative role methods omit subscription workflow
    // flags. Role assignment does not itself verify ownership of an address.
    let administrative_role = matches!(s.flavor, ApiFlavor::Compat31)
        && matches!(v.role, MemberRole::Owner | MemberRole::Moderator);
    if v.workflow.invitation
        || (!administrative_role
            && (!v.confirmation.pre_verified
                || !v.confirmation.pre_confirmed
                || !v.confirmation.pre_approved))
    {
        return Err(ApiError(Error::Validation(
            "Phase 1 subscriptions require pre_verified, pre_confirmed and pre_approved".into(),
        )));
    }
    let member =
        s.db.members()
            .subscribe_with_context(
                NewMember {
                    list_id: v.list_id,
                    email: v.subscriber.clone(),
                    role: v.role,
                    subscription_mode: SubscriptionMode::AsAddress,
                    display_name: v.display_name,
                },
                v.confirmation.pre_verified,
                &audit_context(&auth, addr),
            )
            .await?;
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
    get,
    path = "/api/v1/lists/{id}/roster/{role}",
    params(PageQuery, ("id" = String, Path, description = "id path parameter"), ("role" = String, Path, description = "role path parameter")),
    responses((status = 200, description = "Successful operation", body = MemberPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn roster(
    State(s): State<AppState>,
    Path((id, role)): Path<(String, String)>,
    Query(page_query): Query<PageQuery>,
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
    Ok(Json(paged(s.flavor, entries, &page_query)?))
}
#[utoipa::path(
    get,
    path = "/api/v1/lists/{id}/member/{email}",
    params(("id" = String, Path, description = "id path parameter"), ("email" = String, Path, description = "email path parameter")),
    responses((status = 200, description = "Successful operation", body = listmngr_core::Member), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
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
    let member = values
        .iter()
        .find(|member| member.list_id == id)
        .ok_or(ApiError(Error::NotFound(email)))?;
    Ok(Json(member_value(&s, member).await?))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct UnsubscribeInput {
    #[serde(default, deserialize_with = "mailman_bool")]
    pre_confirmed: bool,
    #[serde(default, deserialize_with = "mailman_bool")]
    pre_approved: bool,
}

#[utoipa::path(
    delete,
    path = "/api/v1/lists/{id}/member/{email}",
    params(("id" = String, Path), ("email" = String, Path)),
    request_body(content((UnsubscribeInput = "application/json"), (UnsubscribeInput = "application/x-www-form-urlencoded"))),
    responses((status = 204, description = "Confirmed administrative removal"), (status = 400, description = "Both pre_confirmed and pre_approved must be true", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn list_member_delete(
    State(s): State<AppState>,
    Path((id, email)): Path<(String, String)>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(input): JsonOrForm<UnsubscribeInput>,
) -> ApiResult<StatusCode> {
    let id = id.parse()?;
    let addr = peer(c);
    let auth = authorize_list(&s, &h, addr, "members:write", &id).await?;
    if !input.pre_confirmed || !input.pre_approved {
        return Err(ApiError(Error::Validation("only pre_confirmed=true and pre_approved=true administrative unsubscribe is supported here; use the public leave confirmation workflow otherwise".into())));
    }
    let members = s.db.members().find(&email).await?;
    let member = members
        .iter()
        .find(|member| member.list_id == id && member.role == MemberRole::Member)
        .ok_or_else(|| ApiError(Error::NotFound(email)))?;
    s.db.members()
        .delete_with_context(member.id, &audit_context(&auth, addr))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

const HELD_OUT_MAX_ATTEMPTS: i64 = 8;

/// Held-message wire shape for `mailmanclient`'s `HeldMessage`.
///
/// Fields: `hold_date`, `message_id`, `msg`, `reason`, `request_id`,
/// `self_link`, `sender`, `subject`, `type`. Documented separately since the
/// flavor-specific runtime JSON is built by hand (see `held_entry_value`),
/// like `list_value`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct HeldMessageResponse {
    pub hold_date: String,
    pub message_id: String,
    pub msg: String,
    pub reason: String,
    pub request_id: String,
    pub self_link: String,
    pub sender: String,
    pub subject: String,
    pub r#type: String,
}
page_response!(HeldMessagePageResponse, HeldMessageResponse);
#[derive(Debug, Serialize, utoipa::ToSchema)]
struct CountResponse {
    count: usize,
}

async fn held_entry_value(
    s: &AppState,
    list: &listmngr_core::MailingList,
    held: &listmngr_db::moderation::HeldMessage,
) -> ApiResult<Value> {
    let message = s.db.mail_queue().message(held.message_id).await?;
    let hold_date = chrono::DateTime::from_timestamp_millis(held.hold_date)
        .map(|value| value.to_rfc3339())
        .unwrap_or_default();
    let prefix = match s.flavor {
        ApiFlavor::Compat31 => "/3.1",
        ApiFlavor::V1 => "/api/v1",
    };
    Ok(json!({
        "hold_date": hold_date,
        "message_id": message.external_id,
        // A lossy UTF-8 preview of the exact stored bytes; the database
        // (`mail_queue().message`) keeps the authoritative exact bytes.
        "msg": String::from_utf8_lossy(&message.raw),
        "reason": held.reason,
        "request_id": held.id.0.to_string(),
        "self_link": format!("{prefix}/lists/{}/held/{}", list.id, held.id.0),
        "sender": held.sender,
        "subject": held.subject,
        "type": "held_message",
    }))
}

fn parse_held_id(value: &str) -> ApiResult<listmngr_db::moderation::HeldId> {
    value
        .parse::<uuid::Uuid>()
        .map(listmngr_db::moderation::HeldId)
        .map_err(|_| ApiError(Error::Validation("held message id".into())))
}

#[utoipa::path(
    get,
    path = "/api/v1/lists/{id}/held",
    params(PageQuery, ("id" = String, Path, description = "id path parameter")),
    responses((status = 200, description = "Successful operation", body = HeldMessagePageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn list_held(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(page_query): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = parse_list_path(s.flavor, &id)?;
    authorize_list(&s, &h, peer(c), "moderation", &id).await?;
    let list = s.db.lists().get(&id).await?;
    let total = s.db.moderation().count_pending(&id).await?;
    let (start, end) = page_window(total, &page_query)?;
    let held = if start == end {
        Vec::new()
    } else {
        s.db.moderation()
            .pending_page(&id, start, end - start)
            .await?
    };
    let mut entries = Vec::with_capacity(held.len());
    for item in &held {
        entries.push(held_entry_value(&s, &list, item).await?);
    }
    Ok(Json(page_response(s.flavor, &entries, start, total)))
}
#[utoipa::path(
    get,
    path = "/api/v1/lists/{id}/held/count",
    params(("id" = String, Path, description = "id path parameter")),
    responses((status = 200, description = "Successful operation", body = CountResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn list_held_count(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = parse_list_path(s.flavor, &id)?;
    authorize_list(&s, &h, peer(c), "moderation", &id).await?;
    let count = s.db.moderation().count_pending(&id).await?;
    Ok(Json(json!({ "count": count })))
}
#[utoipa::path(
    get,
    path = "/api/v1/lists/{id}/held/{held_id}",
    params(("id" = String, Path, description = "id path parameter"), ("held_id" = String, Path, description = "held_id path parameter")),
    responses((status = 200, description = "Successful operation", body = HeldMessageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn list_held_get(
    State(s): State<AppState>,
    Path((id, held_id)): Path<(String, String)>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = parse_list_path(s.flavor, &id)?;
    authorize_list(&s, &h, peer(c), "moderation", &id).await?;
    let held = s.db.moderation().get(parse_held_id(&held_id)?).await?;
    if held.list_id != id {
        return Err(ApiError(Error::NotFound("held message".into())));
    }
    let list = s.db.lists().get(&id).await?;
    Ok(Json(held_entry_value(&s, &list, &held).await?))
}
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct ModerateInput {
    action: String,
    comment: Option<String>,
}
#[utoipa::path(
    post,
    path = "/api/v1/lists/{id}/held/{held_id}",
    params(("id" = String, Path, description = "id path parameter"), ("held_id" = String, Path, description = "held_id path parameter")),
    request_body(content((ModerateInput = "application/json"), (ModerateInput = "application/x-www-form-urlencoded"))),
    responses((status = 204, description = "Deleted"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn list_held_moderate(
    State(s): State<AppState>,
    Path((id, held_id)): Path<(String, String)>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(v): JsonOrForm<ModerateInput>,
) -> ApiResult<StatusCode> {
    use listmngr_db::moderation::ReviewAction;
    let id = parse_list_path(s.flavor, &id)?;
    let addr = peer(c);
    let auth = authorize_list(&s, &h, addr, "moderation", &id).await?;
    let held_id = parse_held_id(&held_id)?;
    let held = s.db.moderation().get(held_id).await?;
    if held.list_id != id {
        return Err(ApiError(Error::NotFound("held message".into())));
    }
    let now_ms = chrono::Utc::now().timestamp_millis();
    let reason = v.comment.unwrap_or_default();
    let action = match v.action.as_str() {
        "accept" => ReviewAction::Accept {
            max_attempts: HELD_OUT_MAX_ATTEMPTS,
        },
        "reject" => ReviewAction::Reject,
        "discard" => ReviewAction::Discard,
        "defer" => ReviewAction::Defer,
        other => {
            return Err(ApiError(Error::Validation(format!(
                "moderation action {other:?} is not implemented"
            ))));
        }
    };
    s.db.moderation()
        .review(
            held_id,
            &audit_context(&auth, addr),
            &action,
            &reason,
            now_ms,
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(
    get,
    path = "/api/v1/members/{id}",
    params(("id" = String, Path, description = "id path parameter")),
    responses((status = 200, description = "Successful operation", body = listmngr_core::Member), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
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
    delete,
    path = "/api/v1/members/{id}",
    params(("id" = String, Path, description = "id path parameter")),
    responses((status = 204, description = "Deleted"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
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
    let addr = peer(c);
    let (auth, _) = authorize_member(&s, &h, addr, "members:write", id).await?;
    s.db.members()
        .delete_with_context(id, &audit_context(&auth, addr))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(
    patch,
    path = "/api/v1/members/{id}",
    params(("id" = String, Path, description = "id path parameter")),
    request_body(content = MemberPatchInput, content_type = "application/json"),
    responses((status = 200, description = "Successful operation", body = listmngr_core::Member), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn member_patch(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    Json(v): Json<Value>,
) -> ApiResult<Json<Value>> {
    let id: MemberId = id
        .parse()
        .map_err(|_| ApiError(Error::Validation("member id".into())))?;
    let addr = peer(c);
    let (auth, _) = authorize_member(&s, &h, addr, "members:write", id).await?;
    let member =
        s.db.members()
            .update_with_context(id, &v, &audit_context(&auth, addr))
            .await?;
    let preferences = s.db.preferences().get(member.preferences_id).await?;
    let mut value = serde_json::to_value(member).expect("serialize");
    let preference_value = serde_json::to_value(preferences).expect("serialize");
    if let (Some(target), Some(source)) = (value.as_object_mut(), preference_value.as_object()) {
        target.extend(source.clone());
    }
    Ok(Json(value))
}
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct FindInput {
    subscriber: Option<String>,
    substring: Option<String>,
    list_id: Option<ListId>,
    role: Option<MemberRole>,
}
#[utoipa::path(
    post,
    path = "/api/v1/members/find",
    params(PageQuery),
    request_body(content((FindInput = "application/json"), (FindInput = "application/x-www-form-urlencoded"))),
    responses((status = 200, description = "Successful operation", body = MemberPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn members_find(
    State(s): State<AppState>,
    Query(page_query): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    JsonOrForm(v): JsonOrForm<FindInput>,
) -> ApiResult<Json<Value>> {
    let auth = authenticate_for_authorization(&s, &h, peer(c), "members:read").await?;
    let found = match (&v.subscriber, &v.substring) {
        (Some(subscriber), None) if !subscriber.trim().is_empty() => {
            s.db.members().find(subscriber).await?
        }
        (None, Some(substring)) if !substring.trim().is_empty() => {
            s.db.members().find_substring(substring).await?
        }
        _ => {
            return Err(ApiError(Error::Validation(
                "provide exactly one of subscriber or substring".into(),
            )));
        }
    };
    let members = filter_members_for_auth(&s, &auth, found)
        .await?
        .into_iter()
        .filter(|member| {
            v.list_id
                .as_ref()
                .is_none_or(|list| &member.list_id == list)
        })
        .filter(|member| v.role.is_none_or(|role| member.role == role))
        .collect::<Vec<_>>();
    finish_authorization(&s, auth).await?;
    let mut entries = Vec::with_capacity(members.len());
    for member in members {
        entries.push(member_value(&s, &member).await?);
    }
    Ok(Json(paged(s.flavor, entries, &page_query)?))
}
#[utoipa::path(
    get,
    path = "/api/v1/members/{id}/preferences",
    params(("id" = String, Path, description = "id path parameter")),
    responses((status = 200, description = "Successful operation", body = Preferences), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
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
    get,
    path = "/api/v1/members/{id}/all/preferences",
    params(("id" = String, Path, description = "id path parameter")),
    responses((status = 200, description = "Successful operation", body = Preferences), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
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
fn preferences_update(
    current: Preferences,
    value: &Value,
    replace: bool,
) -> Result<Preferences, Error> {
    let object = value
        .as_object()
        .ok_or_else(|| Error::Validation("preferences must be an object".into()))?;
    let mut result = if replace {
        Preferences::default()
    } else {
        current
    };
    for (key, value) in object {
        match key.as_str() {
            "acknowledge_posts" => {
                result.acknowledge_posts = if value.is_null() {
                    None
                } else {
                    Some(
                        value
                            .as_bool()
                            .ok_or_else(|| Error::Validation(key.clone()))?,
                    )
                };
            }
            "hide_address" => {
                result.hide_address = if value.is_null() {
                    None
                } else {
                    Some(
                        value
                            .as_bool()
                            .ok_or_else(|| Error::Validation(key.clone()))?,
                    )
                };
            }
            "preferred_language" => {
                result.preferred_language = if value.is_null() {
                    None
                } else {
                    Some(
                        value
                            .as_str()
                            .filter(|language| !language.trim().is_empty())
                            .ok_or_else(|| Error::Validation(key.clone()))?
                            .to_owned(),
                    )
                };
            }
            "receive_list_copy" => {
                result.receive_list_copy = if value.is_null() {
                    None
                } else {
                    Some(
                        value
                            .as_bool()
                            .ok_or_else(|| Error::Validation(key.clone()))?,
                    )
                };
            }
            "receive_own_postings" => {
                result.receive_own_postings = if value.is_null() {
                    None
                } else {
                    Some(
                        value
                            .as_bool()
                            .ok_or_else(|| Error::Validation(key.clone()))?,
                    )
                };
            }
            "delivery_mode" => {
                result.delivery_mode = if value.is_null() {
                    None
                } else {
                    Some(
                        value
                            .as_str()
                            .ok_or_else(|| Error::Validation(key.clone()))?
                            .parse::<DeliveryMode>()?,
                    )
                };
            }
            "delivery_status" => {
                result.delivery_status = if value.is_null() {
                    None
                } else {
                    Some(
                        value
                            .as_str()
                            .ok_or_else(|| Error::Validation(key.clone()))?
                            .parse::<DeliveryStatus>()?,
                    )
                };
            }
            _ => {
                return Err(Error::Validation(format!(
                    "read-only or unknown preference: {key}"
                )));
            }
        }
    }
    Ok(result)
}

#[utoipa::path(
    put,
    path = "/api/v1/members/{id}/preferences",
    params(("id" = String, Path, description = "id path parameter")),
    request_body(content = Preferences, content_type = "application/json"),
    responses((status = 200, description = "Successful operation", body = Preferences), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn member_preferences_put(
    state: State<AppState>,
    path: Path<String>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    body: Json<Value>,
) -> ApiResult<Json<Value>> {
    member_preferences_write(state, path, headers, connect, body, true).await
}
#[utoipa::path(
    patch,
    path = "/api/v1/members/{id}/preferences",
    params(("id" = String, Path, description = "id path parameter")),
    request_body(content = Preferences, content_type = "application/json"),
    responses((status = 200, description = "Successful operation", body = Preferences), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn member_preferences_patch(
    state: State<AppState>,
    path: Path<String>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    body: Json<Value>,
) -> ApiResult<Json<Value>> {
    member_preferences_write(state, path, headers, connect, body, false).await
}
async fn member_preferences_write(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    Json(v): Json<Value>,
    replace: bool,
) -> ApiResult<Json<Value>> {
    let id: MemberId = id
        .parse()
        .map_err(|_| ApiError(Error::Validation("member id".into())))?;
    let addr = peer(c);
    let (auth, member) = authorize_member(&s, &h, addr, "members:write", id).await?;
    let current = s.db.preferences().get(member.preferences_id).await?;
    let updated = preferences_update(current, &v, replace)?;
    s.db.preferences()
        .set_member_with_context(id, updated, &audit_context(&auth, addr))
        .await?;
    Ok(Json(
        serde_json::to_value(s.db.preferences().get(member.preferences_id).await?)
            .expect("serialize"),
    ))
}

#[utoipa::path(
    get,
    path = "/api/v1/users",
    params(PageQuery),
    responses((status = 200, description = "Successful operation", body = UserPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn users_list(
    State(s): State<AppState>,
    Query(page_query): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let auth = authenticate_for_authorization(&s, &h, peer(c), "system:read").await?;
    let mut users = Vec::new();
    for user in s.db.users().list().await? {
        if auth_allows_user(&s, &auth, user.id).await? {
            users.push(user);
        }
    }
    finish_authorization(&s, auth).await?;
    Ok(Json(paged(s.flavor, users, &page_query)?))
}
#[utoipa::path(
    post,
    path = "/api/v1/users",
    request_body(content = listmngr_db::NewUser, content_type = "application/json"),
    responses((status = 201, description = "Successful operation", body = listmngr_core::User), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn users_create(
    State(s): State<AppState>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    Json(v): Json<NewUser>,
) -> ApiResult<impl IntoResponse> {
    let addr = peer(c);
    let auth = authorize_admin(&s, &h, addr, "users:write").await?;
    Ok((
        StatusCode::CREATED,
        Json(
            s.db.users()
                .create_with_context(v, &audit_context(&auth, addr))
                .await?,
        ),
    ))
}
#[utoipa::path(
    get,
    path = "/api/v1/users/{id}",
    params(("id" = String, Path, description = "id path parameter")),
    responses((status = 200, description = "Successful operation", body = listmngr_core::User), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn users_get(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id: UserId = id
        .parse()
        .map_err(|_| ApiError(Error::Validation("user id".into())))?;
    authorize_user(&s, &h, peer(c), "system:read", id).await?;
    Ok(Json(
        serde_json::to_value(s.db.users().get(id).await?).expect("serialize"),
    ))
}
#[utoipa::path(
    delete,
    path = "/api/v1/users/{id}",
    params(("id" = String, Path, description = "id path parameter")),
    responses((status = 204, description = "Deleted"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn users_delete(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<StatusCode> {
    let id = id
        .parse()
        .map_err(|_| ApiError(Error::Validation("user id".into())))?;
    let addr = peer(c);
    let auth = authorize_user(&s, &h, addr, "users:write", id).await?;
    s.db.users()
        .delete_with_context(id, &audit_context(&auth, addr))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(
    patch,
    path = "/api/v1/users/{id}",
    params(("id" = String, Path, description = "id path parameter")),
    request_body(content = UserPatchInput, content_type = "application/json"),
    responses((status = 200, description = "Successful operation", body = listmngr_core::User), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn users_patch(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    Json(v): Json<Value>,
) -> ApiResult<Json<Value>> {
    let id: UserId = id
        .parse()
        .map_err(|_| ApiError(Error::Validation("user id".into())))?;
    let addr = peer(c);
    let auth = authorize_user(&s, &h, addr, "users:write", id).await?;
    Ok(Json(
        serde_json::to_value(
            s.db.users()
                .update_with_context(id, &v, &audit_context(&auth, addr))
                .await?,
        )
        .expect("serialize"),
    ))
}
#[utoipa::path(
    get,
    path = "/api/v1/users/{id}/preferences",
    params(("id" = String, Path, description = "id path parameter")),
    responses((status = 200, description = "Successful operation", body = Preferences), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn user_preferences(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id: UserId = id
        .parse()
        .map_err(|_| ApiError(Error::Validation("user id".into())))?;
    authorize_user(&s, &h, peer(c), "system:read", id).await?;
    Ok(Json(
        serde_json::to_value(s.db.preferences().get_user(id).await?).expect("serialize"),
    ))
}
#[utoipa::path(
    get,
    path = "/api/v1/users/{id}/all/preferences",
    params(("id" = String, Path, description = "id path parameter")),
    responses((status = 200, description = "Successful operation", body = Preferences), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn user_all_preferences(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id: UserId = id
        .parse()
        .map_err(|_| ApiError(Error::Validation("user id".into())))?;
    authorize_user(&s, &h, peer(c), "system:read", id).await?;
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
    put,
    path = "/api/v1/users/{id}/preferences",
    params(("id" = String, Path, description = "id path parameter")),
    request_body(content = Preferences, content_type = "application/json"),
    responses((status = 200, description = "Successful operation", body = Preferences), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn user_preferences_put(
    state: State<AppState>,
    path: Path<String>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    body: Json<Value>,
) -> ApiResult<Json<Value>> {
    user_preferences_write(state, path, headers, connect, body, true).await
}
#[utoipa::path(
    patch,
    path = "/api/v1/users/{id}/preferences",
    params(("id" = String, Path, description = "id path parameter")),
    request_body(content = Preferences, content_type = "application/json"),
    responses((status = 200, description = "Successful operation", body = Preferences), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn user_preferences_patch(
    state: State<AppState>,
    path: Path<String>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    body: Json<Value>,
) -> ApiResult<Json<Value>> {
    user_preferences_write(state, path, headers, connect, body, false).await
}
async fn user_preferences_write(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    Json(v): Json<Value>,
    replace: bool,
) -> ApiResult<Json<Value>> {
    let id = id
        .parse()
        .map_err(|_| ApiError(Error::Validation("user id".into())))?;
    let addr = peer(c);
    let auth = authorize_user(&s, &h, addr, "users:write", id).await?;
    let updated = preferences_update(s.db.preferences().get_user(id).await?, &v, replace)?;
    s.db.preferences()
        .set_user_with_context(id, updated.clone(), &audit_context(&auth, addr))
        .await?;
    Ok(Json(serde_json::to_value(updated).expect("serialize")))
}
#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct LoginInput {
    password: String,
}
#[utoipa::path(
    post,
    path = "/api/v1/users/{id}/login",
    params(("id" = String, Path, description = "id path parameter")),
    request_body(content = LoginInput, content_type = "application/json"),
    responses((status = 200, description = "Successful operation", body = LoginResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn user_login(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    Json(v): Json<LoginInput>,
) -> ApiResult<Json<Value>> {
    let id = id
        .parse()
        .map_err(|_| ApiError(Error::Validation("user id".into())))?;
    authorize_user(&s, &h, peer(c), "users:write", id).await?;
    let valid = s.db.users().verify_password(id, &v.password).await?;
    Ok(Json(json!({"success":valid})))
}
#[utoipa::path(
    get,
    path = "/api/v1/users/{id}/addresses",
    params(PageQuery, ("id" = String, Path, description = "id path parameter")),
    responses((status = 200, description = "Successful operation", body = AddressPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn user_addresses(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(page_query): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let id = id
        .parse()
        .map_err(|_| ApiError(Error::Validation("user id".into())))?;
    let auth = authenticate_for_authorization(&s, &h, peer(c), "system:read").await?;
    if !auth_allows_user(&s, &auth, id).await? {
        return Err(ApiError(Error::Forbidden("system:read".into())));
    }
    s.db.users().get(id).await?;
    let mut entries = Vec::new();
    for address in s.db.addresses().by_user(id).await? {
        if auth_allows_address(&s, &auth, &address.email).await? {
            entries.push(address);
        }
    }
    finish_authorization(&s, auth).await?;
    Ok(Json(paged(s.flavor, entries, &page_query)?))
}
#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct LinkInput {
    email: String,
}
#[utoipa::path(
    post,
    path = "/api/v1/users/{id}/addresses",
    params(("id" = String, Path, description = "id path parameter")),
    request_body(content = LinkInput, content_type = "application/json"),
    responses((status = 201, description = "Successful operation", body = listmngr_core::Address), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn user_address_link(
    State(s): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    Json(v): Json<LinkInput>,
) -> ApiResult<Json<Value>> {
    let id = id
        .parse()
        .map_err(|_| ApiError(Error::Validation("user id".into())))?;
    let addr = peer(c);
    let auth = authorize_user_and_address(&s, &h, addr, "users:write", id, &v.email).await?;
    Ok(Json(
        serde_json::to_value(
            s.db.addresses()
                .link_with_context(&v.email, Some(id), &audit_context(&auth, addr))
                .await?,
        )
        .expect("serialize"),
    ))
}

#[utoipa::path(
    get,
    path = "/api/v1/addresses/{email}",
    params(("email" = String, Path, description = "email path parameter")),
    responses((status = 200, description = "Successful operation", body = listmngr_core::Address), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn address_get(
    State(s): State<AppState>,
    Path(email): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize_address(&s, &h, peer(c), "system:read", &email).await?;
    Ok(Json(
        serde_json::to_value(s.db.addresses().get(&email).await?).expect("serialize"),
    ))
}
#[utoipa::path(
    post,
    path = "/api/v1/addresses/{email}/verify",
    params(("email" = String, Path, description = "email path parameter")),
    request_body(content = EmptyMutationInput, content_type = "application/json"),
    responses((status = 200, description = "Successful operation", body = listmngr_core::Address), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn address_verify(
    State(s): State<AppState>,
    Path(email): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    _body: Option<Json<EmptyMutationInput>>,
) -> ApiResult<Json<Value>> {
    let addr = peer(c);
    let auth = authorize_address(&s, &h, addr, "users:write", &email).await?;
    Ok(Json(
        serde_json::to_value(
            s.db.addresses()
                .verify_with_context(&email, true, &audit_context(&auth, addr))
                .await?,
        )
        .expect("serialize"),
    ))
}
#[utoipa::path(
    post,
    path = "/api/v1/addresses/{email}/unverify",
    params(("email" = String, Path, description = "email path parameter")),
    request_body(content = EmptyMutationInput, content_type = "application/json"),
    responses((status = 200, description = "Successful operation", body = listmngr_core::Address), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn address_unverify(
    State(s): State<AppState>,
    Path(email): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    _body: Option<Json<EmptyMutationInput>>,
) -> ApiResult<Json<Value>> {
    let addr = peer(c);
    let auth = authorize_address(&s, &h, addr, "users:write", &email).await?;
    Ok(Json(
        serde_json::to_value(
            s.db.addresses()
                .verify_with_context(&email, false, &audit_context(&auth, addr))
                .await?,
        )
        .expect("serialize"),
    ))
}
#[utoipa::path(
    get,
    path = "/api/v1/addresses/{email}/user",
    params(("email" = String, Path, description = "email path parameter")),
    responses((status = 200, description = "Successful operation", body = AddressUserResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn address_user(
    State(s): State<AppState>,
    Path(email): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize_address(&s, &h, peer(c), "system:read", &email).await?;
    let address = s.db.addresses().get(&email).await?;
    Ok(Json(json!({"user_id":address.user_id})))
}
#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct UserLink {
    user_id: UserId,
}
#[utoipa::path(
    post,
    path = "/api/v1/addresses/{email}/user",
    params(("email" = String, Path, description = "email path parameter")),
    request_body(content = UserLink, content_type = "application/json"),
    responses((status = 201, description = "Successful operation", body = listmngr_core::Address), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn address_link(
    State(s): State<AppState>,
    Path(email): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    Json(v): Json<UserLink>,
) -> ApiResult<Json<Value>> {
    let addr = peer(c);
    let auth = authorize_user_and_address(&s, &h, addr, "users:write", v.user_id, &email).await?;
    Ok(Json(
        serde_json::to_value(
            s.db.addresses()
                .link_with_context(&email, Some(v.user_id), &audit_context(&auth, addr))
                .await?,
        )
        .expect("serialize"),
    ))
}
#[utoipa::path(
    delete,
    path = "/api/v1/addresses/{email}/user",
    params(("email" = String, Path, description = "email path parameter")),
    responses((status = 204, description = "Deleted"), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn address_unlink(
    State(s): State<AppState>,
    Path(email): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<StatusCode> {
    let addr = peer(c);
    let auth = authorize_address(&s, &h, addr, "users:write", &email).await?;
    s.db.addresses()
        .link_with_context(&email, None, &audit_context(&auth, addr))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(
    get,
    path = "/api/v1/addresses/{email}/memberships",
    params(PageQuery, ("email" = String, Path, description = "email path parameter")),
    responses((status = 200, description = "Successful operation", body = MemberPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn address_memberships(
    State(s): State<AppState>,
    Path(email): Path<String>,
    Query(page_query): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let auth = authorize_address(&s, &h, peer(c), "members:read", &email).await?;
    let members = filter_members_for_auth(&s, &auth, s.db.members().find(&email).await?).await?;
    let mut entries = Vec::with_capacity(members.len());
    for member in members {
        entries.push(member_value(&s, &member).await?);
    }
    Ok(Json(paged(s.flavor, entries, &page_query)?))
}
#[utoipa::path(
    get,
    path = "/api/v1/addresses/{email}/preferences",
    params(("email" = String, Path, description = "email path parameter")),
    responses((status = 200, description = "Successful operation", body = Preferences), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn address_preferences(
    State(s): State<AppState>,
    Path(email): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize_address(&s, &h, peer(c), "system:read", &email).await?;
    Ok(Json(
        serde_json::to_value(s.db.preferences().get_address(&email).await?).expect("serialize"),
    ))
}
#[utoipa::path(
    get,
    path = "/api/v1/addresses/{email}/all/preferences",
    params(("email" = String, Path, description = "email path parameter")),
    responses((status = 200, description = "Successful operation", body = Preferences), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn address_all_preferences(
    State(s): State<AppState>,
    Path(email): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize_address(&s, &h, peer(c), "system:read", &email).await?;
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
    put,
    path = "/api/v1/addresses/{email}/preferences",
    params(("email" = String, Path, description = "email path parameter")),
    request_body(content = Preferences, content_type = "application/json"),
    responses((status = 200, description = "Successful operation", body = Preferences), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn address_preferences_put(
    state: State<AppState>,
    path: Path<String>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    body: Json<Value>,
) -> ApiResult<Json<Value>> {
    address_preferences_write(state, path, headers, connect, body, true).await
}
#[utoipa::path(
    patch,
    path = "/api/v1/addresses/{email}/preferences",
    params(("email" = String, Path, description = "email path parameter")),
    request_body(content = Preferences, content_type = "application/json"),
    responses((status = 200, description = "Successful operation", body = Preferences), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn address_preferences_patch(
    state: State<AppState>,
    path: Path<String>,
    headers: HeaderMap,
    connect: ConnectInfo<SocketAddr>,
    body: Json<Value>,
) -> ApiResult<Json<Value>> {
    address_preferences_write(state, path, headers, connect, body, false).await
}
async fn address_preferences_write(
    State(s): State<AppState>,
    Path(email): Path<String>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
    Json(v): Json<Value>,
    replace: bool,
) -> ApiResult<Json<Value>> {
    let addr = peer(c);
    let auth = authorize_address(&s, &h, addr, "users:write", &email).await?;
    let updated = preferences_update(s.db.preferences().get_address(&email).await?, &v, replace)?;
    s.db.preferences()
        .set_address_with_context(&email, updated.clone(), &audit_context(&auth, addr))
        .await?;
    Ok(Json(serde_json::to_value(updated).expect("serialize")))
}
#[utoipa::path(
    get,
    path = "/api/v1/owners",
    params(PageQuery),
    responses((status = 200, description = "Successful operation", body = UserPageResponse), (status = 400, description = "Invalid request", body = ErrorResponse), (status = 401, description = "Authentication required", body = ErrorResponse), (status = 403, description = "Insufficient scope", body = ErrorResponse), (status = 404, description = "Resource not found", body = ErrorResponse), (status = 409, description = "Resource conflict", body = ErrorResponse), (status = 429, description = "Rate limit exceeded", body = ErrorResponse), (status = 500, description = "Internal error", body = ErrorResponse)),
    security(("bearerAuth" = []))
)]
async fn owners(
    State(s): State<AppState>,
    Query(page_query): Query<PageQuery>,
    h: HeaderMap,
    c: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    authorize_admin(&s, &h, peer(c), "system:read").await?;
    let owners =
        s.db.users()
            .list()
            .await?
            .into_iter()
            .filter(|u| u.is_server_owner)
            .collect::<Vec<_>>();
    Ok(Json(paged(s.flavor, owners, &page_query)?))
}
