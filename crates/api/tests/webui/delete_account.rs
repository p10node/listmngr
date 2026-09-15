//! A reader deletes their own account: memberships, addresses, tokens and
//! sessions go with it; other people's data does not.
use super::{call, csrf, fixture, login_as, member, seeded_fixture, text, user};
use axum::http::StatusCode;
use listmngr_core::MemberRole;
use listmngr_db::Database;

#[tokio::test]
async fn a_reader_deletes_their_account() {
    let (db, app) = fixture().await;
    matrix(db, app).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_delete_account_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_delete")
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

async fn attempt(app: &axum::Router, cookie: &str, password: &str) -> StatusCode {
    let page = call(app, "GET", "/web/account/delete", cookie, "").await;
    assert_eq!(page.status(), StatusCode::OK);
    let html = text(page).await;
    let body =
        serde_urlencoded::to_string([("csrf", csrf(&html).as_str()), ("password", password)])
            .unwrap();
    call(app, "POST", "/web/account/delete", cookie, &body)
        .await
        .status()
}

async fn matrix(db: Database, app: axum::Router) {
    let reader = user(&db, "reader@example.com", false).await;
    member(&db, "reader@example.com", MemberRole::Member).await;
    let bystander = user(&db, "bystander@example.com", false).await;
    member(&db, "bystander@example.com", MemberRole::Member).await;
    let only_root = user(&db, "root@example.com", true).await;
    db.tokens()
        .create(reader.id, "to be removed", &["lists:read"], None)
        .await
        .unwrap();
    // The password bucket allows five checks a minute; every attempt below
    // counts, so this test signs in as few times as it can.
    let cookie = login_as(&app, "reader@example.com").await;
    let html = text(call(&app, "GET", "/web/account", &cookie, "").await).await;
    assert!(html.contains("/web/account/delete"), "{html}");
    refused(&db, &app, &cookie, reader.id, only_root.id).await;
    deleted(&db, &app, &cookie, reader.id).await;
    assert!(db.users().get(bystander.id).await.is_ok());
}

/// The last server owner cannot delete themselves; a wrong password or a
/// missing CSRF token deletes nothing.
async fn refused(
    db: &Database,
    app: &axum::Router,
    cookie: &str,
    reader: listmngr_core::UserId,
    only_root: listmngr_core::UserId,
) {
    let root = login_as(app, "root@example.com").await;
    assert_eq!(
        attempt(app, &root, "very secure password").await,
        StatusCode::BAD_REQUEST
    );
    assert!(db.users().get(only_root).await.is_ok());
    assert_eq!(
        attempt(app, cookie, "not the password").await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            app,
            "POST",
            "/web/account/delete",
            cookie,
            "password=very%20secure%20password"
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert!(db.users().get(reader).await.is_ok());
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM audit_log WHERE action='user.delete'"
        )
        .await,
        0
    );
}

/// The real thing: everything of the reader's goes, the cookie is cleared,
/// and the bystander keeps everything.
async fn deleted(db: &Database, app: &axum::Router, cookie: &str, reader: listmngr_core::UserId) {
    let done = call(app, "GET", "/web/account/delete", cookie, "").await;
    let form = serde_urlencoded::to_string([
        ("csrf", csrf(&text(done).await).as_str()),
        ("password", "very secure password"),
    ])
    .unwrap();
    let response = call(app, "POST", "/web/account/delete", cookie, &form).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    assert!(text(response).await.contains("Account deleted"));
    assert!(db.users().get(reader).await.is_err());
    for (sql, expected) in [
        (
            "SELECT COUNT(*) FROM addresses WHERE email='reader@example.com'",
            0,
        ),
        (
            "SELECT COUNT(*) FROM members m JOIN addresses a ON a.id=m.address_id WHERE a.email='reader@example.com'",
            0,
        ),
        (
            "SELECT COUNT(*) FROM api_tokens WHERE name='to be removed'",
            0,
        ),
        (
            "SELECT COUNT(*) FROM audit_log WHERE action='user.delete'",
            1,
        ),
        (
            "SELECT COUNT(*) FROM audit_log WHERE action='member.delete'",
            1,
        ),
        (
            "SELECT COUNT(*) FROM addresses WHERE email='bystander@example.com' AND user_id IS NOT NULL",
            1,
        ),
        (
            "SELECT COUNT(*) FROM members m JOIN addresses a ON a.id=m.address_id WHERE a.email='bystander@example.com'",
            1,
        ),
    ] {
        assert_eq!(count(db, sql).await, expected, "{sql}");
    }
    assert_eq!(
        call(app, "GET", "/web/account", cookie, "").await.status(),
        StatusCode::UNAUTHORIZED
    );
    let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM web_sessions WHERE user_id=$1")
        .bind(reader.to_string())
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(sessions, 0);
}
