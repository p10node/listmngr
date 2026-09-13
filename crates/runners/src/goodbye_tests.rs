use super::*;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

async fn api_write(app: &axum::Router, token: &str, method: &str, path: &str, body: &str) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_owned()))
        .unwrap();
    request.extensions_mut().insert(axum::extract::ConnectInfo(
        "127.0.0.1:4242".parse::<std::net::SocketAddr>().unwrap(),
    ));
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(
        response.status(),
        if method == "POST" {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        }
    );
}

#[tokio::test]
async fn api_enable_delete_reopen_and_deliver_only_stored_subscriber() {
    verify_goodbye_delivery(false).await;
    verify_goodbye_delivery(true).await;
}

async fn verify_goodbye_delivery(delete_list: bool) {
    let dir = tempfile::tempdir_in(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target")).unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("goodbye.sqlite").display()
    );
    let db = Database::connect(&url, 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains().create("example.com", "", None).await.unwrap();
    db.lists()
        .create(NewList {
            list_id: "test.example.com".parse().unwrap(),
            // The list display name is public (Mailman prints it in notices).
            display_name: "Test List".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let admin = db
        .users()
        .create(listmngr_db::NewUser {
            display_name: "Admin".into(),
            email: "requester@example.com".into(),
            password: "Orbit!Cobalt7-River$Quartz".into(),
            server_owner: true,
        })
        .await
        .unwrap();
    // Store authoritative transport spelling before a differently-cased API request.
    db.members()
        .create(listmngr_db::NewMember {
            list_id: "test.example.com".parse().unwrap(),
            email: "Exact@example.com".into(),
            role: listmngr_core::MemberRole::Nonmember,
            subscription_mode: listmngr_core::SubscriptionMode::AsAddress,
            display_name: "PRIVATE NAME".into(),
        })
        .await
        .unwrap();
    let token = db
        .tokens()
        .create(admin.id, "fixture", &["admin"], None)
        .await
        .unwrap()
        .token;
    let app = listmngr_api::router(db.clone(), listmngr_core::Config::default());
    api_write(&app, &token, "PATCH", "/api/v1/lists/test.example.com/config", r#"{"send_goodbye_message":true,"anonymous_list":true,"subject_prefix":"PRIVATE PREFIX","dmarc_mitigate_action":"munge_from","dmarc_mitigate_unconditionally":true}"#).await;
    api_write(&app, &token, "POST", "/api/v1/members", r#"{"list_id":"test.example.com","subscriber":"exact@example.com","pre_verified":true,"pre_confirmed":true,"pre_approved":true}"#).await;
    let id: String = sqlx::query_scalar("SELECT id FROM members WHERE role='member'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let mut request = Request::builder()
        .method("DELETE")
        .uri(format!("/api/v1/members/{id}"))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    request.extensions_mut().insert(axum::extract::ConnectInfo(
        "127.0.0.1:4242".parse::<std::net::SocketAddr>().unwrap(),
    ));
    assert_eq!(
        app.clone().oneshot(request).await.unwrap().status(),
        StatusCode::NO_CONTENT
    );
    drop(app);
    db.pool().close().await;
    let db = Database::connect(&url, 1).await.unwrap();
    let recipients: Vec<String> = sqlx::query_scalar("SELECT email FROM delivery_recipients")
        .fetch_all(db.pool())
        .await
        .unwrap();
    assert_eq!(recipients, ["Exact@example.com"]);
    let sink = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut role = MailRoleConfig::from_core(&tests::plaintext_config()).unwrap();
    role.smtp_relay = sink.local_addr().unwrap();
    role.command_timeout = Duration::from_secs(2);
    if delete_list {
        db.lists()
            .delete(&"test.example.com".parse().unwrap())
            .await
            .unwrap();
    }
    assert_goodbye_delivery(&db, &role, &sink).await;
}

async fn assert_goodbye_delivery(
    db: &Database,
    role: &MailRoleConfig,
    sink: &tokio::net::TcpListener,
) {
    let stored: Vec<u8> = sqlx::query_scalar("SELECT b.raw FROM workflow_notices n JOIN queue_jobs q ON q.id=n.job_id JOIN messages m ON m.id=q.message_id JOIN message_blobs b ON b.store_key=m.store_key").fetch_one(db.pool()).await.unwrap();
    let mail = deliver_queued_notice(db, role, sink).await;
    assert_eq!(
        mail.as_bytes(),
        stored,
        "SMTP DATA must be exact stored private notice"
    );
    assert!(mail.contains("Subject: You have been unsubscribed from the "));
    assert!(mail.contains("(test@example.com)"));
    assert!(mail.contains("To: Exact@example.com\r\n"));
    assert!(mail.contains("Auto-Submitted: auto-generated\r\n"));
    assert!(mail.len() <= 4096);
    for forbidden in [
        "requester@example.com",
        "PRIVATE",
        "List-Post:",
        "X-BeenThere:",
    ] {
        assert!(!mail.contains(forbidden), "leaked or cooked {forbidden}");
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM workflow_notices")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        1
    );
    assert!(
        db.mail_queue()
            .claim(
                Queue::Out,
                "again",
                chrono::Utc::now().timestamp_millis(),
                60_000
            )
            .await
            .unwrap()
            .is_none()
    );
    db.pool().close().await;
}
