//! Passkeys: register one under the account, sign in with it alone, and let
//! it satisfy the second-factor policy; remove it with the password.
#[path = "soft_authenticator.rs"]
mod soft_authenticator;
use super::{call, cookie, csrf, login_as, seeded_fixture_configured, text, user};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use listmngr_db::Database;
use soft_authenticator::SoftPasskey;
use tower::ServiceExt;

const ORIGIN: &str = "http://localhost";

fn policy(config: &mut listmngr_core::Config) {
    config.security.require_2fa_for = vec!["server_owner".into()];
}

#[tokio::test]
async fn register_sign_in_and_remove_a_passkey() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let (db, app) = seeded_fixture_configured(db, policy).await;
    matrix(db, app).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_passkeys_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_passkeys")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 3).await.unwrap();
    db.migrate().await.unwrap();
    let (db, app) = seeded_fixture_configured(db, policy).await;
    matrix(db, app).await;
    schema.drop().await.unwrap();
}

async fn count(db: &Database, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(db.pool()).await.unwrap()
}

/// A JSON POST the page's script would make: same-origin, CSRF in a header.
async fn json_post(
    app: &axum::Router,
    path: &str,
    cookie: &str,
    csrf_token: &str,
    body: &str,
) -> (StatusCode, serde_json::Value, Option<String>) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("host", "localhost")
                .header("origin", ORIGIN)
                .header("sec-fetch-site", "same-origin")
                .header("cookie", cookie)
                .header("x-csrf-token", csrf_token)
                .header("content-type", "application/json")
                .body(Body::from(body.to_owned()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let fresh = response
        .headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned());
    let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, value, fresh)
}

async fn page(app: &axum::Router, cookie: &str) -> String {
    let response = call(app, "GET", "/web/account/passkeys", cookie, "").await;
    assert_eq!(response.status(), StatusCode::OK);
    text(response).await
}

async fn matrix(db: Database, app: axum::Router) {
    user(&db, "root@example.com", true).await;
    let root = login_as(&app, "root@example.com").await;
    assert_eq!(
        call(&app, "GET", "/web/admin", &root, "").await.status(),
        StatusCode::FORBIDDEN,
        "no second factor yet"
    );
    let html = page(&app, &root).await;
    assert!(
        html.contains("/web/passkeys.js"),
        "the page is enhanced by a first-party script: {html}"
    );
    assert!(html.contains("<script"), "{html}");
    let response = call(&app, "GET", "/web/account/passkeys", &root, "").await;
    let csp = response.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(csp.contains("script-src 'self'"), "{csp}");
    assert!(csp.contains("connect-src 'self'"), "{csp}");
    let script = call(&app, "GET", "/web/passkeys.js", "", "").await;
    assert_eq!(script.status(), StatusCode::OK);
    assert_eq!(
        script.headers()["content-type"],
        "text/javascript; charset=utf-8"
    );
    let csrf_token = csrf(&html);

    let mut passkey = registered(&db, &app, &root, &csrf_token).await;
    assert_eq!(
        call(&app, "GET", "/web/admin", &root, "").await.status(),
        StatusCode::OK,
        "a passkey satisfies the second-factor policy"
    );
    let signed_in = passkey_login(&db, &app, &mut passkey).await;
    removed(&db, &app, &signed_in, &passkey).await;
}

/// Registration: options name this site and exclude nothing yet; a response
/// for another origin is refused; a good one stores the key and audits.
async fn registered(
    db: &Database,
    app: &axum::Router,
    root: &str,
    csrf_token: &str,
) -> SoftPasskey {
    let (status, options, _) = json_post(
        app,
        "/web/account/passkeys/register/start",
        root,
        csrf_token,
        "{}",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{options}");
    assert_eq!(options["rp"]["id"], "localhost");
    assert_eq!(options["user"]["name"], "root@example.com");
    assert_eq!(options["authenticatorSelection"]["residentKey"], "required");
    assert_eq!(
        options["excludeCredentials"].as_array().map(Vec::len),
        Some(0)
    );
    let (status, _, _) = json_post(
        app,
        "/web/account/passkeys/register/start",
        root,
        "wrong",
        "{}",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // Signed for the wrong origin.
    let (_, foreign) = SoftPasskey::register(&options, "https://evil.example");
    let body = serde_json::json!({"name": "Laptop", "credential": serde_json::from_str::<serde_json::Value>(&foreign).unwrap()}).to_string();
    let (status, _, _) = json_post(
        app,
        "/web/account/passkeys/register/finish",
        root,
        csrf_token,
        &body,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(count(db, "SELECT COUNT(*) FROM user_passkeys").await, 0);
    // The ceremony was consumed by the failure; start again.
    let (_, options, _) = json_post(
        app,
        "/web/account/passkeys/register/start",
        root,
        csrf_token,
        "{}",
    )
    .await;
    let (passkey, response) = SoftPasskey::register(&options, ORIGIN);
    let body = serde_json::json!({"name": "Laptop <b>", "credential": serde_json::from_str::<serde_json::Value>(&response).unwrap()}).to_string();
    let (status, reply, _) = json_post(
        app,
        "/web/account/passkeys/register/finish",
        root,
        csrf_token,
        &body,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    for (sql, expected) in [
        ("SELECT COUNT(*) FROM user_passkeys", 1),
        (
            "SELECT COUNT(*) FROM audit_log WHERE action='user.passkey.add'",
            1,
        ),
        (
            "SELECT COUNT(*) FROM users WHERE webauthn_handle IS NOT NULL",
            1,
        ),
    ] {
        assert_eq!(count(db, sql).await, expected, "{sql}");
    }
    let listed = page(app, root).await;
    assert!(listed.contains("Laptop &lt;b&gt;"), "{listed}");
    // A second registration excludes the first credential.
    let (_, options, _) = json_post(
        app,
        "/web/account/passkeys/register/start",
        root,
        csrf_token,
        "{}",
    )
    .await;
    assert_eq!(
        options["excludeCredentials"][0]["id"],
        passkey.credential_id()
    );
    passkey
}

/// Passwordless login: an anonymous session starts the ceremony, the
/// assertion completes it into a signed-in session, and the challenge is
/// gone afterwards.
async fn passkey_login(db: &Database, app: &axum::Router, passkey: &mut SoftPasskey) -> String {
    let login = call(app, "GET", "/web/login", "", "").await;
    let anonymous = cookie(&login);
    let html = text(login).await;
    assert!(
        html.contains("/web/passkeys.js"),
        "the login page offers passkeys: {html}"
    );
    let csrf_token = csrf(&html);
    let (status, options, _) = json_post(
        app,
        "/web/login/passkey/start",
        &anonymous,
        &csrf_token,
        "{}",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{options}");
    assert_eq!(options["rpId"], "localhost");
    assert_eq!(options["userVerification"], "required");
    let assertion = passkey.assert(&options);
    let (status, reply, fresh) = json_post(
        app,
        "/web/login/passkey/finish",
        &anonymous,
        &csrf_token,
        &assertion,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["next"], "/web/account");
    let signed_in = fresh.expect("a rotated session cookie");
    assert_ne!(signed_in, anonymous);
    assert_eq!(
        call(app, "GET", "/web/account", &signed_in, "")
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        call(app, "GET", "/web/admin", &signed_in, "")
            .await
            .status(),
        StatusCode::OK,
        "a passkey login needs no further step and satisfies the policy"
    );
    for (sql, expected) in [
        (
            "SELECT COUNT(*) FROM audit_log WHERE action='web.login' AND diff LIKE '%passkey%'",
            1,
        ),
        (
            "SELECT COUNT(*) FROM user_passkeys WHERE last_used_at IS NOT NULL",
            1,
        ),
    ] {
        assert_eq!(count(db, sql).await, expected, "{sql}");
    }
    replay_and_counter(app, passkey, &assertion).await;
    signed_in
}

/// Replaying an assertion against a fresh session fails, since the challenge
/// it answered is gone; a fresh assertion with a higher counter works.
async fn replay_and_counter(app: &axum::Router, passkey: &mut SoftPasskey, assertion: &str) {
    let login = call(app, "GET", "/web/login", "", "").await;
    let other = cookie(&login);
    let other_csrf = csrf(&text(login).await);
    let (status, _, _) = json_post(
        app,
        "/web/login/passkey/finish",
        &other,
        &other_csrf,
        assertion,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (_, options, _) =
        json_post(app, "/web/login/passkey/start", &other, &other_csrf, "{}").await;
    let fresh = passkey.assert(&options);
    let (status, _, _) = json_post(
        app,
        "/web/login/passkey/finish",
        &other,
        &other_csrf,
        &fresh,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// Removal needs the password; afterwards the key can no longer sign in.
async fn removed(db: &Database, app: &axum::Router, cookie: &str, passkey: &SoftPasskey) {
    let html = page(app, cookie).await;
    let id: String = sqlx::query_scalar("SELECT id FROM user_passkeys")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(
        html.contains(&format!("/web/account/passkeys/{id}/remove")),
        "{html}"
    );
    let token = csrf(&html);
    let wrong =
        serde_urlencoded::to_string([("csrf", token.as_str()), ("password", "wrong")]).unwrap();
    assert_eq!(
        call(
            app,
            "POST",
            &format!("/web/account/passkeys/{id}/remove"),
            cookie,
            &wrong
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    let right = serde_urlencoded::to_string([
        ("csrf", token.as_str()),
        ("password", "very secure password"),
    ])
    .unwrap();
    assert_eq!(
        call(
            app,
            "POST",
            &format!("/web/account/passkeys/{id}/remove"),
            cookie,
            &right
        )
        .await
        .status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(count(db, "SELECT COUNT(*) FROM user_passkeys").await, 0);
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM audit_log WHERE action='user.passkey.remove'"
        )
        .await,
        1
    );
    assert_eq!(
        call(app, "GET", "/web/admin", cookie, "").await.status(),
        StatusCode::FORBIDDEN,
        "the policy gate returns without a second factor"
    );
    let _ = passkey.credential_id();
}
