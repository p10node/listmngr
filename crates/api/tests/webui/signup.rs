//! A person creates an account with a password, proves the address from
//! their mailbox, and only then can sign in. Nothing on the way says whether
//! an address already has an account.
use super::{call, cookie, csrf, fixture, seeded_fixture, text};
use axum::http::StatusCode;
use listmngr_core::Config;
use listmngr_db::Database;

#[tokio::test]
async fn signup_verify_and_login() {
    let (db, app) = fixture().await;
    matrix(db, app).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_signup_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_signup")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 3).await.unwrap();
    db.migrate().await.unwrap();
    let (db, app) = seeded_fixture(db).await;
    matrix(db, app).await;
    schema.drop().await.unwrap();
}

#[tokio::test]
async fn signup_can_be_switched_off() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let mut config = Config::default();
    config.site.base_url = "http://localhost".into();
    config.web.signup = false;
    let app = listmngr_api::router(db, config);
    assert_eq!(
        call(&app, "GET", "/web/signup", "", "").await.status(),
        StatusCode::NOT_FOUND
    );
    let login = text(call(&app, "GET", "/web/login", "", "").await).await;
    assert!(!login.contains("/web/signup"));
    let page = call(&app, "GET", "/web/login", "", "").await;
    let session = cookie(&page);
    let token = csrf(&text(page).await);
    let body = form(
        &token,
        "off@example.com",
        "Off",
        "correct horse battery staple 9",
    );
    assert_eq!(
        call(&app, "POST", "/web/signup", &session, &body)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
}

async fn count(db: &Database, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(db.pool()).await.unwrap()
}

async fn anonymous(app: &axum::Router) -> (String, String) {
    let response = call(app, "GET", "/web/signup", "", "").await;
    assert_eq!(response.status(), StatusCode::OK);
    let session = cookie(&response);
    let html = text(response).await;
    (session, csrf(&html))
}

fn form(token: &str, email: &str, name: &str, password: &str) -> String {
    serde_urlencoded::to_string([
        ("csrf", token),
        ("email", email),
        ("display_name", name),
        ("password", password),
        ("confirm_password", password),
    ])
    .unwrap()
}

/// The verification token from the one queued message.
async fn mailed_token(db: &Database) -> String {
    let raw: Vec<u8> = sqlx::query_scalar(
        "SELECT b.raw FROM message_blobs b JOIN messages m ON m.store_key=b.store_key ORDER BY m.created_at DESC LIMIT 1",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    let mail = String::from_utf8(raw).unwrap();
    assert!(mail.contains("From: postmaster@example.com\r\n"), "{mail}");
    assert!(
        mail.contains("To: Person@Example.net\r\n"),
        "the address as typed"
    );
    assert!(mail.contains("/web/verify"));
    let after = mail.split("enter this token:").nth(1).expect("token line");
    after
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .expect("a token")
        .to_owned()
}

async fn login(app: &axum::Router, email: &str, password: &str) -> StatusCode {
    let response = call(app, "GET", "/web/login", "", "").await;
    let session = cookie(&response);
    let token = csrf(&text(response).await);
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

async fn matrix(db: Database, app: axum::Router) {
    let login_page = text(call(&app, "GET", "/web/login", "", "").await).await;
    assert!(login_page.contains("/web/signup"), "{login_page}");
    let users_before = count(&db, "SELECT COUNT(*) FROM users").await;
    rejected(&db, &app, users_before).await;
    let mailed = accepted(&db, &app, users_before).await;
    verify(&db, &app, &mailed).await;
    assert_eq!(
        login(&app, "person@example.net", PASSWORD).await,
        StatusCode::SEE_OTHER
    );
    repeat_after_verification(&db, &app, users_before).await;
}

const PASSWORD: &str = "correct horse battery staple 9";

/// Nothing below creates anything.
async fn rejected(db: &Database, app: &axum::Router, users_before: i64) {
    let (session, token) = anonymous(app).await;
    for (name, body, status) in [
        (
            "no csrf",
            form("", "person@example.net", "Person", PASSWORD),
            StatusCode::FORBIDDEN,
        ),
        (
            "weak password",
            form(&token, "person@example.net", "Person", "password"),
            StatusCode::BAD_REQUEST,
        ),
        (
            "mismatch",
            serde_urlencoded::to_string([
                ("csrf", token.as_str()),
                ("email", "person@example.net"),
                ("display_name", "Person"),
                ("password", PASSWORD),
                ("confirm_password", "something else entirely 9"),
            ])
            .unwrap(),
            StatusCode::BAD_REQUEST,
        ),
        (
            "not a mailbox",
            form(&token, "person at example", "Person", PASSWORD),
            StatusCode::BAD_REQUEST,
        ),
        (
            "blank name",
            form(&token, "person@example.net", "  ", PASSWORD),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let response = call(app, "POST", "/web/signup", &session, &body).await;
        assert_eq!(response.status(), status, "{name}");
    }
    assert_eq!(count(db, "SELECT COUNT(*) FROM users").await, users_before);
    assert_eq!(count(db, "SELECT COUNT(*) FROM queue_jobs").await, 0);
}

/// A valid signup: an unverified account, one verification mail, one audit
/// row, no way in until the address is proven — and a repeat within the hour
/// that looks identical from outside and mails nothing more.
async fn accepted(db: &Database, app: &axum::Router, users_before: i64) -> String {
    let (session, token) = anonymous(app).await;
    let body = form(&token, "Person@Example.net", "Người <mới>", PASSWORD);
    let response = call(app, "POST", "/web/signup", &session, &body).await;
    let status = response.status();
    let html = text(response).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{html}");
    assert!(html.contains("Check your email"), "{html}");
    for (sql, expected) in [
        ("SELECT COUNT(*) FROM users", users_before + 1),
        (
            "SELECT COUNT(*) FROM addresses WHERE email='person@example.net' AND verified_on IS NULL AND user_id IS NOT NULL",
            1,
        ),
        (
            "SELECT COUNT(*) FROM audit_log WHERE action='user.signup'",
            1,
        ),
        ("SELECT COUNT(*) FROM workflow_notices", 1),
        (
            "SELECT COUNT(*) FROM account_tokens WHERE purpose='verify_address' AND consumed_at IS NULL",
            1,
        ),
    ] {
        assert_eq!(count(db, sql).await, expected, "{sql}");
    }
    assert_eq!(
        login(app, "person@example.net", PASSWORD).await,
        StatusCode::UNAUTHORIZED
    );
    let mailed = mailed_token(db).await;
    assert!(mailed.len() >= 32);
    let (session, token) = anonymous(app).await;
    let again = call(
        app,
        "POST",
        "/web/signup",
        &session,
        &form(&token, "person@example.net", "Again", PASSWORD),
    )
    .await;
    assert_eq!(again.status(), StatusCode::ACCEPTED);
    assert_eq!(
        count(db, "SELECT COUNT(*) FROM users").await,
        users_before + 1
    );
    assert_eq!(count(db, "SELECT COUNT(*) FROM workflow_notices").await, 1);
    mailed
}

/// A verified address that signs up again is neither recreated, mailed nor
/// given the stranger's password.
async fn repeat_after_verification(db: &Database, app: &axum::Router, users_before: i64) {
    let (session, token) = anonymous(app).await;
    let repeat = call(
        app,
        "POST",
        "/web/signup",
        &session,
        &form(
            &token,
            "person@example.net",
            "Third",
            "another strong pass phrase 9",
        ),
    )
    .await;
    assert_eq!(repeat.status(), StatusCode::ACCEPTED);
    assert_eq!(
        count(db, "SELECT COUNT(*) FROM users").await,
        users_before + 1
    );
    assert_eq!(count(db, "SELECT COUNT(*) FROM workflow_notices").await, 1);
    assert_eq!(
        login(app, "person@example.net", PASSWORD).await,
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        login(app, "person@example.net", "another strong pass phrase 9").await,
        StatusCode::UNAUTHORIZED,
        "a stranger's repeat signup did not change the password"
    );
}

/// The token page consumes the token once; the address is verified, audited,
/// and the same token is refused afterwards.
async fn verify(db: &Database, app: &axum::Router, token: &str) {
    let page = call(app, "GET", &format!("/web/verify?token={token}"), "", "").await;
    assert_eq!(page.status(), StatusCode::OK);
    let session = cookie(&page);
    let html = text(page).await;
    assert!(
        html.contains(&format!("value=\"{token}\"")),
        "prefilled token"
    );
    let csrf_token = csrf(&html);
    let body =
        serde_urlencoded::to_string([("csrf", csrf_token.as_str()), ("token", "wrong")]).unwrap();
    assert_eq!(
        call(app, "POST", "/web/verify", &session, &body)
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    let body =
        serde_urlencoded::to_string([("csrf", csrf_token.as_str()), ("token", token)]).unwrap();
    let done = call(app, "POST", "/web/verify", &session, &body).await;
    assert_eq!(done.status(), StatusCode::OK);
    assert!(text(done).await.contains("Address verified"));
    assert_eq!(
        count(db, "SELECT COUNT(*) FROM addresses WHERE email='person@example.net' AND verified_on IS NOT NULL").await,
        1
    );
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM audit_log WHERE action='address.verify'"
        )
        .await,
        1
    );
    assert_eq!(
        call(app, "POST", "/web/verify", &session, &body)
            .await
            .status(),
        StatusCode::BAD_REQUEST,
        "a token works once"
    );
}
