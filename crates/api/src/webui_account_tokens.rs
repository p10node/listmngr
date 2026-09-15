//! The reader's own API tokens: mint within their authority, see the secret
//! once, revoke.
use super::{
    ApiResult, AppState, Csrf, Form, HeaderMap, IntoResponse, Path, Redirect, Response, Shell,
    State, html, load, reader_language, write_session,
};
use listmngr_core::Error;
use listmngr_db::TokenRequest;
use listmngr_web::Nav;

fn moment(shell: &Shell, value: Option<&str>, absent: &str) -> String {
    value.map_or_else(
        || shell.t(absent),
        |value| {
            chrono::DateTime::parse_from_rfc3339(value).map_or_else(
                |_| value.to_owned(),
                |at| at.format("%Y-%m-%d %H:%M UTC").to_string(),
            )
        },
    )
}

pub(super) async fn index(State(s): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    let session = load(&s, &headers).await?;
    let language = reader_language(&s, &headers, &session).await?;
    let shell = Shell::new(language, "web-title-tokens", Nav::Account);
    let (tokens, authority) = s.db.browser_tokens(&session).await?;
    let tokens = tokens
        .into_iter()
        .map(|item| listmngr_web::TokenRow {
            bound: item
                .list_id
                .clone()
                .unwrap_or_else(|| shell.t("web-tokens-bound-none")),
            created: moment(&shell, Some(&item.created_at), "web-tokens-never"),
            expires: moment(&shell, item.expires_at.as_deref(), "web-tokens-never"),
            last_used: moment(&shell, item.last_used_at.as_deref(), "web-tokens-unused"),
            revoked: item.revoked_at.is_some(),
            id: item.id,
            name: item.name,
            scopes: item.scopes,
        })
        .collect();
    Ok(html(&listmngr_web::Tokens {
        csrf: session.csrf.clone(),
        tokens,
        scopes: authority
            .scopes()
            .iter()
            .map(|scope| (*scope).to_owned())
            .collect(),
        lists: authority.lists,
        server_owner: authority.server_owner,
        shell,
    }))
}

/// The form carries repeated `scopes` fields, which the typed extractor does
/// not model; the pairs are read directly.
pub(super) async fn create(
    State(s): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> ApiResult<Response> {
    let pairs: Vec<(String, String)> =
        serde_urlencoded::from_str(&body).map_err(|_| Error::Validation("token form".into()))?;
    let mut request = TokenRequest {
        name: String::new(),
        scopes: Vec::new(),
        list_id: None,
        expires_days: None,
    };
    let mut csrf = String::new();
    for (key, value) in pairs {
        match key.as_str() {
            "csrf" => csrf = value,
            "name" => request.name = value,
            "scopes" => {
                if value.len() > 32 || request.scopes.len() >= 16 {
                    return Err(Error::Validation("scopes".into()).into());
                }
                request.scopes.push(value);
            }
            "list_id" => {
                if !value.is_empty() {
                    request.list_id = Some(value.parse()?);
                }
            }
            "expires_days" => {
                if !value.is_empty() {
                    request.expires_days = Some(
                        value
                            .parse()
                            .map_err(|_| Error::Validation("token lifetime".into()))?,
                    );
                }
            }
            _ => return Err(Error::Validation("unknown token field".into()).into()),
        }
    }
    let session = write_session(&s, &headers, &csrf).await?;
    request.scopes.sort();
    request.scopes.dedup();
    let issued = s.db.browser_create_token(&session, &request).await?;
    Ok(html(&listmngr_web::TokenIssued {
        shell: Shell::new(
            reader_language(&s, &headers, &session).await?,
            "web-title-tokens",
            Nav::Account,
        ),
        token: issued.token,
    }))
}

pub(super) async fn revoke(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(form): Form<Csrf>,
) -> ApiResult<Response> {
    if id.len() > 64 || !id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return Err(Error::NotFound("token".into()).into());
    }
    let session = write_session(&s, &headers, &form.csrf).await?;
    s.db.browser_revoke_token(&session, &id).await?;
    Ok(Redirect::to("/web/account/tokens").into_response())
}
