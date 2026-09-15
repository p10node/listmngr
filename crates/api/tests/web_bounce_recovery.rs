use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use listmngr_core::{Config, MemberRole, SubscriptionMode};
use listmngr_db::{Database, NewList, NewMember, NewUser};
use tower::ServiceExt;

async fn fixture() -> (
    Database,
    listmngr_core::Member,
    listmngr_db::web_sessions::WebSession,
) {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains()
        .create("recover.invalid", "", None)
        .await
        .unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: "list.recover.invalid".parse().unwrap(),
            display_name: "Recovery".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    db.users()
        .create(NewUser {
            email: "own@recover.invalid".into(),
            display_name: "Own".into(),
            password: "a very secure fixture password".into(),
            server_owner: false,
        })
        .await
        .unwrap();
    db.addresses()
        .verify("own@recover.invalid", true)
        .await
        .unwrap();
    let m = db
        .members()
        .create(NewMember {
            list_id: list.id,
            email: "own@recover.invalid".into(),
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsUser,
            display_name: "Own".into(),
        })
        .await
        .unwrap();
    sqlx::query("UPDATE preferences SET delivery_status='by_bounces' WHERE id=$1")
        .bind(m.preferences_id.0.to_string())
        .execute(db.pool())
        .await
        .unwrap();
    let anon = db
        .create_web_session(None, None, chrono::Utc::now().timestamp_millis())
        .await
        .unwrap();
    let listmngr_db::web_sessions::LoginOutcome::Complete(session) = db
        .browser_login(
            "own@recover.invalid",
            "a very secure fixture password",
            &anon,
        )
        .await
        .unwrap()
    else {
        panic!("no second factor is enrolled")
    };
    (db, m, session)
}

#[tokio::test]
async fn recovery_confirmation_then_post() {
    let (db, m, session) = fixture().await;
    let mut config = Config::default();
    config.site.base_url = "http://localhost".into();
    let app = listmngr_api::router(db.clone(), config);
    let url = format!("/web/members/{}/recover", m.id);
    let cookie = format!("listmngr_session={}", session.token);
    let response = call(&app, "GET", &url, &cookie, "", "http://localhost").await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = String::from_utf8(
        to_bytes(response.into_body(), 100_000)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(html.contains("Verify that your mailbox is working"));
    let audits: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='bounce.recover'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(audits, 0);
    let account = call(&app, "GET", "/web/account", &cookie, "", "http://localhost").await;
    let html = String::from_utf8(
        to_bytes(account.into_body(), 100_000)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(html.contains(&url));
    let body = format!("csrf={}", session.csrf);
    for (csrf, origin) in [
        ("csrf=wrong", "http://localhost"),
        (body.as_str(), "null"),
        (body.as_str(), "https://foreign.invalid"),
        (body.as_str(), ""),
    ] {
        assert_eq!(
            call(&app, "POST", &url, &cookie, csrf, origin)
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        call(&app, "POST", &url, &cookie, &body, "http://localhost")
            .await
            .status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        call(&app, "POST", &url, &cookie, &body, "http://localhost")
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    let status: String = sqlx::query_scalar("SELECT delivery_status FROM preferences WHERE id=$1")
        .bind(m.preferences_id.0.to_string())
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(status, "enabled");
}
async fn call(
    app: &axum::Router,
    method: &str,
    url: &str,
    cookie: &str,
    body: &str,
    origin: &str,
) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(url)
                .header("cookie", cookie)
                .header("origin", origin)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(body.to_owned()))
                .unwrap(),
        )
        .await
        .unwrap()
}
