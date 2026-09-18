//! Browser interface. This router is deliberately outside bearer API middleware.
//!
//! Handlers build view models; `listmngr_web` owns every byte of markup and
//! escapes every value at compile time (ADR-0004). No handler concatenates
//! HTML, and no page loads a third-party asset.
#[path = "webui_account_addresses.rs"]
mod account_addresses;
#[path = "webui_account_delete.rs"]
mod account_delete;
#[path = "webui_account_profile.rs"]
mod account_profile;
#[path = "webui_account_sessions.rs"]
mod account_sessions;
#[path = "webui_account_tokens.rs"]
mod account_tokens;
#[path = "webui_admin.rs"]
mod admin;
#[path = "webui_archive.rs"]
mod archive;
#[path = "webui_archive_interact.rs"]
mod archive_interact;
#[path = "webui_archive_post.rs"]
mod archive_post;
#[path = "webui_domains.rs"]
mod domains;
#[path = "webui_gdpr.rs"]
mod gdpr;
#[path = "webui_list_settings.rs"]
mod list_settings;
#[path = "webui_lists.rs"]
mod lists;
#[path = "webui_members.rs"]
mod members;
#[path = "webui_membership.rs"]
mod membership;
#[path = "webui_moderation.rs"]
mod moderation;
#[path = "webui_oidc.rs"]
mod oidc;
#[path = "webui_passkeys.rs"]
mod passkeys;
#[path = "webui_password.rs"]
mod password;
#[path = "webui_recovery.rs"]
mod recovery;
#[path = "webui_reset.rs"]
mod reset;
#[path = "webui_settings.rs"]
mod settings;
#[path = "webui_signup.rs"]
mod signup;
#[path = "webui_system.rs"]
mod system;
#[path = "webui_totp.rs"]
mod totp;
#[path = "webui_users.rs"]
mod users;
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

/// A page that loads the first-party passkey script: the middleware's default
/// CSP allows no script at all, so this one allows `'self'` scripts and the
/// same-origin fetches the ceremonies make.
fn scripted(mut response: Response) -> Response {
    response.headers_mut().insert(
        axum::http::HeaderName::from_static("content-security-policy"),
        axum::http::HeaderValue::from_static(
            "default-src 'none'; script-src 'self'; connect-src 'self'; style-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'",
        ),
    );
    response
}

/// The reader's language: their browser's ordered preferences, then the site
/// default, then English. Nothing else — never the list, never a forwarded
/// header — chooses the document language.
/// The language for a page a signed-in reader opens: the interface language
/// from their profile when it names a shipped catalog, else what
/// [`language`] would negotiate for the browser alone.
async fn reader_language(
    s: &AppState,
    headers: &HeaderMap,
    session: &WebSession,
) -> ApiResult<&'static str> {
    let negotiated = language(s, headers);
    let chosen = s.db.browser_locale(session).await?;
    Ok(chosen
        .as_deref()
        .and_then(listmngr_i18n::supported_match)
        .unwrap_or(negotiated))
}

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

/// The first-party assets, each served from this origin with its type.
fn assets() -> Router<AppState> {
    fn asset(kind: &'static str, body: &'static str) -> Router<AppState> {
        Router::new().route(
            "/",
            get(move || async move { ([(header::CONTENT_TYPE, kind)], body) }),
        )
    }
    Router::new()
        .nest(
            "/web/style.css",
            asset("text/css; charset=utf-8", listmngr_web::STYLESHEET),
        )
        .nest(
            "/web/passkeys.js",
            asset(
                "text/javascript; charset=utf-8",
                listmngr_web::PASSKEYS_SCRIPT,
            ),
        )
        .nest(
            "/web/moderation.js",
            asset(
                "text/javascript; charset=utf-8",
                listmngr_web::MODERATION_SCRIPT,
            ),
        )
        .nest(
            "/web/htmx.min.js",
            asset("text/javascript; charset=utf-8", listmngr_web::HTMX),
        )
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/web", get(lists::directory))
        .route(
            "/web/lists/new",
            get(lists::create_form).post(lists::create),
        )
        .merge(assets())
        .route("/web/login", get(login_form).post(login))
        .route("/web/login/totp", get(totp::login_form).post(totp::login))
        .route("/web/login/oidc/{name}", get(oidc::start))
        .route("/web/login/oidc/{name}/callback", get(oidc::callback))
        .route("/web/login/passkey/start", post(passkeys::login_start))
        .route(
            "/web/login/passkey/finish",
            post(passkeys::login_finish).layer(DefaultBodyLimit::max(65_536)),
        )
        .merge(account_routes())
        .route("/web/signup", get(signup::form).post(signup::create))
        .route("/web/verify", get(signup::verify_form).post(signup::verify))
        .route("/web/reset", get(reset::form).post(reset::request))
        .route(
            "/web/reset/confirm",
            get(reset::confirm_form).post(reset::confirm),
        )
        .route("/web/account", get(account))
        .route("/web/admin", get(admin::index))
        .merge(site_admin_routes())
        .route(
            "/web/lists/{id}/settings",
            get(settings::form).post(settings::save),
        )
        .merge(list_settings::routes())
        .route("/web/lists/{id}/members", get(members::roster))
        .route(
            "/web/lists/{id}/members/subscribe",
            get(members::mass_form)
                .post(members::mass_subscribe)
                .layer(DefaultBodyLimit::max(1_048_576)),
        )
        .route("/web/lists/{id}/members/remove", post(members::mass_remove))
        .route("/web/lists/{id}/members/export.csv", get(members::export))
        .route(
            "/web/lists/{id}/members/{member}",
            get(members::member).post(members::member_save),
        )
        .route(
            "/web/lists/{id}/members/{member}/policy",
            post(members::policy),
        )
        .route(
            "/web/lists/{id}/members/{member}/bounce/reset",
            post(members::bounce_reset),
        )
        .route(
            "/web/lists/{id}/members/{member}/remove",
            post(members::member_remove),
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
        .route("/web/lists/{id}", get(lists::list_page))
        .merge(archive_routes())
        .route("/web/lists/{id}/request", post(subscription_request))
        .merge(moderation::routes())
        .route("/web/lists/{id}/confirm", get(confirm_form).post(confirm))
        .layer(DefaultBodyLimit::max(8192))
        .layer(middleware::from_fn(security_headers))
}

/// The archive: reading pages, feeds, the avatar proxy, and readers'
/// interactions.
fn archive_routes() -> Router<AppState> {
    // The posting form carries a message body, so it alone takes more
    // than the 8 KiB every other browser form is held to; the handler
    // still refuses bodies over its own limit.
    let posting = Router::new()
        .route(
            "/web/lists/{id}/archive/post",
            get(archive_post::form).post(archive_post::submit),
        )
        .layer(DefaultBodyLimit::max(96 * 1024));
    Router::new()
        .merge(posting)
        .route("/web/lists/{id}/archive", get(archive::browse))
        .route("/web/lists/{id}/archive/overview", get(archive::overview))
        .route("/web/lists/{id}/archive/threads", get(archive::threads))
        .route(
            "/web/lists/{id}/archive/threads/{year}/{month}",
            get(archive::threads_month),
        )
        .route(
            "/web/lists/{id}/archive/thread/{thread}",
            get(archive::thread_page),
        )
        .route(
            "/web/lists/{id}/archive/senders/{digest}",
            get(archive::sender),
        )
        .route("/web/lists/{id}/archive/favorites", get(archive::favorites))
        .route("/web/lists/{id}/archive/tags/{tag}", get(archive::tagged))
        .route(
            "/web/lists/{id}/archive/categories/{name}",
            get(archive::in_category),
        )
        .route("/web/lists/{id}/archive/vote", post(archive_interact::vote))
        .route("/web/lists/{id}/archive/tags", post(archive_interact::tag))
        .route(
            "/web/lists/{id}/archive/category",
            post(archive_interact::category),
        )
        .route(
            "/web/lists/{id}/archive/favorite",
            post(archive_interact::favorite),
        )
        .route("/web/lists/{id}/archive/feed.atom", get(archive::feed_atom))
        .route("/web/lists/{id}/archive/feed.rss", get(archive::feed_rss))
        .route(
            "/web/lists/{id}/archive/attachments/{hash}/{position}",
            get(archive::attachment),
        )
        .route("/web/lists/{id}/archive/reattach", post(archive::reattach))
        .route("/web/gravatar/{hash}", get(archive::gravatar))
}

/// The server owner's site-wide pages: domains, the system page and the
/// audit log, accounts.
fn site_admin_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/web/admin/domains",
            get(domains::index).post(domains::create),
        )
        .route("/web/admin/domains/{host}", get(domains::domain))
        .route("/web/admin/domains/{host}/owners", post(domains::owner_add))
        .route(
            "/web/admin/domains/{host}/owners/{user}/remove",
            post(domains::owner_remove),
        )
        .route("/web/admin/domains/{host}/delete", post(domains::delete))
        .route(
            "/web/admin/domains/{host}/templates/{name}",
            get(domains::template_editor)
                .post(domains::template_save)
                .layer(DefaultBodyLimit::max(131_072)),
        )
        .route(
            "/web/admin/domains/{host}/templates/{name}/remove",
            post(domains::template_remove),
        )
        .route("/web/admin/system", get(system::index))
        .route("/web/admin/system/audit", get(system::audit))
        .route("/web/admin/users", get(users::index))
        .route("/web/admin/users/{id}/export.json", get(gdpr::admin_export))
        .route("/web/admin/users/{id}/erase", post(gdpr::erase))
        .route("/web/admin/users/{id}", get(users::user).post(users::save))
        .route(
            "/web/admin/users/{id}/addresses/{address}/verify",
            post(users::verify),
        )
}

/// Self-service pages under `/web/account/…`: second factor, profile,
/// addresses, tokens, sessions, password and deletion.
fn account_routes() -> Router<AppState> {
    Router::new()
        .route("/web/account/oidc", get(oidc::index))
        .route("/web/account/oidc/{name}/link", post(oidc::link))
        .route("/web/account/oidc/{name}/unlink", post(oidc::unlink))
        .route("/web/account/passkeys", get(passkeys::index))
        .route(
            "/web/account/passkeys/register/start",
            post(passkeys::register_start),
        )
        .route(
            "/web/account/passkeys/register/finish",
            post(passkeys::register_finish).layer(DefaultBodyLimit::max(65_536)),
        )
        .route("/web/account/passkeys/{id}/remove", post(passkeys::remove))
        .route("/web/account/totp", get(totp::status))
        .route("/web/account/totp/confirm", post(totp::confirm))
        .route("/web/account/totp/recovery", post(totp::recovery))
        .route("/web/account/totp/disable", post(totp::disable))
        .route(
            "/web/account/password",
            get(password::form).post(password::change),
        )
        .route(
            "/web/account/profile",
            get(account_profile::form).post(account_profile::save),
        )
        .route(
            "/web/account/addresses",
            get(account_addresses::index).post(account_addresses::add),
        )
        .route(
            "/web/account/addresses/{id}/primary",
            post(account_addresses::primary),
        )
        .route(
            "/web/account/addresses/{id}/remove",
            post(account_addresses::remove),
        )
        .route(
            "/web/account/tokens",
            get(account_tokens::index).post(account_tokens::create),
        )
        .route(
            "/web/account/tokens/{id}/revoke",
            post(account_tokens::revoke),
        )
        .route(
            "/web/account/delete",
            get(account_delete::form).post(account_delete::delete),
        )
        .route("/web/account/sessions", get(account_sessions::index))
        .route("/web/account/export.json", get(gdpr::own_export))
        .route(
            "/web/account/sessions/revoke-others",
            post(account_sessions::revoke_others),
        )
        .route(
            "/web/account/sessions/{id}/revoke",
            post(account_sessions::revoke),
        )
}

/// Marks a 400 response whose body is the form itself, refusals inline, so
/// `security_headers` leaves it alone. Never reaches the browser.
const INLINE_REFUSAL: &str = "x-listmngr-inline-refusal";

/// The form again, with its refusals inline, as a 400.
fn inline_refusal(mut response: Response) -> Response {
    *response.status_mut() = StatusCode::BAD_REQUEST;
    response
        .headers_mut()
        .insert(INLINE_REFUSAL, header::HeaderValue::from_static("1"));
    response
}

async fn security_headers(request: Request, next: Next) -> Response {
    let language = request
        .headers()
        .get(header::ACCEPT_LANGUAGE)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
        .unwrap_or_default();
    let mut r = next.run(request).await;
    // A form rendered again with its refusals inline keeps its body; every
    // other failure shows the one page that says nothing about the cause.
    let inline = r.headers_mut().remove(INLINE_REFUSAL).is_some();
    if (r.status().is_client_error() && !inline) || r.status().is_server_error() {
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
    ] {
        r.headers_mut()
            .insert(axum::http::HeaderName::from_static(k), v.parse().unwrap());
    }
    // A page that loads the first-party script sets its own, wider policy
    // (`scripted`); everything else forbids scripts entirely.
    let csp = axum::http::HeaderName::from_static("content-security-policy");
    if !r.headers().contains_key(&csp) {
        r.headers_mut().insert(
            csp,
            axum::http::HeaderValue::from_static(
                "default-src 'none'; style-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'",
            ),
        );
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
/// Expire the session cookie, so a browser whose session just ended stops
/// presenting a credential the server has already deleted.
fn clear_cookie(s: &AppState, response: &mut Response) -> ApiResult<()> {
    let (_, secure) = origin(s)?;
    response.headers_mut().insert(
        header::SET_COOKIE,
        format!(
            "listmngr_session=; Path=/web; HttpOnly; SameSite=Strict; Max-Age=0{}",
            if secure { "; Secure" } else { "" }
        )
        .parse()
        .map_err(|_| denied())?,
    );
    Ok(())
}
/// Pages that administer or moderate stay closed to a reader the site's
/// policy requires to enrol a second factor until they do.
async fn privileged(s: &AppState, session: &WebSession) -> ApiResult<()> {
    if s.db
        .browser_second_factor_missing(session, &s.config.security.require_2fa_for)
        .await?
    {
        return Err(Error::Forbidden("second factor required".into()).into());
    }
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
    let mut r = scripted(html(&listmngr_web::Login {
        shell: Shell::new(
            reader_language(&s, &h, &session).await?,
            "web-title-login",
            Nav::Login,
        )
        .with_scripts(),
        csrf: session.csrf.clone(),
        signup: s.config.web.signup,
        providers: oidc::offered(&s),
    }));
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
    let (fresh, next) = match s.db.browser_login(&f.email, &f.password, &session).await? {
        listmngr_db::web_sessions::LoginOutcome::Complete(fresh) => (fresh, "/web/account"),
        listmngr_db::web_sessions::LoginOutcome::SecondFactor(pending) => {
            (pending, "/web/login/totp")
        }
    };
    let mut r = Redirect::to(next).into_response();
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
    let language = reader_language(&s, &headers, &session).await?;
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
    let second_factor_missing =
        s.db.browser_second_factor_missing(&session, &s.config.security.require_2fa_for)
            .await?;
    Ok(html(&listmngr_web::Account {
        shell: Shell::new(language, "web-title-account", Nav::Account),
        csrf: session.csrf.clone(),
        second_factor_missing,
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
    clear_cookie(&s, &mut r)?;
    Ok(r)
}

use axum::extract::{Path, Query};
use listmngr_core::ListId;
use listmngr_db::workflows::SubscriptionAction;
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
    let language = reader_language(&s, &h, &session).await?;
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

use listmngr_core::{DeliveryMode, DeliveryStatus, MemberId};
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
