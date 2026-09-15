//! Time-based one-time passwords: enrol, sign in with a second step, fall
//! back to a recovery code, and the policy that server owners must enrol
//! before they touch anything privileged.
use super::{call, cookie, csrf, login_as, member, seeded_fixture_configured, text, user};
use axum::http::StatusCode;
use listmngr_core::MemberRole;
use listmngr_db::Database;
use listmngr_db::totp;

fn policy(config: &mut listmngr_core::Config) {
    config.security.require_2fa_for = vec!["server_owner".into()];
}

#[tokio::test]
async fn enrol_sign_in_twice_and_recover() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let (db, app) = seeded_fixture_configured(db, policy).await;
    matrix(db, app).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_totp_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_totp")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 3).await.unwrap();
    db.migrate().await.unwrap();
    let (db, app) = seeded_fixture_configured(db, policy).await;
    matrix(db, app).await;
    schema.drop().await.unwrap();
}

async fn get_status(app: &axum::Router, path: &str, cookie: &str) -> StatusCode {
    call(app, "GET", path, cookie, "").await.status()
}

async fn count(db: &Database, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(db.pool()).await.unwrap()
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// The base32 secret the enrolment page shows.
fn shown_secret(html: &str) -> String {
    html.split("<code id=\"totp-secret\">")
        .nth(1)
        .unwrap_or_else(|| panic!("a secret on the page: {html}"))
        .split('<')
        .next()
        .unwrap()
        .to_owned()
}

/// The recovery codes the confirmation page shows once.
fn shown_codes(html: &str) -> Vec<String> {
    html.split("<li><code>")
        .skip(1)
        .map(|rest| rest.split('<').next().unwrap().to_owned())
        .collect()
}

/// Sign in with the password only; returns the cookie and where it went.
async fn password_step(app: &axum::Router, email: &str) -> (String, StatusCode, String) {
    let response = call(app, "GET", "/web/login", "", "").await;
    let session = cookie(&response);
    let token = csrf(&text(response).await);
    let body = serde_urlencoded::to_string([
        ("csrf", token.as_str()),
        ("email", email),
        ("password", "very secure password"),
    ])
    .unwrap();
    let response = call(app, "POST", "/web/login", &session, &body).await;
    let status = response.status();
    let location = response
        .headers()
        .get("location")
        .map(|v| v.to_str().unwrap().to_owned())
        .unwrap_or_default();
    (cookie(&response), status, location)
}

async fn second_step(app: &axum::Router, cookie: &str, code: &str) -> (StatusCode, String) {
    let page = call(app, "GET", "/web/login/totp", cookie, "").await;
    assert_eq!(page.status(), StatusCode::OK);
    let token = csrf(&text(page).await);
    let body = serde_urlencoded::to_string([("csrf", token.as_str()), ("code", code)]).unwrap();
    let response = call(app, "POST", "/web/login/totp", cookie, &body).await;
    let status = response.status();
    let fresh = response.headers().get("set-cookie").map_or_else(
        || cookie.to_owned(),
        |v| v.to_str().unwrap().split(';').next().unwrap().to_owned(),
    );
    (status, fresh)
}

async fn post_page(
    app: &axum::Router,
    cookie: &str,
    path: &str,
    fields: &[(&str, &str)],
) -> (StatusCode, String) {
    let page = call(app, "GET", "/web/account/totp", cookie, "").await;
    assert_eq!(page.status(), StatusCode::OK);
    let token = csrf(&text(page).await);
    let mut pairs = vec![("csrf", token.as_str())];
    pairs.extend_from_slice(fields);
    let body = serde_urlencoded::to_string(pairs).unwrap();
    let response = call(app, "POST", path, cookie, &body).await;
    let status = response.status();
    (status, text(response).await)
}

async fn post(app: &axum::Router, cookie: &str, path: &str, fields: &[(&str, &str)]) -> StatusCode {
    post_page(app, cookie, path, fields).await.0
}

async fn matrix(db: Database, app: axum::Router) {
    user(&db, "root@example.com", true).await;
    user(&db, "member@example.com", false).await;
    member(&db, "member@example.com", MemberRole::Member).await;
    let root = login_as(&app, "root@example.com").await;
    let plain = login_as(&app, "member@example.com").await;
    policy_gate(&app, &root, &plain).await;
    let (secret, codes) = enrolled(&db, &app, &root).await;
    assert_eq!(get_status(&app, "/web/admin", &root).await, StatusCode::OK);
    let logout = text(call(&app, "GET", "/web/account", &root, "").await).await;
    let body = serde_urlencoded::to_string([("csrf", csrf(&logout).as_str())]).unwrap();
    assert_eq!(
        call(&app, "POST", "/web/logout", &root, &body)
            .await
            .status(),
        StatusCode::SEE_OTHER
    );
    two_step_login(&db, &app, &secret).await;
    let fresh = recovery_login(&db, &app, &codes).await;
    disabled(&db, &app, &fresh).await;
}

/// Policy: a server owner without a second factor is kept out of anything
/// privileged, and told why; an ordinary member is not.
async fn policy_gate(app: &axum::Router, root: &str, plain: &str) {
    for path in ["/web/admin", "/web/moderation"] {
        assert_eq!(
            get_status(app, path, root).await,
            StatusCode::FORBIDDEN,
            "{path}"
        );
    }
    let account = text(call(app, "GET", "/web/account", root, "").await).await;
    assert!(account.contains("/web/account/totp"), "{account}");
    assert!(
        account.contains("requires a second sign-in step"),
        "{account}"
    );
    assert_eq!(get_status(app, "/web/account", plain).await, StatusCode::OK);
}

/// Enrolment: the page shows a secret and a QR code, keeps them on reload,
/// and refuses a wrong code.
async fn enrolment_page(db: &Database, app: &axum::Router, root: &str) -> String {
    let page = text(call(app, "GET", "/web/account/totp", root, "").await).await;
    let secret = shown_secret(&page);
    assert_eq!(secret.len(), 32, "160-bit secret as base32: {secret}");
    assert!(page.contains("otpauth://totp/"), "{page}");
    assert!(page.contains("<svg"), "an inline QR code: {page}");
    let again = text(call(app, "GET", "/web/account/totp", root, "").await).await;
    assert_eq!(
        shown_secret(&again),
        secret,
        "reloading keeps the pending secret"
    );
    assert_eq!(
        post(
            app,
            root,
            "/web/account/totp/confirm",
            &[("code", "000000")]
        )
        .await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM user_totp WHERE confirmed_at IS NOT NULL"
        )
        .await,
        0
    );
    secret
}

/// The right code confirms and shows ten recovery codes once.
async fn enrolled(db: &Database, app: &axum::Router, root: &str) -> (String, Vec<String>) {
    let secret = enrolment_page(db, app, root).await;
    let code = totp::code(&totp::decode(&secret).unwrap(), now_ms() / 1000 / 30);
    let page = call(app, "GET", "/web/account/totp", root, "").await;
    let token = csrf(&text(page).await);
    let body = serde_urlencoded::to_string([("csrf", token.as_str()), ("code", &code)]).unwrap();
    let response = call(app, "POST", "/web/account/totp/confirm", root, &body).await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = text(response).await;
    let codes = shown_codes(&html);
    assert_eq!(codes.len(), 10, "{html}");
    assert!(
        codes
            .iter()
            .all(|code| code.len() == 11 && code.chars().nth(5) == Some('-')),
        "{codes:?}"
    );
    for (sql, expected) in [
        (
            "SELECT COUNT(*) FROM user_totp WHERE confirmed_at IS NOT NULL",
            1,
        ),
        (
            "SELECT COUNT(*) FROM user_recovery_codes WHERE used_at IS NULL",
            10,
        ),
        (
            "SELECT COUNT(*) FROM audit_log WHERE action='user.totp.enable'",
            1,
        ),
    ] {
        assert_eq!(count(db, sql).await, expected, "{sql}");
    }
    let status = text(call(app, "GET", "/web/account/totp", root, "").await).await;
    assert!(!status.contains(&codes[0]), "recovery codes are shown once");
    assert!(status.contains("/web/account/totp/disable"), "{status}");
    (secret, codes)
}

/// The password alone yields a pending session that can do nothing but the
/// second step, and five wrong codes end it.
async fn pending_session(app: &axum::Router) {
    let (pending, status, location) = password_step(app, "root@example.com").await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location, "/web/login/totp");
    for path in ["/web/account", "/web/admin"] {
        assert_eq!(
            get_status(app, path, &pending).await,
            StatusCode::UNAUTHORIZED,
            "{path}"
        );
    }
    for _ in 0..5 {
        let (status, _) = second_step(app, &pending, "000000").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    assert_eq!(
        get_status(app, "/web/login/totp", &pending).await,
        StatusCode::UNAUTHORIZED,
        "five failures end the pending session"
    );
}

/// The right code completes the login into a rotated session and cannot be
/// replayed.
async fn two_step_login(db: &Database, app: &axum::Router, secret: &str) {
    pending_session(app).await;
    let (pending, _, _) = password_step(app, "root@example.com").await;
    // The confirmation consumed the current step, so the next step's code —
    // one step of drift, which is accepted — is what a real app would show
    // moments later.
    let step = now_ms() / 1000 / 30 + 1;
    let code = totp::code(&totp::decode(secret).unwrap(), step);
    let (status, full) = second_step(app, &pending, &code).await;
    assert_eq!(
        status,
        StatusCode::SEE_OTHER,
        "one step of drift is accepted"
    );
    assert_ne!(full, pending, "the session is rotated on completion");
    assert_eq!(get_status(app, "/web/account", &full).await, StatusCode::OK);
    assert_eq!(get_status(app, "/web/admin", &full).await, StatusCode::OK);
    assert_eq!(
        get_status(app, "/web/account", &pending).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM audit_log WHERE action='web.login' AND diff LIKE '%totp%'"
        )
        .await,
        1
    );
    // The same code again, within its step, is a replay.
    let (pending, _, _) = password_step(app, "root@example.com").await;
    let (status, _) = second_step(app, &pending, &code).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "a code is accepted once");
}

/// A recovery code completes the login once; regenerating replaces the rest.
async fn recovery_login(db: &Database, app: &axum::Router, codes: &[String]) -> Vec<String> {
    let (pending, _, _) = password_step(app, "root@example.com").await;
    let (status, full) = second_step(app, &pending, &codes[3]).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(get_status(app, "/web/account", &full).await, StatusCode::OK);
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM user_recovery_codes WHERE used_at IS NULL"
        )
        .await,
        9
    );
    let (pending, _, _) = password_step(app, "root@example.com").await;
    let (status, _) = second_step(app, &pending, &codes[3]).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a recovery code works once"
    );
    let (status, full) = second_step(app, &pending, &codes[4].to_uppercase()).await;
    assert_eq!(
        status,
        StatusCode::SEE_OTHER,
        "case and spacing are forgiven"
    );
    // Regenerating replaces the remaining codes.
    assert_eq!(
        post(
            app,
            &full,
            "/web/account/totp/recovery",
            &[("password", "wrong")]
        )
        .await,
        StatusCode::UNAUTHORIZED
    );
    let (status, html) = post_page(
        app,
        &full,
        "/web/account/totp/recovery",
        &[("password", "very secure password")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let fresh = shown_codes(&html);
    assert_eq!(fresh.len(), 10);
    assert!(
        !fresh.contains(&codes[0]),
        "regeneration replaces every code"
    );
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM user_recovery_codes WHERE used_at IS NULL"
        )
        .await,
        10
    );
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM audit_log WHERE action='user.totp.recovery'"
        )
        .await,
        1
    );
    fresh
}

/// Disabling needs the password; afterwards login is one step again and the
/// policy gate is back.
async fn disabled(db: &Database, app: &axum::Router, codes: &[String]) {
    let (pending, _, _) = password_step(app, "root@example.com").await;
    let (status, full) = second_step(app, &pending, &codes[0]).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        post(
            app,
            &full,
            "/web/account/totp/disable",
            &[("password", "wrong")]
        )
        .await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        post(
            app,
            &full,
            "/web/account/totp/disable",
            &[("password", "very secure password")]
        )
        .await,
        StatusCode::SEE_OTHER
    );
    for (sql, expected) in [
        ("SELECT COUNT(*) FROM user_totp", 0),
        ("SELECT COUNT(*) FROM user_recovery_codes", 0),
        (
            "SELECT COUNT(*) FROM audit_log WHERE action='user.totp.disable'",
            1,
        ),
    ] {
        assert_eq!(count(db, sql).await, expected, "{sql}");
    }
    let (cookie, status, location) = password_step(app, "root@example.com").await;
    assert_eq!(
        (status, location.as_str()),
        (StatusCode::SEE_OTHER, "/web/account")
    );
    assert_eq!(
        get_status(app, "/web/admin", &cookie).await,
        StatusCode::FORBIDDEN
    );
}
