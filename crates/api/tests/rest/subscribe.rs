//! `POST /members` as Mailman's registrar: the list's policy decides what a
//! subscription still needs, `pre_verified`/`pre_confirmed`/`pre_approved`
//! supply those steps, and `invitation=True` invites instead.
use super::*;

async fn fixture(policy: &str) -> (axum::Router, Database, String) {
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
        .create(user.id, "test", &["admin"], None)
        .await
        .unwrap()
        .token;
    db.domains().create("example.com", "", None).await.unwrap();
    let list = db
        .lists()
        .create(listmngr_db::NewList {
            list_id: "dev.example.com".parse().unwrap(),
            display_name: "Dev".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    db.lists()
        .update(
            &list.id,
            &serde_json::json!({"subscription_policy": policy}),
        )
        .await
        .unwrap();
    (
        listmngr_api::router(db.clone(), config_with_rate(100)),
        db,
        token,
    )
}

async fn members(db: &Database) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM members")
        .fetch_one(db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn a_fully_pre_supplied_subscription_still_creates_the_member() {
    let (app, db, token) = fixture("confirm").await;
    let response = call_form(
        &app,
        "POST",
        "/3.1/members",
        &token,
        "list_id=dev.example.com&subscriber=reader%40example.net&display_name=A%20Reader&pre_verified=true&pre_confirmed=true&pre_approved=true",
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = response_json(response).await;
    assert_eq!(body["email"], "reader@example.net");
    assert_eq!(body["display_name"], "A Reader");
    assert_eq!(members(&db).await, 1);
}

#[tokio::test]
async fn an_unconfirmed_subscription_is_held_for_the_address_and_then_the_moderator() {
    let (app, db, token) = fixture("confirm_then_moderate").await;
    let response = call(
        &app,
        "POST",
        "/3.1/members",
        Some(&token),
        Some(r#"{"list_id":"dev.example.com","subscriber":"reader@example.net"}"#),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = response_json(response).await;
    let held = body["token"].as_str().expect("a token").to_owned();
    assert_eq!(body["token_owner"], "subscriber");
    assert!(body["http_etag"].is_string());
    assert_eq!(members(&db).await, 0);
    let notices: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workflow_notices")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(notices, 1, "the address is asked to confirm");

    // The request is the one the moderator queue would show once confirmed,
    // and a moderator may accept it right away.
    let entry = response_json(
        call(
            &app,
            "GET",
            &format!("/3.1/lists/dev.example.com/requests/{held}"),
            Some(&token),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(entry["email"], "reader@example.net");
    assert_eq!(entry["token_owner"], "subscriber");
    assert_eq!(
        call_form(
            &app,
            "POST",
            &format!("/3.1/lists/dev.example.com/requests/{held}"),
            &token,
            "action=accept",
        )
        .await
        .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(members(&db).await, 1);
}

#[tokio::test]
async fn a_confirmed_but_unapproved_subscription_waits_for_the_moderator() {
    let (app, db, token) = fixture("moderate").await;
    let body = response_json(
        call(
            &app,
            "POST",
            "/api/v1/members",
            Some(&token),
            Some(r#"{"list_id":"dev.example.com","subscriber":"reader@example.net","pre_verified":true,"pre_confirmed":true}"#),
        )
        .await,
    )
    .await;
    assert_eq!(body["token_owner"], "moderator");
    assert_eq!(members(&db).await, 0);
    let notices: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workflow_notices")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(notices, 0, "a moderated request mails nobody yet");
    let count = response_json(
        call(
            &app,
            "GET",
            "/api/v1/lists/dev.example.com/requests/count",
            Some(&token),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(count["count"], 1);
}

#[tokio::test]
async fn an_invitation_asks_the_address_and_needs_no_moderator() {
    let (app, db, token) = fixture("moderate").await;
    let response = call_form(
        &app,
        "POST",
        "/3.1/members",
        &token,
        "list_id=dev.example.com&subscriber=guest%40example.net&invitation=true",
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(response_json(response).await["token_owner"], "subscriber");
    assert_eq!(members(&db).await, 0);
    let raw: Vec<u8> = sqlx::query_scalar(
        "SELECT b.raw FROM workflow_notices n JOIN queue_jobs q ON q.id=n.job_id JOIN messages m ON m.id=q.message_id JOIN message_blobs b ON b.store_key=m.store_key",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(
        String::from_utf8_lossy(&raw).contains("has been invited to join"),
        "the invitation template is used"
    );
    let audit: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='subscription.invite'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(audit, 1);
}

#[tokio::test]
async fn administrative_roles_and_banned_addresses_keep_their_own_rules() {
    let (app, db, token) = fixture("moderate").await;
    // Mailman's role assignment carries no workflow flags and is immediate.
    let response = call_form(
        &app,
        "POST",
        "/3.1/members",
        &token,
        "list_id=dev.example.com&subscriber=owner%40example.net&role=owner",
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(members(&db).await, 1);

    db.bans()
        .create(
            &"dev.example.com".parse().unwrap(),
            "banned@example.net",
            &listmngr_db::AuditContext::system(),
        )
        .await
        .unwrap();
    let response = call(
        &app,
        "POST",
        "/3.1/members",
        Some(&token),
        Some(r#"{"list_id":"dev.example.com","subscriber":"banned@example.net","pre_verified":true,"pre_confirmed":true,"pre_approved":true}"#),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(members(&db).await, 1);
}
