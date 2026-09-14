//! Browser interface. This router is deliberately outside bearer API middleware.
//!
//! Handlers build view models; `listmngr_web` owns every byte of markup and
//! escapes every value at compile time (ADR-0004). No handler concatenates
//! HTML, and no page loads a third-party asset.
#[path = "webui_admin.rs"]
mod admin;
#[path = "webui_archive.rs"]
mod archive;
#[path = "webui_membership.rs"]
mod membership;
#[path = "webui_password.rs"]
mod password;
#[path = "webui_recovery.rs"]
mod recovery;
#[path = "webui_settings.rs"]
mod settings;
use crate::{ApiError, ApiResult, AppState};
use askama::Template;
use axum::{
    Form,
    extract::{DefaultBodyLimit, Request},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::Redirect,
    routing::post,
};
use axum::{
    Router,
    extract::State,
    http::header,
    response::{Html, IntoResponse, Response},
    routing::get,
};
use listmngr_core::Error;
use listmngr_db::web_sessions::WebSession;
use listmngr_web::{Nav, Pagination, Shell, choices};
use serde::Deserialize;

/// A rendered template. Rendering writes into a `String` and so only fails the
/// way `write!` into a `String` fails.
fn html<T: Template>(template: &T) -> Response {
    Html(template.render().expect("template renders")).into_response()
}

/// The reader's language: their browser's ordered preferences, then the site
/// default, then English. Nothing else — never the list, never a forwarded
/// header — chooses the document language.
pub fn language(s: &AppState, headers: &HeaderMap) -> &'static str {
    let header = headers
        .get(header::ACCEPT_LANGUAGE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let mut ranked: Vec<(f32, &str)> = header
        .split(',')
        .take(16)
        .filter_map(|part| {
            let mut pieces = part.split(';');
            let tag = pieces.next()?.trim();
            if tag.is_empty() || tag.len() > 35 {
                return None;
            }
            let quality = pieces
                .find_map(|piece| piece.trim().strip_prefix("q=")?.parse::<f32>().ok())
                .unwrap_or(1.0);
            (0.0..=1.0).contains(&quality).then_some((quality, tag))
        })
        .collect();
    ranked.sort_by(|left, right| right.0.total_cmp(&left.0));
    listmngr_i18n::choose(
        ranked
            .into_iter()
            .map(|(_, tag)| tag)
            .chain([s.db.default_language()]),
    )
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/web", get(directory))
        .route(
            "/web/style.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    listmngr_web::STYLESHEET,
                )
            }),
        )
        .route(
            "/web/htmx.min.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    listmngr_web::HTMX,
                )
            }),
        )
        .route("/web/login", get(login_form).post(login))
        .route("/web/account", get(account))
        .route("/web/admin", get(admin::index))
        .route(
            "/web/lists/{id}/settings",
            get(settings::form).post(settings::save),
        )
        .route("/web/lists/{id}/members", get(admin::members))
        .route(
            "/web/lists/{id}/members/{member}/policy",
            post(admin::policy),
        )
        .route(
            "/web/account/password",
            get(password::form).post(password::change),
        )
        .route("/web/members/{id}/preferences", post(preferences))
        .route(
            "/web/members/{id}/leave",
            get(membership::preview).post(membership::leave),
        )
        .route(
            "/web/members/{id}/recover",
            get(recovery::preview).post(recovery::recover),
        )
        .route("/web/logout", post(logout))
        .route("/web/lists/{id}", get(list_page))
        .route("/web/lists/{id}/archive", get(archive::browse))
        .route("/web/lists/{id}/request", post(subscription_request))
        .route("/web/moderation", get(moderation_index))
        .route("/web/lists/{id}/held", get(held_page))
        .route("/web/lists/{id}/held/{held}", post(review))
        .route("/web/lists/{id}/confirm", get(confirm_form).post(confirm))
        .layer(DefaultBodyLimit::max(8192))
        .layer(middleware::from_fn(security_headers))
}

async fn directory(
    State(s): State<AppState>,
    Query(paging): Query<BrowserPage>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ids: Vec<String> = sqlx::query_scalar(
        "SELECT list_id FROM mailing_lists WHERE advertised=1 ORDER BY list_id LIMIT 21 OFFSET $1",
    )
    .bind(paging.offset()?)
    .fetch_all(s.db.pool())
    .await
    .map_err(|error| database_error(&error))?;
    let more = ids.len() > 20;
    let mut entries = Vec::new();
    for id in ids.into_iter().take(20) {
        let list = s.db.lists().get(&id.parse()?).await?;
        if !list.advertised {
            continue;
        }
        entries.push(listmngr_web::DirectoryEntry {
            href: format!("/web/lists/{}", list.id.as_str()),
            name: list.display_name,
            id: list.id.to_string(),
            description: list.description,
        });
    }
    Ok(html(&listmngr_web::Directory {
        shell: Shell::new(language(&s, &headers), "web-title-directory", Nav::Lists),
        entries,
        pagination: paging.pagination("/web", more),
    }))
}

async fn security_headers(request: Request, next: Next) -> Response {
    let language = request
        .headers()
        .get(header::ACCEPT_LANGUAGE)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
        .unwrap_or_default();
    let mut r = next.run(request).await;
    if r.status().is_client_error() || r.status().is_server_error() {
        let status = r.status();
        let retry_after = r.headers().get(header::RETRY_AFTER).cloned();
        // The failure page cannot reach the database, so it negotiates on the
        // request's own preferences and English.
        r = html(&listmngr_web::ErrorPage {
            shell: Shell::new(
                listmngr_i18n::choose(
                    language
                        .split(',')
                        .map(|tag| tag.split(';').next().unwrap_or_default().trim()),
                ),
                "web-title-error",
                Nav::None,
            ),
        });
        *r.status_mut() = status;
        if let Some(value) = retry_after {
            r.headers_mut().insert(header::RETRY_AFTER, value);
        }
    }
    for (k, v) in [
        ("cache-control", "no-store"),
        // no-referrer makes Chromium serialize form POST Origin as null.
        // strict-origin still strips token-bearing paths and query strings.
        ("referrer-policy", "strict-origin"),
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
        (
            "content-security-policy",
            "default-src 'none'; style-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'",
        ),
    ] {
        r.headers_mut()
            .insert(axum::http::HeaderName::from_static(k), v.parse().unwrap());
    }
    r
}
fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn denied() -> ApiError {
    ApiError(Error::Forbidden("browser request".into()))
}
fn token(h: &HeaderMap) -> Option<&str> {
    let mut values = h
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|p| p.trim().strip_prefix("listmngr_session="));
    let first = values.next()?;
    if values.next().is_some() {
        None
    } else {
        Some(first)
    }
}
// Only configured origins are accepted; never infer trust from Host or forwarded headers.
fn origin(s: &AppState) -> ApiResult<(String, bool)> {
    let uri: sup::Uri = s.config.site.base_url.parse().map_err(|_| denied())?;
    let scheme = uri.scheme_str().ok_or_else(denied)?;
    let authority = uri.authority().ok_or_else(denied)?;
    let secure = scheme == "https";
    if !(secure
        || scheme == "http" && matches!(uri.host(), Some("localhost" | "127.0.0.1" | "[::1]")))
    {
        return Err(denied());
    }
    if authority.as_str().contains('@') {
        return Err(denied());
    }
    Ok((format!("{scheme}://{authority}"), secure))
}
mod sup {
    pub use axum::http::Uri;
}
fn check_origin(s: &AppState, h: &HeaderMap) -> ApiResult<()> {
    let (expected, _) = origin(s)?;
    if h.get(header::ORIGIN).and_then(|v| v.to_str().ok()) != Some(expected.as_str())
        || h.get("sec-fetch-site")
            .is_some_and(|v| v != "same-origin" && v != "none")
    {
        return Err(denied());
    }
    Ok(())
}
async fn load(s: &AppState, h: &HeaderMap) -> ApiResult<WebSession> {
    Ok(s.db
        .web_session(token(h).ok_or(Error::Authentication)?, now())
        .await?)
}
async fn anonymous(s: &AppState, h: &HeaderMap) -> ApiResult<WebSession> {
    if let Some(t) = token(h) {
        match s.db.web_session(t, now()).await {
            Ok(v) => return Ok(v),
            Err(Error::Authentication) => {}
            Err(e) => return Err(e.into()),
        }
    }
    s.pre_auth_rate
        .check("web-session-issue")
        .map_err(|retry_after| ApiError(Error::RateLimited { retry_after }))?;
    Ok(s.db.create_web_session(None, None, now()).await?)
}
fn set_cookie(s: &AppState, session: &WebSession, r: &mut Response) -> ApiResult<()> {
    let (_, secure) = origin(s)?;
    r.headers_mut().insert(
        header::SET_COOKIE,
        format!(
            "listmngr_session={}; Path=/web; HttpOnly; SameSite=Strict; Max-Age={}{}",
            session.token,
            if session.user_id.is_some() {
                28800
            } else {
                1800
            },
            if secure { "; Secure" } else { "" }
        )
        .parse()
        .map_err(|_| denied())?,
    );
    Ok(())
}
async fn write_session(s: &AppState, h: &HeaderMap, csrf: &str) -> ApiResult<WebSession> {
    check_origin(s, h)?;
    let session = load(s, h).await?;
    if !session.verifies_csrf(csrf) {
        return Err(denied());
    }
    Ok(session)
}
async fn login_form(State(s): State<AppState>, h: HeaderMap) -> ApiResult<Response> {
    let session = anonymous(&s, &h).await?;
    let mut r = html(&listmngr_web::Login {
        shell: Shell::new(language(&s, &h), "web-title-login", Nav::Login),
        csrf: session.csrf.clone(),
    });
    set_cookie(&s, &session, &mut r)?;
    Ok(r)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Login {
    #[serde(default)]
    csrf: String,
    email: String,
    password: String,
}
async fn login(
    State(s): State<AppState>,
    h: HeaderMap,
    Form(f): Form<Login>,
) -> ApiResult<Response> {
    let session = write_session(&s, &h, &f.csrf).await?;
    // A bounded global bucket limits expensive password work without attacker-chosen keys.
    s.web_login_rate
        .check("web-login")
        .map_err(|retry_after| ApiError(Error::RateLimited { retry_after }))?;
    if f.password.len() > 1024 {
        return Err(Error::Authentication.into());
    }
    let fresh = s.db.browser_login(&f.email, &f.password, &session).await?;
    let mut r = Redirect::to("/web/account").into_response();
    set_cookie(&s, &fresh, &mut r)?;
    Ok(r)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Csrf {
    #[serde(default)]
    csrf: String,
}
async fn account(
    State(s): State<AppState>,
    Query(paging): Query<BrowserPage>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let session = load(&s, &headers).await?;
    let language = language(&s, &headers);
    let u =
        s.db.users()
            .get(session.user_id.ok_or(Error::Authentication)?)
            .await?;
    let ids: Vec<String> = sqlx::query_scalar("SELECT m.id FROM members m JOIN addresses a ON a.id=m.address_id WHERE a.user_id=$1 AND a.verified_on IS NOT NULL AND (m.subscription_mode='as_address' OR m.user_id=$1) AND m.role='member' ORDER BY m.list_id,m.id LIMIT 21 OFFSET $2")
        .bind(u.id.to_string()).bind(paging.offset()?).fetch_all(s.db.pool()).await.map_err(|error| database_error(&error))?;
    let more = ids.len() > 20;
    let mut subscriptions = Vec::new();
    for id in ids.into_iter().take(20) {
        let m =
            s.db.members()
                .get(id.parse().map_err(|_| denied())?)
                .await?;
        let p =
            s.db.preferences()
                .resolve_member(m.id, s.db.default_language())
                .await?;
        let list = s.db.lists().get(&m.list_id).await?;
        let delivery_mode = p.delivery_mode.map(|v| v.to_string()).unwrap_or_default();
        let status = p.delivery_status.map(|v| v.to_string()).unwrap_or_default();
        let disabled = matches!(
            p.delivery_status,
            Some(DeliveryStatus::ByModerator | DeliveryStatus::ByBounces | DeliveryStatus::Unknown)
        );
        let recoverable = disabled && s.db.browser_recover_preview(&session, m.id).await.is_ok();
        subscriptions.push(listmngr_web::Subscription {
            list_id: m.list_id.to_string(),
            delivery: listmngr_i18n::message(
                language,
                "web-account-delivery",
                &[("mode", &delivery_mode), ("status", &status)],
            ),
            archive_href: (list.archive_policy != listmngr_core::ArchivePolicy::Never)
                .then(|| format!("/web/lists/{}/archive", m.list_id.as_str())),
            recover_href: recoverable.then(|| format!("/web/members/{}/recover", m.id)),
            restricted: disabled && !recoverable,
            preferences: (!disabled).then(|| listmngr_web::Preferences {
                action: format!("/web/members/{}/preferences", m.id),
                modes: choices(
                    language,
                    &[
                        ("regular", "web-delivery-regular"),
                        ("plaintext_digests", "web-delivery-plaintext"),
                        ("mime_digests", "web-delivery-mime"),
                    ],
                    Some(delivery_mode.as_str()),
                ),
                statuses: choices(
                    language,
                    &[
                        ("enabled", "web-status-enabled"),
                        ("by_user", "web-status-paused"),
                    ],
                    Some(status.as_str()),
                ),
                own_postings: yes_no(language, p.receive_own_postings),
                list_copy: yes_no(language, p.receive_list_copy),
            }),
            leave_href: format!("/web/members/{}/leave", m.id),
        });
    }
    Ok(html(&listmngr_web::Account {
        shell: Shell::new(language, "web-title-account", Nav::Account),
        csrf: session.csrf.clone(),
        signed_in: listmngr_i18n::message(
            language,
            "web-account-signed-in",
            &[("name", &u.display_name)],
        ),
        subscriptions,
        pagination: paging.pagination("/web/account", more),
    }))
}

/// The yes/no options of an optional preference, with the resolved value selected.
fn yes_no(language: &str, value: Option<bool>) -> Vec<listmngr_web::Choice> {
    choices(
        language,
        &[("true", "web-yes"), ("false", "web-no")],
        value.map(|v| if v { "true" } else { "false" }),
    )
}
async fn logout(
    State(s): State<AppState>,
    h: HeaderMap,
    Form(f): Form<Csrf>,
) -> ApiResult<Response> {
    let session = write_session(&s, &h, &f.csrf).await?;
    s.db.delete_web_session(&session).await?;
    let mut r = Redirect::to("/web").into_response();
    let (_, secure) = origin(&s)?;
    r.headers_mut().insert(
        header::SET_COOKIE,
        format!(
            "listmngr_session=; Path=/web; HttpOnly; SameSite=Strict; Max-Age=0{}",
            if secure { "; Secure" } else { "" }
        )
        .parse()
        .unwrap(),
    );
    Ok(r)
}

use axum::extract::{Path, Query};
use listmngr_core::ListId;
use listmngr_db::workflows::SubscriptionAction;
async fn list_page(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    h: HeaderMap,
) -> ApiResult<Response> {
    let list = s.db.lists().get(&id).await?;
    if !list.advertised {
        return Err(Error::NotFound("list".into()).into());
    }
    let session = anonymous(&s, &h).await?;
    let language = language(&s, &h);
    let mut r = html(&listmngr_web::ListPage {
        shell: Shell::titled(language, list.display_name.clone(), Nav::Lists),
        description: list.description.clone(),
        info: list.info.clone(),
        archive_href: (list.archive_policy == listmngr_core::ArchivePolicy::Public)
            .then(|| format!("/web/lists/{}/archive", id.as_str())),
        action: format!("/web/lists/{}/request", id.as_str()),
        csrf: session.csrf.clone(),
        confirm_href: format!("/web/lists/{}/confirm", id.as_str()),
    });
    set_cookie(&s, &session, &mut r)?;
    Ok(r)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SubscriptionRequest {
    #[serde(default)]
    csrf: String,
    email: String,
    action: SubscriptionAction,
}
async fn subscription_request(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    h: HeaderMap,
    Form(f): Form<SubscriptionRequest>,
) -> ApiResult<Response> {
    write_session(&s, &h, &f.csrf).await?;
    s.pre_auth_rate
        .check("public-workflows")
        .map_err(|retry_after| ApiError(Error::RateLimited { retry_after }))?;
    s.db.workflows()
        .request(&id, &f.email, f.action, now())
        .await?;
    let mut r = html(&listmngr_web::CheckEmail {
        shell: Shell::new(language(&s, &h), "web-title-check-email", Nav::Lists),
    });
    *r.status_mut() = StatusCode::ACCEPTED;
    Ok(r)
}
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ConfirmationQuery {
    #[serde(default)]
    token: String,
}
async fn confirm_form(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    Query(q): Query<ConfirmationQuery>,
    h: HeaderMap,
) -> ApiResult<Response> {
    if q.token.len() > 128 {
        return Err(Error::Validation("token".into()).into());
    }
    let session = anonymous(&s, &h).await?;
    let language = language(&s, &h);
    let mut r = html(&listmngr_web::ConfirmForm {
        shell: Shell::new(language, "web-title-confirm", Nav::Lists),
        intro: listmngr_i18n::message(language, "web-confirm-intro", &[("list", id.as_str())]),
        action: format!("/web/lists/{}/confirm", id.as_str()),
        csrf: session.csrf.clone(),
        token: q.token,
    });
    set_cookie(&s, &session, &mut r)?;
    Ok(r)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Confirmation {
    #[serde(default)]
    csrf: String,
    token: String,
}
async fn confirm(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    h: HeaderMap,
    Form(f): Form<Confirmation>,
) -> ApiResult<Response> {
    write_session(&s, &h, &f.csrf).await?;
    s.pre_auth_rate
        .check("public-workflows")
        .map_err(|retry_after| ApiError(Error::RateLimited { retry_after }))?;
    s.db.workflows().confirm(&id, &f.token, now()).await?;
    Ok(html(&listmngr_web::Confirmed {
        shell: Shell::new(language(&s, &h), "web-title-confirmed", Nav::Lists),
    }))
}

use listmngr_core::{
    DeliveryMode, DeliveryStatus, Member, MemberId, MemberRole, SubscriptionMode, UserId,
};
async fn own_members(s: &AppState, user: UserId) -> ApiResult<Vec<Member>> {
    let mut members = Vec::new();
    for a in s.db.addresses().by_user(user).await? {
        if a.verified_on.is_none() {
            continue;
        }
        for m in s.db.members().find(&a.email).await? {
            if m.address_id == a.id
                && (m.subscription_mode == SubscriptionMode::AsAddress || m.user_id == Some(user))
                && !members.iter().any(|v: &Member| v.id == m.id)
            {
                members.push(m);
            }
        }
    }
    Ok(members)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreferenceForm {
    #[serde(default)]
    csrf: String,
    delivery_mode: DeliveryMode,
    delivery_status: DeliveryStatus,
    receive_own_postings: Option<bool>,
    receive_list_copy: Option<bool>,
}
async fn preferences(
    State(s): State<AppState>,
    Path(id): Path<MemberId>,
    h: HeaderMap,
    Form(f): Form<PreferenceForm>,
) -> ApiResult<Response> {
    let session = write_session(&s, &h, &f.csrf).await?;
    s.db.browser_preferences_with_options(
        &session,
        id,
        f.delivery_mode,
        f.delivery_status,
        f.receive_own_postings,
        f.receive_list_copy,
    )
    .await?;
    Ok(Redirect::to("/web/account").into_response())
}

async fn can_moderate(s: &AppState, user: UserId, list: &ListId) -> ApiResult<bool> {
    let u = s.db.users().get(user).await?;
    if u.is_server_owner
        && s.db
            .addresses()
            .by_user(user)
            .await?
            .iter()
            .any(|a| a.verified_on.is_some())
    {
        return Ok(true);
    }
    Ok(own_members(s, user)
        .await?
        .iter()
        .any(|m| &m.list_id == list && matches!(m.role, MemberRole::Owner | MemberRole::Moderator)))
}
async fn moderation_index(
    State(s): State<AppState>,
    Query(paging): Query<BrowserPage>,
    h: HeaderMap,
) -> ApiResult<Response> {
    let session = load(&s, &h).await?;
    let language = language(&s, &h);
    let user = session.user_id.ok_or(Error::Authentication)?;
    let lists: Vec<(String, String)> = sqlx::query_as(
        "SELECT l.list_id,l.display_name FROM mailing_lists l WHERE
         EXISTS (SELECT 1 FROM users u JOIN addresses a ON a.user_id=u.id
                 WHERE u.id=$1 AND u.is_server_owner=1 AND a.verified_on IS NOT NULL)
         OR EXISTS (SELECT 1 FROM members m JOIN addresses a ON a.id=m.address_id
                    WHERE m.list_id=l.list_id AND a.user_id=$1 AND a.verified_on IS NOT NULL
                    AND (m.subscription_mode='as_address' OR m.user_id=$1)
                    AND m.role IN ('owner','moderator'))
         ORDER BY l.list_id LIMIT 21 OFFSET $2",
    )
    .bind(user.to_string())
    .bind(paging.offset()?)
    .fetch_all(s.db.pool())
    .await
    .map_err(|error| database_error(&error))?;
    let more = lists.len() > 20;
    let rows = lists
        .into_iter()
        .take(20)
        .map(|(id, name)| listmngr_web::ModerationRow {
            href: format!("/web/lists/{id}/held"),
            label: listmngr_i18n::message(language, "web-moderation-held", &[("name", &name)]),
        })
        .collect();
    Ok(html(&listmngr_web::Moderation {
        shell: Shell::new(language, "web-title-moderation", Nav::Moderation),
        rows,
        pagination: paging.pagination("/web/moderation", more),
    }))
}
async fn held_page(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    Query(paging): Query<BrowserPage>,
    h: HeaderMap,
) -> ApiResult<Response> {
    let session = load(&s, &h).await?;
    let language = language(&s, &h);
    let user = session.user_id.ok_or(Error::Authentication)?;
    if !can_moderate(&s, user, &id).await? {
        return Err(denied());
    }
    let held: Vec<String> = sqlx::query_scalar("SELECT id FROM held_messages WHERE list_id=$1 AND disposition IS NULL ORDER BY hold_date,id LIMIT 21 OFFSET $2")
        .bind(id.as_str()).bind(paging.offset()?).fetch_all(s.db.pool()).await.map_err(|error| database_error(&error))?;
    let more = held.len() > 20;
    let mut items = Vec::new();
    for held_id in held.into_iter().take(20) {
        let item =
            s.db.moderation()
                .get(listmngr_db::moderation::HeldId(
                    held_id.parse().map_err(|_| denied())?,
                ))
                .await?;
        // Bound bytes in SQL, before fetching/decoding a potentially huge raw message.
        let raw: Vec<u8> =
            sqlx::query_scalar("SELECT substr(b.raw,1,65536) FROM message_blobs b JOIN messages m ON m.store_key=b.store_key WHERE m.id=$1")
                .bind(item.message_id.0.to_string())
                .fetch_one(s.db.pool())
                .await
                .map_err(|error| database_error(&error))?;
        items.push(listmngr_web::HeldItem {
            subject: item.subject,
            sender: item.sender,
            reason: item.reason,
            source: String::from_utf8_lossy(&raw).into_owned(),
            action: format!("/web/lists/{}/held/{}", id.as_str(), item.id.0),
        });
    }
    Ok(html(&listmngr_web::Held {
        shell: Shell::new(language, "web-title-held", Nav::Moderation),
        csrf: session.csrf.clone(),
        items,
        decisions: choices(
            language,
            &[
                ("defer", "web-held-defer"),
                ("accept", "web-held-accept"),
                ("reject", "web-held-reject"),
                ("discard", "web-held-discard"),
            ],
            None,
        ),
        pagination: paging.pagination(&format!("/web/lists/{id}/held"), more),
    }))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Review {
    #[serde(default)]
    csrf: String,
    action: String,
    #[serde(default)]
    comment: String,
}
async fn review(
    State(s): State<AppState>,
    Path((id, held)): Path<(ListId, uuid::Uuid)>,
    h: HeaderMap,
    Form(f): Form<Review>,
) -> ApiResult<Response> {
    use listmngr_db::moderation::{HeldId, ReviewAction};
    let session = write_session(&s, &h, &f.csrf).await?;
    let user = session.user_id.ok_or(Error::Authentication)?;
    if !can_moderate(&s, user, &id).await? {
        return Err(denied());
    }
    let held = HeldId(held);
    let item = s.db.moderation().get(held).await?;
    if item.list_id != id {
        return Err(Error::NotFound("held message".into()).into());
    }
    if f.comment.len() > 2000 {
        return Err(Error::Validation("comment too long".into()).into());
    }
    let action = match f.action.as_str() {
        "accept" => ReviewAction::Accept {
            max_attempts: crate::HELD_OUT_MAX_ATTEMPTS,
        },
        "reject" => ReviewAction::Reject,
        "discard" => ReviewAction::Discard,
        "defer" => ReviewAction::Defer,
        _ => return Err(Error::Validation("unsupported decision".into()).into()),
    };
    s.db.browser_review(&session, &id, held, &action, &f.comment)
        .await?;
    Ok(Redirect::to(&format!("/web/lists/{}/held", id.as_str())).into_response())
}

fn database_error(error: &sqlx::Error) -> ApiError {
    ApiError(Error::Database(error.to_string()))
}
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct BrowserPage {
    #[serde(default)]
    page: u32,
}
impl BrowserPage {
    const LAST: u32 = 10_000;
    fn offset(&self) -> ApiResult<i64> {
        if self.page > Self::LAST {
            return Err(Error::Validation("page out of range".into()).into());
        }
        Ok(i64::from(self.page) * 20)
    }
    fn pagination(&self, path: &str, more: bool) -> Pagination {
        Pagination::numbered(path, self.page, more, Self::LAST)
    }
}
