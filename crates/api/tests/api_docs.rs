//! `/api/docs` describes this server's own API from this origin alone.
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use listmngr_core::Config;
use listmngr_db::Database;
use tower::ServiceExt;

async fn app() -> axum::Router {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let mut config = Config::default();
    config.site.base_url = "http://localhost".into();
    listmngr_api::router(db, config)
}

async fn get(app: &axum::Router, path: &str) -> (StatusCode, axum::http::HeaderMap, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(path)
                .header("host", "localhost")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), 8_000_000).await.unwrap();
    (status, headers, String::from_utf8(body.to_vec()).unwrap())
}

#[tokio::test]
async fn the_api_documentation_loads_nothing_from_another_origin() {
    let app = app().await;
    let (status, headers, html) = get(&app, "/api/docs").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "text/html; charset=utf-8");
    for marker in [
        "unpkg", "jsdelivr", "cdn", "http://", "https://", "<script", "swagger",
    ] {
        assert!(
            !html.to_lowercase().contains(marker),
            "the documentation page refers to {marker}"
        );
    }
    assert!(
        html.contains("/openapi.json"),
        "the machine-readable document"
    );
    assert!(html.contains("/web/style.css"), "styles from this origin");
}

#[tokio::test]
async fn the_api_documentation_carries_the_browser_security_headers() {
    let app = app().await;
    let (_, headers, _) = get(&app, "/api/docs").await;
    assert_eq!(
        headers["content-security-policy"],
        "default-src 'none'; style-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'"
    );
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(headers["x-frame-options"], "DENY");
    assert_eq!(headers["referrer-policy"], "strict-origin");
}

#[tokio::test]
async fn the_api_documentation_describes_every_documented_operation() {
    let app = app().await;
    let (_, _, document) = get(&app, "/openapi.json").await;
    let document: serde_json::Value = serde_json::from_str(&document).unwrap();
    let paths = document["paths"].as_object().expect("documented paths");
    assert!(paths.len() > 40, "the documented surface, not a stub");
    let (_, _, html) = get(&app, "/api/docs").await;
    for (path, operations) in paths {
        assert!(html.contains(path), "{path} is missing from /api/docs");
        for method in operations.as_object().expect("operations").keys() {
            assert!(
                html.contains(&method.to_uppercase()),
                "{method} {path} is missing from /api/docs"
            );
        }
    }
    // The bearer scheme and its scopes are what a reader needs before calling.
    assert!(html.contains("bearerAuth") || html.contains("Bearer"));
}
