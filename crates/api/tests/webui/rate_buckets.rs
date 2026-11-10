//! Browser rate buckets are keyed by principal: one client exhausting its
//! budget must not lock other clients or signed-in users out.
use super::{cookie, csrf, strict_login_fixture, text, user};
use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode},
    response::Response,
};
use tower::ServiceExt;

async fn from(
    app: &axum::Router,
    peer: &str,
    method: &str,
    path: &str,
    cookie: &str,
    body: &str,
) -> Response {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "localhost")
        .header("origin", "http://localhost")
        .header("cookie", cookie)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(body.to_owned()))
        .unwrap();
    request.extensions_mut().insert(ConnectInfo(
        format!("{peer}:4242")
            .parse::<std::net::SocketAddr>()
            .unwrap(),
    ));
    app.clone().oneshot(request).await.unwrap()
}

async fn login(app: &axum::Router, peer: &str, email: &str, password: &str) -> Response {
    let page = from(app, peer, "GET", "/web/login", "", "").await;
    let session = cookie(&page);
    let token = csrf(&text(page).await);
    let form = serde_urlencoded::to_string([
        ("csrf", token.as_str()),
        ("email", email),
        ("password", password),
    ])
    .unwrap();
    from(app, peer, "POST", "/web/login", &session, &form).await
}

#[tokio::test]
async fn one_client_exhausting_the_password_budget_does_not_lock_out_others() {
    let (db, app) = strict_login_fixture().await;
    user(&db, "victim@example.com", false).await;
    user(&db, "other@example.com", false).await;
    let victim = login(
        &app,
        "198.51.100.7",
        "victim@example.com",
        "very secure password",
    )
    .await;
    assert_eq!(victim.status(), StatusCode::SEE_OTHER);
    let victim = cookie(&victim);
    // An anonymous client burns its own budget on an account that does not exist.
    for _ in 0..5 {
        let r = login(&app, "203.0.113.9", "nobody@example.com", "wrong").await;
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    }
    let r = login(&app, "203.0.113.9", "nobody@example.com", "wrong").await;
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    // Another client with valid credentials still signs in.
    let r = login(
        &app,
        "198.51.100.8",
        "other@example.com",
        "very secure password",
    )
    .await;
    assert_eq!(r.status(), StatusCode::SEE_OTHER);
    // A signed-in user still changes their password.
    let url = "/web/account/password";
    let token = csrf(&text(from(&app, "198.51.100.7", "GET", url, &victim, "").await).await);
    let form = serde_urlencoded::to_string([
        ("csrf", token.as_str()),
        ("current_password", "very secure password"),
        ("new_password", "new strong password phrase 2026!"),
        ("confirm_password", "new strong password phrase 2026!"),
    ])
    .unwrap();
    let r = from(&app, "198.51.100.7", "POST", url, &victim, &form).await;
    assert_eq!(r.status(), StatusCode::OK);
}

#[tokio::test]
async fn guessing_one_account_from_many_addresses_is_still_limited() {
    let (db, app) = strict_login_fixture().await;
    user(&db, "victim@example.com", false).await;
    for i in 0..5 {
        let r = login(
            &app,
            &format!("203.0.113.{i}"),
            "victim@example.com",
            "wrong",
        )
        .await;
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    }
    let r = login(&app, "203.0.113.200", "victim@example.com", "wrong").await;
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn pre_auth_buckets_are_per_peer() {
    let db = listmngr_db::Database::connect("sqlite::memory:", 1)
        .await
        .unwrap();
    db.migrate().await.unwrap();
    let (_, app) = super::seeded_fixture_configured(db, |config| {
        config.security.rate_limit.api_pre_auth = Some("3/min".into());
    })
    .await;
    for _ in 0..3 {
        let r = from(&app, "203.0.113.9", "GET", "/web/login", "", "").await;
        assert_eq!(r.status(), StatusCode::OK);
    }
    let r = from(&app, "203.0.113.9", "GET", "/web/login", "", "").await;
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    let r = from(&app, "198.51.100.7", "GET", "/web/login", "", "").await;
    assert_eq!(r.status(), StatusCode::OK);
}
