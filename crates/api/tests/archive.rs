use axum::{
    body::{Body, to_bytes},
    extract::ConnectInfo,
    http::{Request, StatusCode},
};
use listmngr_core::{Config, MemberRole, SubscriptionMode};
use listmngr_db::mail_queue::{NewMessage, Queue};
use listmngr_db::{Database, NewList, NewMember, NewUser};
use tower::ServiceExt;
async fn request(app: &axum::Router, path: &str, token: Option<&str>) -> (StatusCode, String) {
    let mut builder = Request::builder().uri(path);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let mut req = builder.body(Body::empty()).unwrap();
    req.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:4242".parse::<std::net::SocketAddr>().unwrap(),
    ));
    let response = app.clone().oneshot(req).await.unwrap();
    let status = response.status();
    (
        status,
        String::from_utf8(
            to_bytes(response.into_body(), 10_000_000)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap(),
    )
}
async fn fixture() -> (Database, axum::Router) {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    for name in ["dev", "other"] {
        db.lists()
            .create(NewList {
                list_id: format!("{name}.example.invalid").parse().unwrap(),
                display_name: name.into(),
                style: "legacy-default".into(),
            })
            .await
            .unwrap();
    }
    let raw=b"Message-ID: <api@example.invalid>\r\nSubject: <script>alert(1)</script>\r\nContent-Type: text/plain\r\n\r\nsecret body <img src=x onerror=alert(1)>\r\nFrom body\r\n";
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: raw.to_vec(),
                external_id: "<api@example.invalid>".into(),
                context: r#"{"list_id":"dev.example.invalid"}"#.into(),
                queue: Queue::Archive,
                max_attempts: 3,
            },
            100,
        )
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::Archive, "archive", 100, 1000)
        .await
        .unwrap()
        .unwrap();
    listmngr_archive::process(&db, &lease, 101).await.unwrap();
    let mut config = Config::default();
    config.security.rate_limit.api = "1000/min".into();
    let app = listmngr_api::router(db.clone(), config);
    (db, app)
}
#[tokio::test]
async fn public_archive_http_search_thread_export_and_escaped_ssr() {
    let (_, app) = fixture().await;
    let (status, body) = request(&app, "/api/v1/archives/dev.example.invalid/messages", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("secret body"));
    let data: serde_json::Value = serde_json::from_str(&body).unwrap();
    let hash = data[0]["thread"].as_str().unwrap();
    assert_eq!(
        request(
            &app,
            &format!("/api/v1/archives/dev.example.invalid/threads/{hash}"),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(
        !request(
            &app,
            "/api/v1/archives/other.example.invalid/messages",
            None
        )
        .await
        .1
        .contains("secret body")
    );
    assert_eq!(
        request(
            &app,
            "/api/v1/archives/dev.example.invalid/messages?q=absent",
            None
        )
        .await
        .1,
        "[]"
    );
    assert_eq!(
        request(
            &app,
            "/api/v1/archives/dev.example.invalid/messages?count=101",
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let (status, mbox) = request(
        &app,
        "/api/v1/archives/dev.example.invalid/export.mbox",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(mbox.contains("\n>From body\r\n"));
    let (status, html) = request(&app, "/archives/dev.example.invalid", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("&lt;script&gt;"));
    assert!(!html.contains("<script>"));
    assert!(!html.contains("<img src=x"));
}
#[tokio::test]
#[allow(clippy::too_many_lines)] // One ordered ownership/policy transition contract.
async fn private_requires_verified_owned_membership_and_resource_bounds() {
    let (db, app) = fixture().await;
    sqlx::query(
        "UPDATE mailing_lists SET archive_policy='private' WHERE list_id='dev.example.invalid'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let user = db
        .users()
        .create(NewUser {
            display_name: "reader".into(),
            email: "reader@example.invalid".into(),
            password: "very secure password".into(),
            server_owner: false,
        })
        .await
        .unwrap();
    let token = db
        .tokens()
        .create(user.id, "reader", &["members:read"], None)
        .await
        .unwrap()
        .token;
    let admin = db
        .tokens()
        .create(user.id, "admin", &["admin"], None)
        .await
        .unwrap()
        .token;
    let list = "dev.example.invalid".parse().unwrap();
    let paths = [
        "/api/v1/archives/dev.example.invalid/messages",
        "/api/v1/archives/dev.example.invalid/messages?q=secret",
        "/api/v1/archives/dev.example.invalid/export.mbox",
        "/archives/dev.example.invalid",
    ];
    for path in paths {
        assert_eq!(request(&app, path, None).await.0, StatusCode::FORBIDDEN);
        assert_eq!(
            request(&app, path, Some(&token)).await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(request(&app, path, Some(&admin)).await.0, StatusCode::OK);
    }
    db.members()
        .create(NewMember {
            list_id: list,
            email: "reader@example.invalid".into(),
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsAddress,
            display_name: String::new(),
        })
        .await
        .unwrap();
    assert_eq!(
        request(&app, paths[0], Some(&token)).await.0,
        StatusCode::FORBIDDEN
    );
    db.addresses()
        .verify("reader@example.invalid", true)
        .await
        .unwrap();
    assert_eq!(
        request(&app, paths[0], Some(&token)).await.0,
        StatusCode::OK
    );
    let other = "other.example.invalid".parse().unwrap();
    let bound = db
        .tokens()
        .create_scoped(
            user.id,
            "bound",
            &["members:read"],
            Some(&other),
            None,
            None,
        )
        .await
        .unwrap()
        .token;
    assert_eq!(
        request(&app, paths[0], Some(&bound)).await.0,
        StatusCode::FORBIDDEN
    );
    db.addresses()
        .link("reader@example.invalid", None)
        .await
        .unwrap();
    assert_eq!(
        request(&app, paths[0], Some(&token)).await.0,
        StatusCode::FORBIDDEN
    );
    sqlx::query(
        "UPDATE mailing_lists SET archive_policy='never' WHERE list_id='dev.example.invalid'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    for path in paths {
        assert_eq!(
            request(&app, path, Some(&admin)).await.0,
            StatusCode::NOT_FOUND
        );
    }
}
