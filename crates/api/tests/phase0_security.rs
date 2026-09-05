use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use base64::Engine;
use listmngr_core::Config;
use listmngr_db::{Database, NewUser};
use std::io::{Read, Write};
use tower::ServiceExt;

async fn app_and_token(mut config: Config) -> (axum::Router, Database, String) {
    config.security.rate_limit.api = "1000/min".into();
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let user = db
        .users()
        .create(NewUser {
            display_name: "Security contract".into(),
            email: "security@example.com".into(),
            password: "Orbit!Cobalt7-River$Quartz".into(),
            server_owner: true,
        })
        .await
        .unwrap();
    let token = db
        .tokens()
        .create(user.id, "security", &["admin"], None)
        .await
        .unwrap()
        .token;
    (listmngr_api::router(db.clone(), config), db, token)
}

async fn oneshot(app: &axum::Router, path: &str) -> axum::response::Response {
    let mut request = Request::builder().uri(path).body(Body::empty()).unwrap();
    request.extensions_mut().insert(axum::extract::ConnectInfo(
        "127.0.0.1:4242".parse::<std::net::SocketAddr>().unwrap(),
    ));
    app.clone().oneshot(request).await.unwrap()
}

#[tokio::test]
async fn readiness_fails_closed_when_the_database_pool_is_broken() {
    let (app, db, _) = app_and_token(Config::default()).await;
    assert_eq!(oneshot(&app, "/readyz").await.status(), StatusCode::OK);
    db.pool().close().await;
    let response = oneshot(&app, "/readyz").await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["status"],
        "not-ready"
    );
}

#[tokio::test]
async fn internal_api_errors_are_correlated_and_hide_database_details() {
    let (app, db, token) = app_and_token(Config::default()).await;
    db.pool().close().await;
    let mut request = Request::builder()
        .uri("/api/v1/system/versions")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    request.extensions_mut().insert(axum::extract::ConnectInfo(
        "127.0.0.1:4242".parse::<std::net::SocketAddr>().unwrap(),
    ));
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["code"], "internal");
    assert!(uuid::Uuid::parse_str(body["correlation_id"].as_str().unwrap()).is_ok());
    let rendered = body.to_string().to_ascii_lowercase();
    for forbidden in [
        "sql",
        "pool",
        "closed",
        "database",
        "postgres://",
        "sqlite:",
    ] {
        assert!(
            !rendered.contains(forbidden),
            "API error leaked {forbidden}: {rendered}"
        );
    }
}

#[tokio::test]
async fn metrics_are_valid_prometheus_text_not_a_status_placeholder() {
    let (app, _, _) = app_and_token(Config::default()).await;
    let response = oneshot(&app, "/metrics").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/plain; version=0.0.4"
    );
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = std::str::from_utf8(&body).unwrap();
    let scrape = prometheus_parse::Scrape::parse(text.lines().map(|line| Ok(line.to_owned())))
        .expect("metrics endpoint must parse as Prometheus exposition text");
    assert!(
        scrape
            .samples
            .iter()
            .any(|sample| sample.metric == "listmngr_up")
    );
}

#[tokio::test]
async fn every_response_has_a_unique_parseable_request_correlation_header() {
    let (app, _, _) = app_and_token(Config::default()).await;
    let first = oneshot(&app, "/healthz").await;
    let second = oneshot(&app, "/does-not-exist").await;
    let first_id = first.headers()["x-request-id"].to_str().unwrap();
    let second_id = second.headers()["x-request-id"].to_str().unwrap();
    assert!(uuid::Uuid::parse_str(first_id).is_ok());
    assert!(uuid::Uuid::parse_str(second_id).is_ok());
    assert_ne!(first_id, second_id);
}

async fn serve_once(config: Config) -> (std::net::SocketAddr, String, tokio::task::JoinHandle<()>) {
    let (app, _, token) = app_and_token(config).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    (address, token, server)
}

async fn raw_get(address: std::net::SocketAddr, authorization: &str, forwarded: &str) -> String {
    let authorization = authorization.to_owned();
    let forwarded = forwarded.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        let request = format!(
            "GET /3.1/system/versions HTTP/1.1\r\nHost: localhost\r\n{}: {authorization}\r\n{}: {forwarded}\r\nConnection: close\r\n\r\n",
            header::AUTHORIZATION,
            header::HeaderName::from_static("x-forwarded-for"),
        );
        stream
            .write_all(request.as_bytes())
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    })
    .await
    .unwrap()
}

fn basic(token: &str) -> String {
    let (_, rest) = token.split_once("lm_").unwrap();
    let (id, secret) = rest.split_once('_').unwrap();
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{id}:{secret}"))
    )
}

#[tokio::test]
async fn basic_allowlist_uses_real_socket_peer_and_never_x_forwarded_for() {
    let mut denied = Config::default();
    denied.api.compat_basic_auth = true;
    denied.api.compat_basic_auth_allow = vec!["192.0.2.0/24".parse().unwrap()];
    let (address, token, server) = serve_once(denied).await;
    let response = raw_get(address, &basic(&token), "192.0.2.1").await;
    server.abort();
    assert!(response.starts_with("HTTP/1.1 401"), "{response}");

    let mut allowed = Config::default();
    allowed.api.compat_basic_auth = true;
    allowed.api.compat_basic_auth_allow = vec!["127.0.0.1/32".parse().unwrap()];
    let (address, token, server) = serve_once(allowed).await;
    let response = raw_get(address, &basic(&token), "203.0.113.9").await;
    server.abort();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
}
