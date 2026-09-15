//! A person who forgot their password proves the mailbox again and chooses a
//! new one. The request page never says whether the address has an account.
use super::{call, cookie, csrf, fixture, login_as, seeded_fixture, text, user};
use axum::http::StatusCode;
use listmngr_db::Database;

const OLD: &str = "very secure password";
const NEW: &str = "lantern-orchard-gravel-2026";

#[tokio::test]
async fn request_confirm_and_login_with_the_new_password() {
    let (db, app) = fixture().await;
    matrix(db, app).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_reset_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_reset")
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

async fn anonymous(app: &axum::Router, path: &str) -> (String, String) {
    let response = call(app, "GET", path, "", "").await;
    assert_eq!(response.status(), StatusCode::OK, "{path}");
    let session = cookie(&response);
    (session, csrf(&text(response).await))
}

async fn request(app: &axum::Router, email: &str) -> StatusCode {
    let (session, token) = anonymous(app, "/web/reset").await;
    let body = serde_urlencoded::to_string([("csrf", token.as_str()), ("email", email)]).unwrap();
    call(app, "POST", "/web/reset", &session, &body)
        .await
        .status()
}

async fn login(app: &axum::Router, email: &str, password: &str) -> StatusCode {
    let (session, token) = anonymous(app, "/web/login").await;
    let body = serde_urlencoded::to_string([
        ("csrf", token.as_str()),
        ("email", email),
        ("password", password),
    ])
    .unwrap();
    call(app, "POST", "/web/login", &session, &body)
        .await
        .status()
}

/// The reset token from the newest queued message.
async fn mailed_token(db: &Database) -> String {
    let raw: Vec<u8> = sqlx::query_scalar(
        "SELECT b.raw FROM message_blobs b JOIN messages m ON m.store_key=b.store_key ORDER BY m.created_at DESC LIMIT 1",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    let mail = String::from_utf8(raw).unwrap();
    assert!(mail.contains("Subject: Reset your password for"), "{mail}");
    assert!(mail.contains("/web/reset/confirm"));
    mail.split("enter this token:")
        .nth(1)
        .expect("token line")
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .expect("a token")
        .to_owned()
}

async fn confirm(app: &axum::Router, token: &str, password: &str, again: &str) -> StatusCode {
    let (session, csrf_token) = anonymous(app, "/web/reset/confirm").await;
    let body = serde_urlencoded::to_string([
        ("csrf", csrf_token.as_str()),
        ("token", token),
        ("password", password),
        ("confirm_password", again),
    ])
    .unwrap();
    call(app, "POST", "/web/reset/confirm", &session, &body)
        .await
        .status()
}

async fn matrix(db: Database, app: axum::Router) {
    let reader = user(&db, "reader@example.com", false).await;
    let login_page = text(call(&app, "GET", "/web/login", "", "").await).await;
    assert!(login_page.contains("/web/reset"), "{login_page}");
    // A live session from before the reset, to be revoked by it.
    let earlier = login_as(&app, "reader@example.com").await;
    let token = requested(&db, &app).await;
    refused(&db, &app, &token).await;
    completed(&db, &app, &token, reader.id, &earlier).await;
}

/// Unknown and unverified addresses are accepted and mail nothing; a verified
/// account gets one mail and one live token, and a second request within the
/// hour mails nothing more.
async fn requested(db: &Database, app: &axum::Router) -> String {
    db.users()
        .create(listmngr_db::NewUser {
            display_name: "Unproven".into(),
            email: "unproven@example.com".into(),
            password: OLD.into(),
            server_owner: false,
        })
        .await
        .unwrap();
    for email in ["nobody@example.com", "unproven@example.com"] {
        assert_eq!(request(app, email).await, StatusCode::ACCEPTED, "{email}");
    }
    assert_eq!(count(db, "SELECT COUNT(*) FROM workflow_notices").await, 0);
    assert_eq!(count(db, "SELECT COUNT(*) FROM account_tokens").await, 0);
    for email in ["Reader@Example.com", "reader@example.com"] {
        assert_eq!(request(app, email).await, StatusCode::ACCEPTED);
    }
    for (sql, expected) in [
        ("SELECT COUNT(*) FROM workflow_notices", 1),
        (
            "SELECT COUNT(*) FROM account_tokens WHERE purpose='password_reset' AND consumed_at IS NULL",
            1,
        ),
        (
            "SELECT COUNT(*) FROM audit_log WHERE action='user.reset.request'",
            1,
        ),
    ] {
        assert_eq!(count(db, sql).await, expected, "{sql}");
    }
    mailed_token(db).await
}

/// The confirmation page prefills the token; a wrong token, a weak or a
/// mismatched password change nothing and leave the token live.
async fn refused(db: &Database, app: &axum::Router, token: &str) {
    let path = format!("/web/reset/confirm?token={token}");
    let page = text(call(app, "GET", &path, "", "").await).await;
    assert!(page.contains(&format!("value=\"{token}\"")));
    for (name, presented, password, again) in [
        ("wrong token", "wrong", NEW, NEW),
        ("weak password", token, "password", "password"),
        ("mismatch", token, NEW, "different one 2026"),
    ] {
        assert_eq!(
            confirm(app, presented, password, again).await,
            StatusCode::BAD_REQUEST,
            "{name}"
        );
    }
    assert_eq!(
        count(db, "SELECT COUNT(*) FROM account_tokens WHERE purpose='password_reset' AND consumed_at IS NULL").await,
        1,
        "a refused attempt does not burn the token"
    );
    assert_eq!(
        login(app, "reader@example.com", OLD).await,
        StatusCode::SEE_OTHER
    );
}

/// The right token with a strong password: the new password works, the old
/// one and every earlier session stop working, one audit row, token used up.
async fn completed(
    db: &Database,
    app: &axum::Router,
    token: &str,
    reader: listmngr_core::UserId,
    earlier: &str,
) {
    assert_eq!(confirm(app, token, NEW, NEW).await, StatusCode::OK);
    assert_eq!(
        login(app, "reader@example.com", OLD).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login(app, "reader@example.com", NEW).await,
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        call(app, "GET", "/web/account", earlier, "").await.status(),
        StatusCode::UNAUTHORIZED,
        "sessions from before the reset are revoked"
    );
    let audited: Vec<(String, String)> = sqlx::query_as(
        "SELECT target_id, diff FROM audit_log WHERE action='user.password' ORDER BY at",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(audited.len(), 1, "{audited:?}");
    assert_eq!(audited[0].0, reader.to_string());
    // The audit layer redacts any `password` value; the cause is what is kept.
    assert!(audited[0].1.contains("mailed token"), "{}", audited[0].1);
    assert_eq!(
        confirm(app, token, NEW, NEW).await,
        StatusCode::BAD_REQUEST,
        "a token works once"
    );
}
