//! A signed-in reader edits their own display name, interface language and
//! time zone; the interface language then wins over the browser's preference.
use super::{call, csrf, fixture, login_as, seeded_fixture, text, user};
use axum::http::StatusCode;
use listmngr_db::Database;

#[tokio::test]
async fn a_reader_edits_their_own_profile() {
    let (db, app) = fixture().await;
    matrix(db, app).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_profile_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_profile")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 3).await.unwrap();
    db.migrate().await.unwrap();
    let (db, app) = seeded_fixture(db).await;
    matrix(db, app).await;
    schema.drop().await.unwrap();
}

async fn audit_count(db: &Database, action: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action=$1")
        .bind(action)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

fn form(token: &str, name: &str, locale: &str, timezone: &str) -> String {
    serde_urlencoded::to_string([
        ("csrf", token),
        ("display_name", name),
        ("locale", locale),
        ("timezone", timezone),
    ])
    .unwrap()
}

async fn matrix(db: Database, app: axum::Router) {
    let reader = user(&db, "reader@example.com", false).await;
    let cookie = login_as(&app, "reader@example.com").await;
    let response = call(&app, "GET", "/web/account/profile", &cookie, "").await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = text(response).await;
    assert!(html.contains("value=\"reader@example.com\""), "{html}");
    assert!(html.contains("value=\"en\" selected"));
    assert!(html.contains("value=\"UTC\" selected"));
    let token = csrf(&html);
    let before = audit_count(&db, "user.profile").await;

    rejected(&db, &app, &cookie, &token, before).await;

    accepted(&db, &app, &cookie, &token, reader.id, before).await;
}

/// A valid edit persists, audits once, and the shell follows the new
/// interface language even though the browser asks for English.
async fn accepted(
    db: &Database,
    app: &axum::Router,
    cookie: &str,
    token: &str,
    reader: listmngr_core::UserId,
    before: i64,
) {
    let body = form(token, "Bạn đọc <script>", "vi", "Asia/Ho_Chi_Minh");
    let saved = call(app, "POST", "/web/account/profile", cookie, &body).await;
    assert_eq!(saved.status(), StatusCode::SEE_OTHER);
    assert_eq!(audit_count(db, "user.profile").await, before + 1);
    let stored = db.users().get(reader).await.unwrap();
    assert_eq!(stored.display_name, "Bạn đọc <script>");
    assert_eq!(stored.locale, "vi");
    assert_eq!(stored.timezone, "Asia/Ho_Chi_Minh");
    let account = text(call(app, "GET", "/web/account", cookie, "").await).await;
    assert!(account.contains("<html lang=\"vi\""), "{account}");
    assert!(account.contains("Bạn đọc &lt;script&gt;"));
    assert!(!account.contains("My subscriptions"));
    let profile = text(call(app, "GET", "/web/account/profile", cookie, "").await).await;
    assert!(profile.contains("value=\"vi\" selected"));
    assert!(profile.contains("value=\"Asia/Ho_Chi_Minh\" selected"));
    // The public directory, without a session, still follows the browser.
    let public = text(call(app, "GET", "/web", "", "").await).await;
    assert!(public.contains("<html lang=\"en\""));
}

/// Nothing below changes the row or writes an audit event.
async fn rejected(db: &Database, app: &axum::Router, cookie: &str, token: &str, before: i64) {
    for (name, body, status) in [
        ("no csrf", form("", "x", "en", "UTC"), StatusCode::FORBIDDEN),
        (
            "wrong csrf",
            form("wrong", "x", "en", "UTC"),
            StatusCode::FORBIDDEN,
        ),
        (
            "unshipped locale",
            form(token, "x", "fr", "UTC"),
            StatusCode::BAD_REQUEST,
        ),
        (
            "unknown zone",
            form(token, "x", "en", "Mars/Olympus"),
            StatusCode::BAD_REQUEST,
        ),
        (
            "control character",
            form(token, "x\u{7}", "en", "UTC"),
            StatusCode::BAD_REQUEST,
        ),
        (
            "empty name",
            form(token, "   ", "en", "UTC"),
            StatusCode::BAD_REQUEST,
        ),
        (
            "too long",
            form(token, &"n".repeat(257), "en", "UTC"),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let response = call(app, "POST", "/web/account/profile", cookie, &body).await;
        assert_eq!(response.status(), status, "{name}");
    }
    assert_eq!(audit_count(db, "user.profile").await, before);
    let unchanged = text(call(app, "GET", "/web/account/profile", cookie, "").await).await;
    assert!(unchanged.contains("value=\"reader@example.com\""));
    assert!(unchanged.contains("value=\"en\" selected"));
}
