//! A signed-in reader can see and end their own browser sessions.
use super::{call, csrf, fixture, login_as, seeded_fixture, text, user};
use axum::http::StatusCode;
use listmngr_core::UserId;
use listmngr_db::Database;

#[tokio::test]
async fn a_reader_sees_and_revokes_their_own_sessions() {
    let (db, app) = fixture().await;
    matrix(db, app).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_session_inventory_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_sessions")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 3).await.unwrap();
    db.migrate().await.unwrap();
    let (db, app) = seeded_fixture(db).await;
    matrix(db, app).await;
    schema.drop().await.unwrap();
}

async fn matrix(db: Database, app: axum::Router) {
    let reader = user(&db, "reader@example.com", false).await;
    user(&db, "other@example.com", false).await;
    let first = login_as(&app, "reader@example.com").await;
    let second = login_as(&app, "reader@example.com").await;
    let stranger = login_as(&app, "other@example.com").await;
    let html = inventory(&app, [&first, &second, &stranger]).await;
    isolation(&db, &app, &second, &stranger, &html).await;
    revocation(&db, &app, &first, &second, reader.id, &html).await;
}

/// The page lists this reader's own sessions, marks the one being used, and
/// never shows a credential.
async fn inventory(app: &axum::Router, cookies: [&str; 3]) -> String {
    let response = call(app, "GET", "/web/account/sessions", cookies[1], "").await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = text(response).await;
    assert!(html.contains("This browser"), "{html}");
    for cookie in cookies {
        let value = cookie.trim_start_matches("listmngr_session=");
        assert!(!html.contains(value), "a session token reached the page");
    }
    assert_eq!(
        session_ids(&html).len(),
        2,
        "both of this reader's sessions: {html}"
    );
    html
}

/// Another user's session is neither listed nor revocable by id, and a write
/// without the session's own CSRF token changes nothing.
async fn isolation(db: &Database, app: &axum::Router, second: &str, stranger: &str, html: &str) {
    let page = text(call(app, "GET", "/web/account/sessions", stranger, "").await).await;
    let ids = session_ids(&page);
    assert_eq!(ids.len(), 1);
    let before = audit_count(db, "web.session.revoke").await;
    let form = serde_urlencoded::to_string([("csrf", csrf(html).as_str())]).unwrap();
    let denied = call(
        app,
        "POST",
        &format!("/web/account/sessions/{}/revoke", ids[0]),
        second,
        &form,
    )
    .await;
    assert_eq!(denied.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        call(app, "GET", "/web/account", stranger, "")
            .await
            .status(),
        StatusCode::OK,
        "the stranger's session survived"
    );
    let target = other_id(html);
    for body in ["", "csrf=wrong"] {
        let response = call(
            app,
            "POST",
            &format!("/web/account/sessions/{target}/revoke"),
            second,
            body,
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    assert_eq!(audit_count(db, "web.session.revoke").await, before);
}

/// Revoking ends exactly the named session; ending every other session leaves
/// only the browser that asked.
async fn revocation(
    db: &Database,
    app: &axum::Router,
    first: &str,
    second: &str,
    reader: UserId,
    html: &str,
) {
    let before = audit_count(db, "web.session.revoke").await;
    let form = serde_urlencoded::to_string([("csrf", csrf(html).as_str())]).unwrap();
    let target = other_id(html);
    let revoked = call(
        app,
        "POST",
        &format!("/web/account/sessions/{target}/revoke"),
        second,
        &form,
    )
    .await;
    assert_eq!(revoked.status(), StatusCode::SEE_OTHER);
    assert_eq!(audit_count(db, "web.session.revoke").await, before + 1);
    assert_eq!(
        call(app, "GET", "/web/account", first, "").await.status(),
        StatusCode::UNAUTHORIZED,
        "the revoked session is gone"
    );
    assert_eq!(
        call(app, "GET", "/web/account", second, "").await.status(),
        StatusCode::OK,
        "the session that did the revoking still works"
    );
    let third = login_as(app, "reader@example.com").await;
    let page = text(call(app, "GET", "/web/account/sessions", &third, "").await).await;
    assert_eq!(session_ids(&page).len(), 2);
    let form = serde_urlencoded::to_string([("csrf", csrf(&page).as_str())]).unwrap();
    let response = call(
        app,
        "POST",
        "/web/account/sessions/revoke-others",
        &third,
        &form,
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        call(app, "GET", "/web/account", second, "").await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(app, "GET", "/web/account", &third, "").await.status(),
        StatusCode::OK
    );
    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM web_sessions WHERE user_id=$1")
        .bind(reader.to_string())
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(remaining, 1);
}

async fn audit_count(db: &Database, action: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action=$1")
        .bind(action)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

/// Every listed session: its id and whether the page marks it as the browser
/// making the request. One `<section>` per session.
fn listed(html: &str) -> Vec<(String, bool)> {
    html.split("<section")
        .skip(1)
        .filter_map(|section| {
            let id = section
                .split("/web/account/sessions/")
                .nth(1)?
                .split("/revoke")
                .next()?
                .to_owned();
            Some((id, section.contains("This browser")))
        })
        .collect()
}

fn session_ids(html: &str) -> Vec<String> {
    listed(html).into_iter().map(|(id, _)| id).collect()
}

/// The id of a listed session other than the one making the request.
fn other_id(html: &str) -> String {
    listed(html)
        .into_iter()
        .find_map(|(id, current)| (!current).then_some(id))
        .expect("a session other than this browser")
}
