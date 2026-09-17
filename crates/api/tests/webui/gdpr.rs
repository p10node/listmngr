//! Data portability and erasure: a reader downloads everything stored about
//! them; a server owner downloads or erases any account, the last server
//! owner excepted.
use super::{call, csrf, fixture, login_as, member, seeded_fixture, text, user};
use axum::http::StatusCode;
use listmngr_core::MemberRole;
use listmngr_db::Database;

#[tokio::test]
async fn export_and_erase() {
    let (db, app) = fixture().await;
    matrix(db, app).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_gdpr_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_gdpr")
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

async fn audit_count(db: &Database, action: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action=$1")
        .bind(action)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

fn has(html: &str, needle: &str) {
    assert!(html.contains(needle), "expected {needle:?} in: {html}");
}

fn lacks(html: &str, needle: &str) {
    assert!(!html.contains(needle), "unexpected {needle:?} in: {html}");
}

async fn matrix(db: Database, app: axum::Router) {
    let root = user(&db, "root@example.com", true).await;
    let reader = user(&db, "reader@example.com", false).await;
    member(&db, "reader@example.com", MemberRole::Member).await;
    db.tokens()
        .create(reader.id, "reader token", &["lists:read"], None)
        .await
        .unwrap();
    db.domains()
        .add_owner("example.com", reader.id)
        .await
        .unwrap();
    let bystander = user(&db, "bystander@example.com", false).await;
    member(&db, "bystander@example.com", MemberRole::Member).await;
    let root_cookie = login_as(&app, "root@example.com").await;
    let reader_cookie = login_as(&app, "reader@example.com").await;

    own_export(&app, &reader_cookie, &reader.id.to_string()).await;
    admin_export(&app, &root_cookie, &reader_cookie, &reader.id.to_string()).await;
    erase_refusals(
        &db,
        &app,
        &root_cookie,
        &reader_cookie,
        &root.id.to_string(),
        &reader.id.to_string(),
    )
    .await;
    erase(&db, &app, &root_cookie, reader.id, bystander.id).await;
}

/// The reader's own export names everything of theirs and no secret.
async fn own_export(app: &axum::Router, reader: &str, reader_id: &str) {
    let html = text(call(app, "GET", "/web/account", reader, "").await).await;
    has(&html, "/web/account/export.json");
    assert_eq!(
        call(app, "GET", "/web/account/export.json", "", "")
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let response = call(app, "GET", "/web/account/export.json", reader, "").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("application/json")
    );
    has(
        response.headers()["content-disposition"].to_str().unwrap(),
        "attachment; filename=\"listmngr-account.json\"",
    );
    let body = text(response).await;
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["format"], "listmngr-account-export/1");
    assert_eq!(value["account"]["id"], reader_id);
    assert_eq!(value["addresses"][0]["email"], "reader@example.com");
    assert_eq!(value["memberships"][0]["list_id"], "public.example.com");
    assert_eq!(value["memberships"][0]["role"], "member");
    assert_eq!(value["api_tokens"][0]["name"], "reader token");
    assert_eq!(value["domains_owned"][0], "example.com");
    assert_eq!(value["browser_sessions"], 1);
    assert!(
        value["audit_events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["action"] == "member.create"),
        "{body}"
    );
    for secret in [
        "password_hash",
        "token_hash",
        "\"token\":",
        "listmngr_session",
    ] {
        lacks(&body, secret);
    }
    lacks(&body, "bystander@example.com");
}

/// A server owner downloads any account; a plain reader may not.
async fn admin_export(app: &axum::Router, root: &str, reader: &str, reader_id: &str) {
    let path = format!("/web/admin/users/{reader_id}/export.json");
    assert_eq!(
        call(app, "GET", &path, reader, "").await.status(),
        StatusCode::FORBIDDEN
    );
    let response = call(app, "GET", &path, root, "").await;
    assert_eq!(response.status(), StatusCode::OK);
    let value: serde_json::Value = serde_json::from_str(&text(response).await).unwrap();
    assert_eq!(value["account"]["id"], reader_id);
    assert_eq!(
        call(
            app,
            "GET",
            "/web/admin/users/00000000-0000-0000-0000-000000000000/export.json",
            root,
            ""
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    let html = text(
        call(
            app,
            "GET",
            &format!("/web/admin/users/{reader_id}"),
            root,
            "",
        )
        .await,
    )
    .await;
    has(&html, &path);
    has(&html, "Erase this account");
}

/// The address must be typed back; the last server owner cannot be erased;
/// a plain reader cannot erase anyone.
async fn erase_refusals(
    db: &Database,
    app: &axum::Router,
    root: &str,
    reader: &str,
    root_id: &str,
    reader_id: &str,
) {
    let deleted = audit_count(db, "user.delete").await;
    let html = text(
        call(
            app,
            "GET",
            &format!("/web/admin/users/{reader_id}"),
            root,
            "",
        )
        .await,
    )
    .await;
    let token = csrf(&html);
    let response = call(
        app,
        "POST",
        &format!("/web/admin/users/{reader_id}/erase"),
        root,
        &serde_urlencoded::to_string([("csrf", token.as_str()), ("confirm", "wrong@example.com")])
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()["location"],
        format!("/web/admin/users/{reader_id}?erase=mismatch")
    );
    let html = text(
        call(
            app,
            "GET",
            &format!("/web/admin/users/{reader_id}?erase=mismatch"),
            root,
            "",
        )
        .await,
    )
    .await;
    has(&html, "does not belong to this account");
    let response = call(
        app,
        "POST",
        &format!("/web/admin/users/{root_id}/erase"),
        root,
        &serde_urlencoded::to_string([("csrf", token.as_str()), ("confirm", "root@example.com")])
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()["location"],
        format!("/web/admin/users/{root_id}?erase=last-owner")
    );
    let html = text(
        call(
            app,
            "GET",
            &format!("/web/admin/users/{root_id}?erase=last-owner"),
            root,
            "",
        )
        .await,
    )
    .await;
    has(&html, "last server owner");
    assert_eq!(
        call(
            app,
            "POST",
            &format!("/web/admin/users/{reader_id}/erase"),
            reader,
            "confirm=reader%40example.com"
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(audit_count(db, "user.delete").await, deleted);
    assert!(db.users().get(reader_id.parse().unwrap()).await.is_ok());
}

/// The erasure removes everything of the account and nothing of anyone else.
async fn erase(
    db: &Database,
    app: &axum::Router,
    root: &str,
    reader: listmngr_core::UserId,
    bystander: listmngr_core::UserId,
) {
    let html = text(call(app, "GET", &format!("/web/admin/users/{reader}"), root, "").await).await;
    let token = csrf(&html);
    let deleted = audit_count(db, "user.delete").await;
    let response = call(
        app,
        "POST",
        &format!("/web/admin/users/{reader}/erase"),
        root,
        &serde_urlencoded::to_string([("csrf", token.as_str()), ("confirm", "Reader@Example.com")])
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()["location"],
        "/web/admin/users?saved=erased"
    );
    assert_eq!(audit_count(db, "user.delete").await, deleted + 1);
    let by: String = sqlx::query_scalar(
        "SELECT diff FROM audit_log WHERE action='user.delete' ORDER BY at DESC LIMIT 1",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    has(&by, "\"by\":\"administrator\"");
    nothing_remains(db, reader, bystander).await;
    let html = text(call(app, "GET", "/web/admin/users?saved=erased", root, "").await).await;
    has(&html, "The account was erased");
    lacks(&html, "reader@example.com");
    assert_eq!(
        call(app, "GET", &format!("/web/admin/users/{reader}"), root, "")
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
}

/// Nothing of the reader remains; the bystander and their membership do.
async fn nothing_remains(
    db: &Database,
    reader: listmngr_core::UserId,
    bystander: listmngr_core::UserId,
) {
    assert!(db.users().get(reader).await.is_err());
    assert!(db.addresses().get("reader@example.com").await.is_err());

    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM api_tokens WHERE name='reader token'"
        )
        .await,
        0
    );
    assert_eq!(count(db, "SELECT COUNT(*) FROM domain_owners").await, 0);
    assert_eq!(
        count(db, "SELECT COUNT(*) FROM members m JOIN addresses a ON a.id=m.address_id WHERE a.email='reader@example.com'").await,
        0
    );
    assert!(db.users().get(bystander).await.is_ok());
    assert_eq!(
        count(db, "SELECT COUNT(*) FROM members m JOIN addresses a ON a.id=m.address_id WHERE a.email='bystander@example.com'").await,
        1
    );
}
