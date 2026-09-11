use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use listmngr_core::Config;
use listmngr_db::{Database, NewList};
use tower::ServiceExt;

#[tokio::test]
async fn public_ban_admission_is_generic_and_rechecks_issued_tokens() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains().create("example.com", "", None).await.unwrap();
    db.lists()
        .create(NewList {
            list_id: "test.example.com".parse().unwrap(),
            display_name: "Test".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let app = listmngr_api::router(db.clone(), Config::default());
    let list = "test.example.com".parse().unwrap();
    let path = "/api/v1/public/lists/test.example.com/subscription";
    let request = serde_json::json!({"email":"Blocked@example.net","action":"join"});
    db.bans()
        .create(
            &list,
            "blocked@example.net",
            &listmngr_db::AuditContext::system(),
        )
        .await
        .unwrap();
    let denied = call(&app, path, request.clone()).await;
    assert_eq!(denied.0, StatusCode::ACCEPTED);
    assert_eq!(
        denied,
        call(
            &app,
            "/api/v1/public/lists/missing.example.com/subscription",
            request.clone()
        )
        .await
    );
    assert_eq!(workflow_count(&db, "subscription_workflows").await, 0);
    assert_eq!(workflow_count(&db, "queue_jobs").await, 0);
    db.bans()
        .delete(
            &list,
            "blocked@example.net",
            &listmngr_db::AuditContext::system(),
        )
        .await
        .unwrap();
    assert_eq!(call(&app, path, request).await, denied);
    let raw: Vec<u8> = sqlx::query_scalar("SELECT raw FROM message_blobs")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let text = String::from_utf8(raw).unwrap();
    let secret = text
        .lines()
        .find_map(|line| line.strip_prefix("Token: "))
        .unwrap();
    let confirm = serde_json::json!({"token":secret});
    let path = "/api/v1/public/lists/test.example.com/confirm";
    db.bans()
        .create(&list, "^Blocked@", &listmngr_db::AuditContext::system())
        .await
        .unwrap();
    assert_eq!(
        call(&app, path, confirm.clone()).await,
        call(&app, path, serde_json::json!({"token":"invalid"})).await
    );
    assert_eq!(workflow_count(&db, "members").await, 0);
    db.bans()
        .delete(&list, "^Blocked@", &listmngr_db::AuditContext::system())
        .await
        .unwrap();
    assert_eq!(call(&app, path, confirm).await.0, StatusCode::OK);
    assert_eq!(workflow_count(&db, "members").await, 1);
}

async fn workflow_count(db: &Database, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
        .fetch_one(db.pool())
        .await
        .unwrap()
}

async fn call(app: &axum::Router, path: &str, body: serde_json::Value) -> (StatusCode, Vec<u8>) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    (
        response.status(),
        to_bytes(response.into_body(), 4096).await.unwrap().to_vec(),
    )
}

#[tokio::test]
async fn public_request_durably_queues_notice_before_scoped_one_time_confirmation() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains().create("example.com", "", None).await.unwrap();
    db.lists()
        .create(NewList {
            list_id: "test.example.com".parse().unwrap(),
            display_name: "Test".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let app = listmngr_api::router(db.clone(), Config::default());
    let path = "/api/v1/public/lists/test.example.com/subscription";
    let request = serde_json::json!({"email":"victim@example.com","action":"join"});
    let (status, body) = call(&app, path, request.clone()).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM members")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        0
    );
    let raw: Vec<u8> = sqlx::query_scalar("SELECT raw FROM message_blobs")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let text = String::from_utf8(raw).unwrap();
    let token = text
        .split("Token: ")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    assert_eq!(token.len(), 43);
    let hash: String = sqlx::query_scalar("SELECT token_hash FROM subscription_workflows")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(hash.len(), 64);
    assert!(!hash.contains(token));
    let confirm = serde_json::json!({"token":token});
    assert_eq!(
        call(
            &app,
            "/api/v1/public/lists/other.example.com/confirm",
            confirm.clone()
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(
            &app,
            "/api/v1/public/lists/test.example.com/confirm",
            confirm.clone()
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM members")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        call(
            &app,
            "/api/v1/public/lists/test.example.com/confirm",
            confirm
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(&app, path, request).await,
        (StatusCode::ACCEPTED, body)
    );
}

#[tokio::test]
async fn public_confirmation_publishes_private_join_and_leave_receipts_once() {
    for (action, expected_members, email) in [
        ("join", 1, "Exact@example.com"),
        ("leave", 0, "DifferentCase@example.net"),
    ] {
        let db = Database::connect("sqlite::memory:", 1).await.unwrap();
        db.migrate().await.unwrap();
        db.domains().create("example.com", "", None).await.unwrap();
        db.lists()
            .create(NewList {
                list_id: "test.example.com".parse().unwrap(),
                display_name: "Test".into(),
                style: "legacy-default".into(),
            })
            .await
            .unwrap();
        let app = listmngr_api::router(db.clone(), Config::default());
        let base = "/api/v1/public/lists/test.example.com";
        assert_eq!(
            call(
                &app,
                &format!("{base}/subscription"),
                serde_json::json!({"email":email,"action":action})
            )
            .await
            .0,
            StatusCode::ACCEPTED
        );
        let challenge: Vec<u8> = sqlx::query_scalar("SELECT raw FROM message_blobs")
            .fetch_one(db.pool())
            .await
            .unwrap();
        let challenge = String::from_utf8(challenge).unwrap();
        let token = challenge
            .lines()
            .find_map(|line| line.strip_prefix("Token: "))
            .unwrap();
        let request = serde_json::json!({"token":token});
        let path = format!("{base}/confirm");
        assert_eq!(call(&app, &path, request.clone()).await.0, StatusCode::OK);
        assert_eq!(
            workflow_count(&db, "workflow_notices").await,
            2,
            "HTTP confirmation must publish challenge plus completion receipt"
        );
        assert_eq!(workflow_count(&db, "members").await, expected_members);
        let raws: Vec<Vec<u8>> = sqlx::query_scalar("SELECT raw FROM message_blobs")
            .fetch_all(db.pool())
            .await
            .unwrap();
        let receipt = raws
            .iter()
            .map(|raw| std::str::from_utf8(raw).unwrap())
            .find(|text| text.contains("request completed\r\n"))
            .unwrap();
        assert!(receipt.contains(&format!("Subject: List {action} request completed\r\n")));
        assert!(receipt.contains(&format!("To: {email}\r\n")));
        assert!(receipt.contains("Auto-Submitted: auto-generated\r\n"));
        assert!(!receipt.contains(token));
        let recipients: Vec<String> = sqlx::query_scalar("SELECT email FROM delivery_recipients")
            .fetch_all(db.pool())
            .await
            .unwrap();
        assert_eq!(recipients, vec![email.to_owned(); 2]);
        assert_eq!(call(&app, &path, request).await.0, StatusCode::BAD_REQUEST);
        assert_eq!(workflow_count(&db, "workflow_notices").await, 2);
        for table in ["owner_deliveries", "digest_posts", "archive_messages"] {
            assert_eq!(workflow_count(&db, table).await, 0);
        }
    }
}

#[tokio::test]
async fn public_requests_and_confirmations_share_a_bounded_pre_auth_budget() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let mut config = Config::default();
    config.security.rate_limit.api_pre_auth = Some("2/min".into());
    let app = listmngr_api::router(db, config);
    let request = serde_json::json!({"email":"unknown@example.com","action":"leave"});
    let path = "/api/v1/public/lists/missing.example.com/subscription";
    assert_eq!(
        call(&app, path, request.clone()).await.0,
        StatusCode::ACCEPTED
    );
    assert_eq!(call(&app, path, request).await.0, StatusCode::ACCEPTED);
    assert_eq!(
        call(
            &app,
            "/api/v1/public/lists/missing.example.com/confirm",
            serde_json::json!({"token":"wrong"})
        )
        .await
        .0,
        StatusCode::TOO_MANY_REQUESTS
    );
}
