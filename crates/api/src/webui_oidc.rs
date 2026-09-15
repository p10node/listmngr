//! Signing in through an `OpenID` Connect provider, and the links a reader
//! keeps under their account. The browser's session carries the ceremony
//! between the redirect out and the callback in; the callback answers only
//! the ceremony this session started, once.
use super::{
    ApiError, ApiResult, AppState, Form, HeaderMap, IntoResponse, Path, Redirect, Response, Shell,
    State, anonymous, denied, html, load, now, reader_language, set_cookie, write_session,
};
use crate::oidc::{Ceremony, Provider};
use axum::extract::Query;
use listmngr_core::Error;
use listmngr_db::VerifiedIdentity;
use listmngr_web::Nav;
use serde::Deserialize;
use subtle::ConstantTimeEq as _;

fn redirect_uri(s: &AppState, provider: &Provider) -> String {
    format!(
        "{}/web/login/oidc/{}/callback",
        s.config.site.base_url.trim_end_matches('/'),
        provider.config.name
    )
}

fn provider<'a>(s: &'a AppState, name: &str) -> ApiResult<&'a Provider> {
    s.oidc
        .get(name)
        .ok_or_else(|| Error::NotFound("identity provider".into()).into())
}

/// The providers the login page offers.
pub(super) fn offered(s: &AppState) -> Vec<listmngr_web::ProviderLink> {
    s.oidc
        .all()
        .into_iter()
        .map(|provider| listmngr_web::ProviderLink {
            href: format!("/web/login/oidc/{}", provider.config.name),
            display_name: provider.config.display_name.clone(),
        })
        .collect()
}

/// Send the browser to the provider, keeping the ceremony on its session.
async fn send_out(
    s: &AppState,
    session: &listmngr_db::web_sessions::WebSession,
    provider: &Provider,
    purpose: &str,
) -> ApiResult<Response> {
    s.pre_auth_rate
        .check("web-oidc")
        .map_err(|retry_after| ApiError(Error::RateLimited { retry_after }))?;
    let (ceremony, authorize) = s
        .oidc
        .begin(provider, &redirect_uri(s, provider), purpose, now())
        .await?;
    let parked = serde_json::to_string(&ceremony).map_err(|_| denied())?;
    s.db.browser_oidc_park(session, &parked, now()).await?;
    Ok(Redirect::to(&authorize).into_response())
}

pub(super) async fn start(
    State(s): State<AppState>,
    Path(name): Path<String>,
    h: HeaderMap,
) -> ApiResult<Response> {
    let provider = provider(&s, &name)?;
    let session = anonymous(&s, &h).await?;
    let mut r = send_out(&s, &session, provider, "login").await?;
    set_cookie(&s, &session, &mut r)?;
    Ok(r)
}

#[derive(Deserialize)]
pub(super) struct Callback {
    #[serde(default)]
    code: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    error: String,
}

pub(super) async fn callback(
    State(s): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<Callback>,
    h: HeaderMap,
) -> ApiResult<Response> {
    let provider = provider(&s, &name)?;
    let session = load_any(&s, &h).await?;
    // Taken once: a replayed callback finds nothing to answer.
    let parked = s.db.browser_oidc_take(&session, now()).await?;
    let ceremony: Ceremony = serde_json::from_str(&parked)
        .map_err(|_| Error::Validation("sign-in with this provider".into()))?;
    if ceremony.provider != name
        || ceremony.expires_at <= now()
        || !bool::from(ceremony.state.as_bytes().ct_eq(q.state.as_bytes()))
    {
        return Err(Error::Validation("this sign-in did not start here".into()).into());
    }
    if !q.error.is_empty() || q.code.is_empty() {
        // The provider's own error code is not shown: it is theirs, not ours.
        return Err(Error::Validation("the provider declined".into()).into());
    }
    s.pre_auth_rate
        .check("web-oidc")
        .map_err(|retry_after| ApiError(Error::RateLimited { retry_after }))?;
    let identity = s
        .oidc
        .complete(
            provider,
            &ceremony,
            &q.code,
            &redirect_uri(&s, provider),
            now() / 1000,
        )
        .await?;
    let identity = VerifiedIdentity {
        provider: name,
        subject: identity.subject,
        email: identity.email,
        email_verified: identity.email_verified,
        name: identity.name,
    };
    if ceremony.purpose == "link" {
        s.db.browser_oidc_link(&session, &identity, now()).await?;
        return Ok(Redirect::to("/web/account/oidc").into_response());
    }
    let language = reader_language(&s, &h, &session).await?;
    let (fresh, next) = match s
        .db
        .browser_oidc_login(&session, &identity, language, now())
        .await?
    {
        listmngr_db::web_sessions::LoginOutcome::Complete(fresh) => (fresh, "/web/account"),
        listmngr_db::web_sessions::LoginOutcome::SecondFactor(pending) => {
            (pending, "/web/login/totp")
        }
    };
    let mut r = Redirect::to(next).into_response();
    set_cookie(&s, &fresh, &mut r)?;
    Ok(r)
}

/// The session as the cookie names it, signed in or not; a callback without
/// one answers no ceremony.
async fn load_any(s: &AppState, h: &HeaderMap) -> ApiResult<listmngr_db::web_sessions::WebSession> {
    match load(s, h).await {
        Ok(session) => Ok(session),
        Err(ApiError(Error::Authentication)) => {
            Err(Error::Validation("no sign-in with this provider is in progress".into()).into())
        }
        Err(error) => Err(error),
    }
}

pub(super) async fn index(State(s): State<AppState>, h: HeaderMap) -> ApiResult<Response> {
    let session = load(&s, &h).await?;
    let shell = Shell::new(
        reader_language(&s, &h, &session).await?,
        "web-title-oidc",
        Nav::Account,
    );
    let methods = s.db.browser_login_methods(&session).await?;
    let last_way_in = methods.link_is_last_way_in() && !methods.links.is_empty();
    let providers = s
        .oidc
        .all()
        .into_iter()
        .map(|provider| {
            let link = methods
                .links
                .iter()
                .find(|link| link.provider == provider.config.name);
            listmngr_web::ProviderRow {
                name: provider.config.name.clone(),
                display_name: provider.config.display_name.clone(),
                linked_email: link.map(|link| link.email.clone()),
                last_used: link.and_then(|link| link.last_used_at).map(|at| {
                    chrono::DateTime::from_timestamp_millis(at)
                        .unwrap_or_default()
                        .format("%Y-%m-%d %H:%M UTC")
                        .to_string()
                }),
            }
        })
        .collect();
    Ok(html(&listmngr_web::Oidc {
        csrf: session.csrf.clone(),
        providers,
        needs_password: methods.usable_password,
        last_way_in,
        shell,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Csrf {
    #[serde(default)]
    csrf: String,
}

pub(super) async fn link(
    State(s): State<AppState>,
    Path(name): Path<String>,
    h: HeaderMap,
    Form(f): Form<Csrf>,
) -> ApiResult<Response> {
    let provider = provider(&s, &name)?;
    let session = write_session(&s, &h, &f.csrf).await?;
    send_out(&s, &session, provider, "link").await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Unlink {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    password: String,
}

pub(super) async fn unlink(
    State(s): State<AppState>,
    Path(name): Path<String>,
    h: HeaderMap,
    Form(f): Form<Unlink>,
) -> ApiResult<Response> {
    provider(&s, &name)?;
    let session = write_session(&s, &h, &f.csrf).await?;
    s.web_login_rate
        .check("web-login")
        .map_err(|retry_after| ApiError(Error::RateLimited { retry_after }))?;
    s.db.browser_oidc_unlink(&session, &name, &f.password)
        .await?;
    Ok(Redirect::to("/web/account/oidc").into_response())
}
