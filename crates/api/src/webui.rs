//! Browser interface. This router is deliberately outside bearer API middleware.
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
use listmngr_web::escape;
use serde::Deserialize;
use std::fmt::Write as _;
fn page(title: &str, body: &str) -> Response {
    Html(listmngr_web::document(title, body)).into_response()
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
) -> ApiResult<Response> {
    let ids: Vec<String> = sqlx::query_scalar(
        "SELECT list_id FROM mailing_lists WHERE advertised=1 ORDER BY list_id LIMIT 21 OFFSET $1",
    )
    .bind(paging.offset()?)
    .fetch_all(s.db.pool())
    .await
    .map_err(|error| database_error(&error))?;
    let more = ids.len() > 20;
    let mut body = String::from("<ul>");
    for id in ids.into_iter().take(20) {
        let l = s.db.lists().get(&id.parse()?).await?;
        if !l.advertised {
            continue;
        }
        write!(
            &mut body,
            "<li><a href=\"/web/lists/{}\">{}</a> — {}<p>{}</p></li>",
            escape(l.id.as_str()),
            escape(&l.display_name),
            escape(l.id.as_str()),
            escape(&l.description)
        )
        .expect("format HTML");
    }
    body.push_str("</ul>");
    body.push_str(&paging.links("/web", more));
    Ok(page("Mailing lists", &body))
}

async fn security_headers(request: Request, next: Next) -> Response {
    let mut r = next.run(request).await;
    if r.status().is_client_error() || r.status().is_server_error() {
        let status = r.status();
        let retry_after = r.headers().get(header::RETRY_AFTER).cloned();
        r = page(
            "Request not completed",
            "<p>The request was invalid, expired or not authorized. No action was completed. <a href=\"/web/login\">Log in again</a> or return to the list and try again.</p>",
        );
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
fn hidden(session: &WebSession) -> String {
    format!(
        "<input type=\"hidden\" name=\"csrf\" value=\"{}\">",
        escape(&session.csrf)
    )
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
    let mut r = page(
        "Log in",
        &format!(
            "<form method=\"post\" action=\"/web/login\">{}<p><label>Email <input type=\"email\" name=\"email\" autocomplete=\"username\" required></label></p><p><label>Password <input type=\"password\" name=\"password\" autocomplete=\"current-password\" required maxlength=\"1024\"></label></p><button>Log in</button></form><p>Account signup and password reset are not available in this interface. Contact the site administrator.</p>",
            hidden(&session)
        ),
    );
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
    let u =
        s.db.users()
            .get(session.user_id.ok_or(Error::Authentication)?)
            .await?;
    let mut body = format!(
        "<p><a href=\"/web/admin\">List administration</a> · <a href=\"/web/moderation\">Moderator queues</a> · <a href=\"/web/account/password\">Change password</a></p><p>Signed in as {}.</p><form method=\"post\" action=\"/web/logout\">{}<button>Log out</button></form>",
        escape(&u.display_name),
        hidden(&session)
    );
    let ids: Vec<String> = sqlx::query_scalar("SELECT m.id FROM members m JOIN addresses a ON a.id=m.address_id WHERE a.user_id=$1 AND a.verified_on IS NOT NULL AND (m.subscription_mode='as_address' OR m.user_id=$1) AND m.role='member' ORDER BY m.list_id,m.id LIMIT 21 OFFSET $2")
        .bind(u.id.to_string()).bind(paging.offset()?).fetch_all(s.db.pool()).await.map_err(|error| database_error(&error))?;
    let more = ids.len() > 20;
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
        write!(
            &mut body,
            "<section><h2>{}</h2><p>Delivery: {}. Status: {}.</p>",
            escape(m.list_id.as_str()),
            escape(&p.delivery_mode.map(|v| v.to_string()).unwrap_or_default()),
            escape(&p.delivery_status.map(|v| v.to_string()).unwrap_or_default())
        )
        .expect("format HTML");
        if list.archive_policy != listmngr_core::ArchivePolicy::Never {
            write!(
                body,
                "<p><a href=\"/web/lists/{}/archive\">Read archive</a></p>",
                escape(m.list_id.as_str())
            )
            .expect("format HTML");
        }
        if matches!(
            p.delivery_status,
            Some(DeliveryStatus::ByModerator | DeliveryStatus::ByBounces | DeliveryStatus::Unknown)
        ) {
            if s.db.browser_recover_preview(&session, m.id).await.is_ok() {
                write!(
                    &mut body,
                    "<p><a href=\"/web/members/{}/recover\">Restore delivery</a></p>",
                    m.id
                )
                .expect("format HTML");
            } else {
                body.push_str("<p>Delivery is restricted. Contact a list administrator.</p>");
            }
        } else {
            write!(&mut body,"<form method=\"post\" action=\"/web/members/{}/preferences\">{}<p><label>Delivery mode <select name=\"delivery_mode\">{}</select></label></p><p><label>Delivery status <select name=\"delivery_status\">{}</select></label></p><p><label>Receive your own posts <select name=\"receive_own_postings\">{}</select></label></p><p><label>Receive list copies when directly addressed <select name=\"receive_list_copy\">{}</select></label></p><button>Save preferences</button></form>",m.id,hidden(&session),options(&[("regular","Individual messages"),("plaintext_digests","Plain text digest"),("mime_digests","MIME digest")],p.delivery_mode.map(|v|v.to_string()).as_deref()),options(&[("enabled","Enabled"),("by_user","Paused by me")],p.delivery_status.map(|v|v.to_string()).as_deref()),options(&[("true","Yes"),("false","No")],p.receive_own_postings.map(|v|v.to_string()).as_deref()),options(&[("true","Yes"),("false","No")],p.receive_list_copy.map(|v|v.to_string()).as_deref())).expect("format HTML");
        }
        write!(
            &mut body,
            "<p><a href=\"/web/members/{}/leave\">Leave list</a></p></section>",
            m.id
        )
        .expect("format HTML");
    }
    body.push_str(&paging.links("/web/account", more));
    Ok(page("My subscriptions", &body))
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
    let id = escape(id.as_str());
    let archive_link = if list.archive_policy == listmngr_core::ArchivePolicy::Public {
        format!("<p><a href=\"/web/lists/{id}/archive\">Browse public archive</a></p>")
    } else {
        String::new()
    };
    let mut r = page(
        &list.display_name,
        &format!(
            "<p>{}</p><p>{}</p>{archive_link}<form method=\"post\" action=\"/web/lists/{id}/request\">{}<p><label>Email <input type=\"email\" name=\"email\" autocomplete=\"email\" required></label></p><p><label>Request <select name=\"action\"><option value=\"join\">Join</option><option value=\"leave\">Leave</option></select></label></p><button>Send confirmation instructions</button></form><p>Mailbox confirmation is required. Requests are limited to one per list and address per hour. Mail delivery must be enabled by the administrator.</p><p><a href=\"/web/lists/{id}/confirm\">Enter a confirmation token from your email</a></p>",
            escape(&list.description),
            escape(&list.info),
            hidden(&session)
        ),
    );
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
    let mut r = page(
        "Check your email",
        "<p>If eligible, confirmation instructions will be sent. Copy the Token from the message into the list’s confirmation form. No membership change has been made yet.</p>",
    );
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
    let mut r = page(
        "Confirm your request",
        &format!(
            "<p>Confirm a join or leave request for {}. Opening this page does not change your subscription.</p><form method=\"post\" action=\"/web/lists/{}/confirm\">{}<p><label>Token from email <input name=\"token\" value=\"{}\" maxlength=\"128\" autocomplete=\"off\" required></label></p><button>Confirm request</button></form>",
            escape(id.as_str()),
            escape(id.as_str()),
            hidden(&session),
            escape(&q.token)
        ),
    );
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
    Ok(page(
        "Request confirmed",
        "<p>Your subscription request has been completed.</p>",
    ))
}

use listmngr_core::{
    DeliveryMode, DeliveryStatus, Member, MemberId, MemberRole, SubscriptionMode, UserId,
};
fn options(values: &[(&str, &str)], selected: Option<&str>) -> String {
    let mut result = String::new();
    for (value, label) in values {
        write!(
            &mut result,
            "<option value=\"{}\"{}>{}</option>",
            escape(value),
            if selected == Some(*value) {
                " selected"
            } else {
                ""
            },
            escape(label)
        )
        .expect("format HTML");
    }
    result
}
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
    let user = session.user_id.ok_or(Error::Authentication)?;
    let mut body = String::from("<ul>");
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
    for (id, name) in lists.into_iter().take(20) {
        write!(
            &mut body,
            "<li><a href=\"/web/lists/{}/held\">{} — held messages</a></li>",
            escape(&id),
            escape(&name)
        )
        .expect("format HTML");
    }
    body.push_str("</ul><p>Only lists you are authorized to moderate are shown.</p>");
    body.push_str(&paging.links("/web/moderation", more));
    Ok(page("Moderator queues", &body))
}
async fn held_page(
    State(s): State<AppState>,
    Path(id): Path<ListId>,
    Query(paging): Query<BrowserPage>,
    h: HeaderMap,
) -> ApiResult<Response> {
    let session = load(&s, &h).await?;
    let user = session.user_id.ok_or(Error::Authentication)?;
    if !can_moderate(&s, user, &id).await? {
        return Err(denied());
    }
    let mut body = String::new();
    let held: Vec<String> = sqlx::query_scalar("SELECT id FROM held_messages WHERE list_id=$1 AND disposition IS NULL ORDER BY hold_date,id LIMIT 21 OFFSET $2")
        .bind(id.as_str()).bind(paging.offset()?).fetch_all(s.db.pool()).await.map_err(|error| database_error(&error))?;
    let more = held.len() > 20;
    if held.is_empty() {
        body.push_str("<p>No messages await review.</p>");
    }
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
        write!(&mut body,"<article><h2>{}</h2><dl><dt>Sender</dt><dd>{}</dd><dt>Reason</dt><dd>{}</dd></dl><details><summary>Message source (first 64 KiB)</summary><pre>{}</pre></details><form method=\"post\" action=\"/web/lists/{}/held/{}\">{}<p><label>Decision <select name=\"action\"><option value=\"defer\">Keep held</option><option value=\"accept\">Accept for delivery</option><option value=\"reject\">Reject</option><option value=\"discard\">Discard</option></select></label></p><p><label>Comment <textarea name=\"comment\" maxlength=\"2000\"></textarea></label></p><button>Apply decision</button></form></article>",escape(&item.subject),escape(&item.sender),escape(&item.reason),escape(&String::from_utf8_lossy(&raw)),escape(id.as_str()),item.id.0,hidden(&session)).expect("format HTML");
    }
    body.push_str("<p>Accept queues regular delivery using the current recipient preferences. Reject and discard record the decision without sending a rejection notice.</p>");
    body.push_str(&paging.links(&format!("/web/lists/{id}/held"), more));
    Ok(page("Held messages", &body))
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
    fn offset(&self) -> ApiResult<i64> {
        if self.page > 10_000 {
            return Err(Error::Validation("page out of range".into()).into());
        }
        Ok(i64::from(self.page) * 20)
    }
    fn links(&self, path: &str, more: bool) -> String {
        let mut html = String::from("<nav aria-label=\"Pagination\">");
        if self.page > 0 {
            write!(
                &mut html,
                "<a href=\"{}?page={}\">Previous page</a> ",
                escape(path),
                self.page - 1
            )
            .expect("format HTML");
        }
        if more && self.page < 10_000 {
            write!(
                &mut html,
                "<a href=\"{}?page={}\">Next page</a>",
                escape(path),
                self.page + 1
            )
            .expect("format HTML");
        }
        html.push_str("</nav>");
        html
    }
}
