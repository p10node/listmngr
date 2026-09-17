//! The server owner's accounts: a search and one account's page with its
//! display name, server-owner flag, addresses and memberships.
use super::{
    ApiResult, AppState, BrowserPage, Form, HeaderMap, Path, Query, Redirect, Response, Shell,
    State, html, inline_refusal, load, privileged, reader_language, write_session,
};
use axum::response::IntoResponse;
use listmngr_core::{AddressId, Error, MemberRole, UserId};
use listmngr_db::web_users::UserDetail;
use listmngr_web::{Fact, Nav, Pagination, choices};
use serde::Deserialize;

fn t(language: &str, id: &str) -> String {
    listmngr_i18n::message(language, id, &[])
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(super) struct SearchQuery {
    #[serde(default)]
    page: u32,
    #[serde(default)]
    q: String,
}

pub(super) async fn index(
    State(s): State<AppState>,
    Query(f): Query<SearchQuery>,
    h: HeaderMap,
) -> ApiResult<Response> {
    if f.q.len() > 200 {
        return Err(Error::Validation("search".into()).into());
    }
    let session = load(&s, &h).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &h, &session).await?;
    let paging = BrowserPage { page: f.page };
    let rows = s.db.browser_users(&session, &f.q, paging.offset()?).await?;
    let more = rows.len() > 20;
    let filters = if f.q.is_empty() {
        String::new()
    } else {
        serde_urlencoded::to_string([("q", &f.q)])
            .map_err(|_| Error::Validation("search".into()))?
    };
    Ok(html(&listmngr_web::AdminUsers {
        shell: Shell::new(language, "web-title-users", Nav::Account),
        query: f.q.clone(),
        rows: rows
            .into_iter()
            .take(20)
            .map(|row| listmngr_web::AdminUserRow {
                href: format!("/web/admin/users/{}", row.id),
                display_name: row.display_name,
                email: row.email,
                server_owner: row.server_owner,
                created: row.created_at,
            })
            .collect(),
        pagination: Pagination::filtered(
            "/web/admin/users",
            &filters,
            f.page,
            more,
            BrowserPage::LAST,
        ),
    }))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(super) struct Notice {
    #[serde(default)]
    saved: String,
}

fn role_label(language: &str, role: MemberRole) -> String {
    t(
        language,
        match role {
            MemberRole::Owner => "web-badge-owner",
            MemberRole::Moderator => "web-badge-moderator",
            MemberRole::Member => "web-badge-member",
            MemberRole::Nonmember => "web-role-nonmember",
        },
    )
}

fn render(
    language: &str,
    csrf: &str,
    detail: &UserDetail,
    display_name: &str,
    server_owner: bool,
    error: Option<String>,
    notice: Option<String>,
) -> Response {
    let id = detail.user.id;
    html(&listmngr_web::AdminUser {
        shell: Shell::titled(language, detail.user.display_name.clone(), Nav::Account),
        csrf: csrf.to_owned(),
        action: format!("/web/admin/users/{id}"),
        display_name: display_name.to_owned(),
        server_owner: choices(
            language,
            &[("false", "web-no"), ("true", "web-yes")],
            Some(if server_owner { "true" } else { "false" }),
        ),
        error,
        notice,
        facts: vec![
            Fact {
                label: t(language, "web-users-id"),
                value: id.to_string(),
            },
            Fact {
                label: t(language, "web-users-locale"),
                value: detail.user.locale.clone(),
            },
            Fact {
                label: t(language, "web-users-created"),
                value: detail.user.created_at.to_rfc3339(),
            },
        ],
        addresses: detail
            .addresses
            .iter()
            .map(|address| listmngr_web::AdminAddressRow {
                email: address.email.clone(),
                verified: address.verified_on.is_some(),
                action: format!("/web/admin/users/{id}/addresses/{}/verify", address.id),
            })
            .collect(),
        memberships: detail
            .memberships
            .iter()
            .map(|membership| listmngr_web::MembershipRow {
                href: format!(
                    "/web/lists/{}/members/{}",
                    membership.list_id, membership.member_id
                ),
                list_id: membership.list_id.clone(),
                role: role_label(language, membership.role),
                email: membership.email.clone(),
            })
            .collect(),
    })
}

pub(super) async fn user(
    State(s): State<AppState>,
    Path(id): Path<UserId>,
    Query(q): Query<Notice>,
    h: HeaderMap,
) -> ApiResult<Response> {
    let session = load(&s, &h).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &h, &session).await?;
    let detail = s.db.browser_user(&session, id).await?;
    let notice = match q.saved.as_str() {
        "1" => Some(t(language, "web-users-saved")),
        "address" => Some(t(language, "web-users-address-changed")),
        _ => None,
    };
    Ok(render(
        language,
        &session.csrf,
        &detail,
        &detail.user.display_name,
        detail.user.is_server_owner,
        None,
        notice,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UserForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    server_owner: String,
}

pub(super) async fn save(
    State(s): State<AppState>,
    Path(id): Path<UserId>,
    h: HeaderMap,
    Form(form): Form<UserForm>,
) -> ApiResult<Response> {
    let session = write_session(&s, &h, &form.csrf).await?;
    privileged(&s, &session).await?;
    let language = reader_language(&s, &h, &session).await?;
    let detail = s.db.browser_user(&session, id).await?;
    let server_owner = form.server_owner == "true";
    let refusal = match s
        .db
        .browser_user_update(&session, id, &form.display_name, server_owner)
        .await
    {
        Ok(()) => {
            return Ok(Redirect::to(&format!("/web/admin/users/{id}?saved=1")).into_response());
        }
        Err(Error::Validation(message)) if message.contains("last server owner") => {
            "web-users-last-owner"
        }
        Err(Error::Validation(_)) => "web-users-bad-name",
        Err(error) => return Err(error.into()),
    };
    Ok(inline_refusal(render(
        language,
        &session.csrf,
        &detail,
        &form.display_name,
        server_owner,
        Some(t(language, refusal)),
        None,
    )))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct VerifyForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    verified: String,
}

pub(super) async fn verify(
    State(s): State<AppState>,
    Path((id, address)): Path<(UserId, AddressId)>,
    h: HeaderMap,
    Form(form): Form<VerifyForm>,
) -> ApiResult<Response> {
    let session = write_session(&s, &h, &form.csrf).await?;
    privileged(&s, &session).await?;
    let verified = match form.verified.as_str() {
        "true" => true,
        "false" => false,
        _ => return Err(Error::Validation("verified".into()).into()),
    };
    s.db.browser_user_address_verify(&session, id, address, verified)
        .await?;
    Ok(Redirect::to(&format!("/web/admin/users/{id}?saved=address")).into_response())
}
