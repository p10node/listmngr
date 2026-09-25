//! `GET /plugins`: what this build's plugins add — none, for a build
//! without any, under both prefixes.
use axum::{
    body::{Body, to_bytes},
    extract::ConnectInfo,
    http::{Request, StatusCode, header},
};
use listmngr_core::Config;
use listmngr_db::{Database, NewUser};
use tower::ServiceExt;

async fn fixture(scopes: &[&str]) -> (axum::Router, String) {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let user = db
        .users()
        .create(NewUser {
            display_name: "Admin".into(),
            email: "admin@example.com".into(),
            password: "very secure password".into(),
            server_owner: true,
        })
        .await
        .unwrap();
    let token = db
        .tokens()
        .create(user.id, "test", scopes, None)
        .await
        .unwrap()
        .token;
    (listmngr_api::router(db, Config::default()), token)
}

async fn get(
    app: &axum::Router,
    path: &str,
    token: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut request = Request::builder().method("GET").uri(path);
    if let Some(token) = token {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let mut request = request.body(Body::empty()).unwrap();
    request.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:4242".parse::<std::net::SocketAddr>().unwrap(),
    ));
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

#[tokio::test]
async fn the_plugins_of_this_build_are_listed_under_both_prefixes() {
    let (app, token) = fixture(&["system:read"]).await;
    for (prefix, key) in [("/api/v1", "items"), ("/3.1", "entries")] {
        let (status, body) = get(&app, &format!("{prefix}/plugins"), Some(&token)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        // This crate links no plugin: the collection is there and empty.
        assert_eq!(body[key], serde_json::json!([]), "{body}");
    }
    assert_eq!(
        get(&app, "/api/v1/plugins", None).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (app, reader) = fixture(&["lists:read"]).await;
    assert_eq!(
        get(&app, "/api/v1/plugins", Some(&reader)).await.0,
        StatusCode::FORBIDDEN
    );
}
