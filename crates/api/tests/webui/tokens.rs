//! A reader mints API tokens for what they may already do, sees the secret
//! once, and revokes them. Scope follows authority: a list owner binds tokens
//! to lists they own; only a server owner mints unbound or administrative ones.
use super::{call, csrf, fixture, login_as, member, seeded_fixture, text, user};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use listmngr_core::MemberRole;
use listmngr_db::Database;
use tower::ServiceExt;

#[tokio::test]
async fn owners_mint_bound_tokens_and_server_owners_mint_unbound_ones() {
    let (db, app) = fixture().await;
    matrix(db, app).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_tokens_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_tokens")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 3).await.unwrap();
    db.migrate().await.unwrap();
    let (db, app) = seeded_fixture(db).await;
    matrix(db, app).await;
    schema.drop().await.unwrap();
}

async fn count(db: &Database, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(db.pool()).await.unwrap()
}

async fn page(app: &axum::Router, cookie: &str) -> String {
    let response = call(app, "GET", "/web/account/tokens", cookie, "").await;
    assert_eq!(response.status(), StatusCode::OK);
    text(response).await
}

/// Create a token through the form; returns the status and page.
async fn create(
    app: &axum::Router,
    cookie: &str,
    name: &str,
    scopes: &[&str],
    list: &str,
    days: &str,
) -> (StatusCode, String) {
    let html = page(app, cookie).await;
    let token = csrf(&html);
    let mut fields = vec![
        ("csrf", token.as_str()),
        ("name", name),
        ("list_id", list),
        ("expires_days", days),
    ];
    for scope in scopes {
        fields.push(("scopes", scope));
    }
    let body = serde_urlencoded::to_string(fields).unwrap();
    let response = call(app, "POST", "/web/account/tokens", cookie, &body).await;
    let status = response.status();
    (status, text(response).await)
}

/// The secret the page shows exactly once.
fn secret(html: &str) -> String {
    html.split("<code>lm_")
        .nth(1)
        .unwrap_or_else(|| panic!("a secret on the page: {html}"))
        .split('<')
        .next()
        .map(|rest| format!("lm_{rest}"))
        .unwrap()
}

async fn api(app: &axum::Router, token: &str, path: &str) -> StatusCode {
    let mut request = Request::builder()
        .method("GET")
        .uri(path)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    request.extensions_mut().insert(axum::extract::ConnectInfo(
        "127.0.0.1:4242".parse::<std::net::SocketAddr>().unwrap(),
    ));
    app.clone().oneshot(request).await.unwrap().status()
}

async fn revoke(app: &axum::Router, cookie: &str, id: &str) -> StatusCode {
    let html = page(app, cookie).await;
    let body = serde_urlencoded::to_string([("csrf", csrf(&html).as_str())]).unwrap();
    call(
        app,
        "POST",
        &format!("/web/account/tokens/{id}/revoke"),
        cookie,
        &body,
    )
    .await
    .status()
}

async fn matrix(db: Database, app: axum::Router) {
    user(&db, "owner@example.com", false).await;
    member(&db, "owner@example.com", MemberRole::Owner).await;
    user(&db, "root@example.com", true).await;
    let owner = login_as(&app, "owner@example.com").await;
    let html = page(&app, &owner).await;
    assert!(
        html.contains("public.example.com"),
        "an owned list is offered: {html}"
    );
    assert!(
        !html.contains("private.example.com"),
        "a list not owned is not: {html}"
    );
    assert!(
        !html.contains("value=\"admin\""),
        "administrative scopes are not offered to a list owner"
    );

    refused(&db, &app, &owner).await;
    let (id, token) = minted(&db, &app, &owner).await;
    revoked(&db, &app, &owner, &id, &token).await;
    unbound_for_server_owner(&db, &app).await;
}

/// Nothing below mints anything.
async fn refused(db: &Database, app: &axum::Router, owner: &str) {
    for (name, scopes, list, days, status) in [
        (
            "admin scope",
            &["admin"][..],
            "public.example.com",
            "30",
            StatusCode::BAD_REQUEST,
        ),
        (
            "system scope",
            &["system:read"][..],
            "public.example.com",
            "30",
            StatusCode::BAD_REQUEST,
        ),
        (
            "unknown scope",
            &["everything"][..],
            "public.example.com",
            "30",
            StatusCode::BAD_REQUEST,
        ),
        (
            "no scope",
            &[][..],
            "public.example.com",
            "30",
            StatusCode::BAD_REQUEST,
        ),
        (
            "list not owned",
            &["members:read"][..],
            "private.example.com",
            "30",
            StatusCode::FORBIDDEN,
        ),
        (
            "no list",
            &["members:read"][..],
            "",
            "30",
            StatusCode::BAD_REQUEST,
        ),
        (
            "blank name",
            &["members:read"][..],
            "public.example.com",
            "30",
            StatusCode::BAD_REQUEST,
        ),
        (
            "too long",
            &["members:read"][..],
            "public.example.com",
            "400",
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let label = if name == "blank name" { "  " } else { name };
        assert_eq!(
            create(app, owner, label, scopes, list, days).await.0,
            status,
            "{name}"
        );
    }
    assert_eq!(count(db, "SELECT COUNT(*) FROM api_tokens").await, 0);
    assert_eq!(
        call(
            app,
            "POST",
            "/web/account/tokens",
            owner,
            "csrf=wrong&name=x&scopes=members:read&list_id=public.example.com&expires_days=30"
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
}

/// A bound token: the secret shown once, working inside its list and refused
/// outside it, listed afterwards without the secret.
async fn minted(db: &Database, app: &axum::Router, owner: &str) -> (String, String) {
    let (status, html) = create(
        app,
        owner,
        "ci reader",
        &["members:read", "lists:read"],
        "public.example.com",
        "30",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{html}");
    let token = secret(&html);
    assert!(token.starts_with("lm_"), "{token}");
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM api_tokens WHERE list_id='public.example.com'"
        )
        .await,
        1
    );
    assert_eq!(count(db, "SELECT COUNT(*) FROM audit_log WHERE action='token.create' AND actor_user_id IS NOT NULL").await, 1);
    assert_eq!(
        api(app, &token, "/api/v1/lists/public.example.com").await,
        StatusCode::OK
    );
    assert_eq!(
        api(app, &token, "/api/v1/lists/private.example.com").await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        api(app, &token, "/api/v1/system/versions").await,
        StatusCode::FORBIDDEN,
        "a scope the token was not given"
    );
    let listed = page(app, owner).await;
    assert!(listed.contains("ci reader"), "{listed}");
    assert!(!listed.contains(&token), "the secret is shown once");
    let id: String = sqlx::query_scalar("SELECT id FROM api_tokens WHERE name='ci reader'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(listed.contains(&format!("/web/account/tokens/{id}/revoke")));
    (id, token)
}

/// Revocation ends the token at once; a stranger's token is not ours.
async fn revoked(db: &Database, app: &axum::Router, owner: &str, id: &str, token: &str) {
    let root = login_as(app, "root@example.com").await;
    assert_eq!(
        revoke(app, &root, id).await,
        StatusCode::NOT_FOUND,
        "another user's token"
    );
    assert_eq!(
        api(app, token, "/api/v1/lists/public.example.com").await,
        StatusCode::OK
    );
    assert_eq!(revoke(app, owner, id).await, StatusCode::SEE_OTHER);
    assert_eq!(
        api(app, token, "/api/v1/lists/public.example.com").await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(count(db, "SELECT COUNT(*) FROM audit_log WHERE action='token.revoke' AND actor_user_id IS NOT NULL").await, 1);
    assert_eq!(
        revoke(app, owner, id).await,
        StatusCode::NOT_FOUND,
        "already revoked"
    );
    let listed = page(app, owner).await;
    assert!(listed.contains("Revoked"), "{listed}");
}

/// A server owner mints unbound tokens with administrative scopes.
async fn unbound_for_server_owner(db: &Database, app: &axum::Router) {
    let root = login_as(app, "root@example.com").await;
    let html = page(app, &root).await;
    assert!(html.contains("value=\"admin\""), "{html}");
    let (status, html) = create(app, &root, "automation", &["admin"], "", "7").await;
    assert_eq!(status, StatusCode::OK, "{html}");
    let token = secret(&html);
    assert_eq!(api(app, &token, "/api/v1/domains").await, StatusCode::OK);
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM api_tokens WHERE list_id IS NULL AND expires_at IS NOT NULL"
        )
        .await,
        1
    );
}
