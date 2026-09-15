//! Signing in through an `OpenID` Connect provider: a just-in-time account for
//! a verified email, an existing account found by its verified address, a
//! link kept under the account, and the refusals in between.
#[path = "mock_oidc.rs"]
mod mock_oidc;
use super::{call, cookie, csrf, login_as, seeded_fixture_configured, text, user};
use axum::http::StatusCode;
use listmngr_core::{Config, OidcProviderConfig};
use listmngr_db::Database;
use mock_oidc::{MockIdentity, MockProvider};

#[tokio::test]
async fn sign_in_link_and_unlink_through_a_provider() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let provider = MockProvider::start().await;
    let (db, app) = seeded_fixture_configured(db, |config| configure(config, &provider)).await;
    matrix(db, app, provider).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_oidc_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_oidc")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 3).await.unwrap();
    db.migrate().await.unwrap();
    let provider = MockProvider::start().await;
    let (db, app) = seeded_fixture_configured(db, |config| configure(config, &provider)).await;
    matrix(db, app, provider).await;
    schema.drop().await.unwrap();
}

fn configure(config: &mut Config, provider: &MockProvider) {
    config.web.oidc = vec![OidcProviderConfig {
        name: "mock".into(),
        display_name: "Mock IdP".into(),
        issuer: provider.issuer.clone(),
        client_id: provider.client_id.clone(),
        client_secret: Some(provider.client_secret.as_str().into()),
        client_secret_file: None,
        scopes: vec!["openid".into(), "email".into(), "profile".into()],
    }];
}

async fn count(db: &Database, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(db.pool()).await.unwrap()
}

fn location(response: &axum::response::Response) -> String {
    response
        .headers()
        .get("location")
        .map(|v| v.to_str().unwrap().to_owned())
        .unwrap_or_default()
}

/// Walk the flow the browser would: our redirect to the provider, the
/// provider's redirect back, our callback. Returns the callback response.
async fn round_trip(
    app: &axum::Router,
    start_path: &str,
    cookie: &str,
) -> axum::response::Response {
    let to_provider = call(app, "GET", start_path, cookie, "").await;
    assert_eq!(to_provider.status(), StatusCode::SEE_OTHER, "{start_path}");
    let session = to_provider.headers().get("set-cookie").map_or_else(
        || cookie.to_owned(),
        |v| v.to_str().unwrap().split(';').next().unwrap().to_owned(),
    );
    let authorize = location(&to_provider);
    assert!(
        authorize.contains("code_challenge_method=S256"),
        "{authorize}"
    );
    assert!(authorize.contains("nonce="), "{authorize}");
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let back = client.get(&authorize).send().await.unwrap();
    assert_eq!(
        back.status(),
        reqwest::StatusCode::SEE_OTHER,
        "the mock redirects back"
    );
    let callback = back.headers()["location"].to_str().unwrap().to_owned();
    let path = callback
        .strip_prefix("http://localhost")
        .unwrap_or_else(|| panic!("callback on our origin: {callback}"))
        .to_owned();
    call(app, "GET", &path, &session, "").await
}

async fn matrix(db: Database, app: axum::Router, provider: MockProvider) {
    let login = text(call(&app, "GET", "/web/login", "", "").await).await;
    assert!(login.contains("/web/login/oidc/mock"), "{login}");
    assert!(login.contains("Mock IdP"), "{login}");
    assert_eq!(
        call(&app, "GET", "/web/login/oidc/nope", "", "")
            .await
            .status(),
        StatusCode::NOT_FOUND
    );

    just_in_time(&db, &app, &provider).await;
    refusals(&db, &app, &provider).await;
    existing_account(&db, &app, &provider).await;
    link_and_unlink(&db, &app, &provider).await;
    second_step_still_asked(&db, &app, &provider).await;
}

/// The provider replaces the password step only: an account with two-step
/// sign-in enabled lands on the code page with a pending session.
async fn second_step_still_asked(db: &Database, app: &axum::Router, provider: &MockProvider) {
    let reader: String =
        sqlx::query_scalar("SELECT user_id FROM addresses WHERE email='reader@example.com'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    sqlx::query("INSERT INTO user_totp(user_id,secret,created_at,confirmed_at) VALUES($1,'JBSWY3DPEHPK3PXP',1,1)")
        .bind(reader)
        .execute(db.pool())
        .await
        .unwrap();
    provider.assert_identity(MockIdentity {
        subject: "subject-3".into(),
        email: "reader@example.com".into(),
        email_verified: true,
        name: "Reader".into(),
    });
    let response = round_trip(app, "/web/login/oidc/mock", "").await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), "/web/login/totp");
    let pending = cookie(&response);
    assert_eq!(
        call(app, "GET", "/web/account", &pending, "")
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        count(db, "SELECT COUNT(*) FROM audit_log WHERE action='web.login.password' AND diff LIKE '%oidc%'").await,
        1
    );
}

/// A verified email nobody has an account for gets one, verified, with no
/// usable password, signed in at once; the same subject comes back to it.
async fn just_in_time(db: &Database, app: &axum::Router, provider: &MockProvider) {
    let users_before = count(db, "SELECT COUNT(*) FROM users").await;
    let response = round_trip(app, "/web/login/oidc/mock", "").await;
    assert_eq!(
        response.status(),
        StatusCode::SEE_OTHER,
        "{}",
        text(response).await
    );
    let signed_in = cookie(&response);
    assert_eq!(location(&response), "/web/account");
    let account = text(call(app, "GET", "/web/account", &signed_in, "").await).await;
    assert!(account.contains("Người Mới"), "{account}");
    for (sql, expected) in [
        ("SELECT COUNT(*) FROM users", users_before + 1),
        (
            "SELECT COUNT(*) FROM addresses WHERE email='person@example.net' AND verified_on IS NOT NULL AND user_id IS NOT NULL",
            1,
        ),
        (
            "SELECT COUNT(*) FROM user_oidc WHERE provider='mock' AND subject='subject-1'",
            1,
        ),
        ("SELECT COUNT(*) FROM audit_log WHERE action='user.jit'", 1),
        (
            "SELECT COUNT(*) FROM audit_log WHERE action='web.login' AND diff LIKE '%oidc%'",
            1,
        ),
        (
            "SELECT COUNT(*) FROM user_credentials c JOIN addresses a ON a.user_id=c.user_id WHERE a.email='person@example.net' AND c.usable=0",
            1,
        ),
    ] {
        assert_eq!(count(db, sql).await, expected, "{sql}");
    }
    // Back again: the link, not a second account.
    let again = round_trip(app, "/web/login/oidc/mock", "").await;
    assert_eq!(again.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        count(db, "SELECT COUNT(*) FROM users").await,
        users_before + 1
    );
    assert_eq!(provider.issued_tokens(), 2);
}

/// An unverified email creates nothing; a callback whose state is not this
/// session's, or replayed, is refused.
async fn refusals(db: &Database, app: &axum::Router, provider: &MockProvider) {
    let users_before = count(db, "SELECT COUNT(*) FROM users").await;
    provider.assert_identity(MockIdentity {
        subject: "subject-2".into(),
        email: "unverified@example.net".into(),
        email_verified: false,
        name: "Unverified".into(),
    });
    let response = round_trip(app, "/web/login/oidc/mock", "").await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(count(db, "SELECT COUNT(*) FROM users").await, users_before);
    assert_eq!(count(db, "SELECT COUNT(*) FROM user_oidc").await, 1);
    // A callback that answers no ceremony of this session.
    let page = call(app, "GET", "/web/login", "", "").await;
    let fresh = cookie(&page);
    let stray = call(
        app,
        "GET",
        "/web/login/oidc/mock/callback?code=x&state=y",
        &fresh,
        "",
    )
    .await;
    assert_eq!(stray.status(), StatusCode::BAD_REQUEST);
    // A callback with the right session but a mismatched state.
    let to_provider = call(app, "GET", "/web/login/oidc/mock", "", "").await;
    let session = cookie(&to_provider);
    let wrong = call(
        app,
        "GET",
        "/web/login/oidc/mock/callback?code=x&state=not-it",
        &session,
        "",
    )
    .await;
    assert_eq!(wrong.status(), StatusCode::BAD_REQUEST);
    assert_eq!(count(db, "SELECT COUNT(*) FROM users").await, users_before);
}

/// A verified email that already has an account with a verified address
/// links to it rather than creating another.
async fn existing_account(db: &Database, app: &axum::Router, provider: &MockProvider) {
    let reader = user(db, "reader@example.com", false).await;
    provider.assert_identity(MockIdentity {
        subject: "subject-3".into(),
        email: "Reader@Example.com".into(),
        email_verified: true,
        name: "Reader".into(),
    });
    let users_before = count(db, "SELECT COUNT(*) FROM users").await;
    let response = round_trip(app, "/web/login/oidc/mock", "").await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let signed_in = cookie(&response);
    let account = text(call(app, "GET", "/web/account", &signed_in, "").await).await;
    assert!(account.contains("reader@example.com"), "{account}");
    assert_eq!(count(db, "SELECT COUNT(*) FROM users").await, users_before);
    let linked: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM user_oidc WHERE provider='mock' AND subject='subject-3' AND user_id=$1",
    )
    .bind(reader.id.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(linked, 1);
}

/// Linking from the account page needs a signed-in session; unlinking needs
/// the password when the account has one, and never removes the last way in.
async fn link_and_unlink(db: &Database, app: &axum::Router, provider: &MockProvider) {
    user(db, "owner@example.com", false).await;
    let owner = login_as(app, "owner@example.com").await;
    let page = text(call(app, "GET", "/web/account/oidc", &owner, "").await).await;
    assert!(page.contains("/web/account/oidc/mock/link"), "{page}");
    assert!(!page.contains("/web/account/oidc/mock/unlink"), "{page}");
    provider.assert_identity(MockIdentity {
        subject: "subject-4".into(),
        email: "owner-elsewhere@example.org".into(),
        email_verified: true,
        name: "Owner".into(),
    });
    // Linking starts from a POST (CSRF) and comes back to the account.
    let token = csrf(&page);
    let body = serde_urlencoded::to_string([("csrf", token.as_str())]).unwrap();
    let started = call(app, "POST", "/web/account/oidc/mock/link", &owner, &body).await;
    assert_eq!(started.status(), StatusCode::SEE_OTHER);
    let authorize = location(&started);
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let back = client.get(&authorize).send().await.unwrap();
    let callback = back.headers()["location"].to_str().unwrap().to_owned();
    let path = callback
        .strip_prefix("http://localhost")
        .unwrap()
        .to_owned();
    let done = call(app, "GET", &path, &owner, "").await;
    assert_eq!(done.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&done), "/web/account/oidc");
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM audit_log WHERE action='user.oidc.link'"
        )
        .await,
        1
    );
    let page = text(call(app, "GET", "/web/account/oidc", &owner, "").await).await;
    assert!(page.contains("/web/account/oidc/mock/unlink"), "{page}");
    assert!(page.contains("owner-elsewhere@example.org"), "{page}");
    unlink_needs_password(db, app, &page, &owner).await;
    last_way_in(db, app, provider).await;
}

/// The linked subject signs the owner in; unlinking needs the password.
async fn unlink_needs_password(db: &Database, app: &axum::Router, page: &str, owner: &str) {
    // The linked subject now signs the owner in.
    let response = round_trip(app, "/web/login/oidc/mock", "").await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let account = text(call(app, "GET", "/web/account", &cookie(&response), "").await).await;
    assert!(account.contains("owner@example.com"), "{account}");
    // Unlinking needs the password.
    let token = csrf(page);
    let wrong =
        serde_urlencoded::to_string([("csrf", token.as_str()), ("password", "wrong")]).unwrap();
    assert_eq!(
        call(app, "POST", "/web/account/oidc/mock/unlink", owner, &wrong)
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
        call(app, "POST", "/web/account/oidc/mock/unlink", owner, &right)
            .await
            .status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM user_oidc WHERE subject='subject-4'"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM audit_log WHERE action='user.oidc.unlink'"
        )
        .await,
        1
    );
}

/// The just-in-time account has no usable password and only this link: it
/// cannot be unlinked.
async fn last_way_in(db: &Database, app: &axum::Router, provider: &MockProvider) {
    provider.assert_identity(MockIdentity {
        subject: "subject-1".into(),
        email: "person@example.net".into(),
        email_verified: true,
        name: "Người Mới".into(),
    });
    let response = round_trip(app, "/web/login/oidc/mock", "").await;
    let jit = cookie(&response);
    let page = text(call(app, "GET", "/web/account/oidc", &jit, "").await).await;
    assert!(page.contains("last way"), "the page says why: {page}");
    assert!(
        !page.contains("/web/account/oidc/mock/unlink"),
        "no form is offered: {page}"
    );
    // The refusal holds without the form: the token comes from another page.
    let token = csrf(&text(call(app, "GET", "/web/account", &jit, "").await).await);
    let body = serde_urlencoded::to_string([("csrf", token.as_str()), ("password", "")]).unwrap();
    assert_eq!(
        call(app, "POST", "/web/account/oidc/mock/unlink", &jit, &body)
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM user_oidc WHERE subject='subject-1'"
        )
        .await,
        1
    );
}
