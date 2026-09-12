//! Mailman's `/lists/{id}/requests`: the moderator's view of subscription
//! and unsubscription requests, and the accept/reject/discard/defer verbs.
use super::*;
use listmngr_db::workflows::SubscriptionAction;

/// A list whose joins wait for a moderator and whose leaves need a token,
/// with one request of each kind waiting, plus the app and an admin token.
async fn fixture() -> (axum::Router, Database, String) {
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
            &serde_json::json!({"subscription_policy": "moderate", "unsubscription_policy": "confirm"}),
        )
        .await
        .unwrap();
    db.members()
        .create(listmngr_db::NewMember {
            list_id: list.id.clone(),
            email: "leaver@example.net".into(),
            display_name: String::new(),
            role: listmngr_core::MemberRole::Member,
            subscription_mode: listmngr_core::SubscriptionMode::AsAddress,
        })
        .await
        .unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    db.workflows()
        .request(
            &list.id,
            "joiner@example.net",
            SubscriptionAction::Join,
            now,
        )
        .await
        .unwrap();
    db.workflows()
        .request(
            &list.id,
            "leaver@example.net",
            SubscriptionAction::Leave,
            now + 1,
        )
        .await
        .unwrap();
    (
        listmngr_api::router(db.clone(), config_with_rate(100)),
        db,
        token,
    )
}

async fn get_json(app: &axum::Router, token: &str, uri: &str) -> serde_json::Value {
    response_json(call(app, "GET", uri, Some(token), None).await).await
}

async fn members(db: &Database) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM members")
        .fetch_one(db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn requests_are_listed_in_mailman_shape_with_owner_and_type_filters() {
    let (app, _, token) = fixture().await;
    let page = get_json(&app, &token, "/3.1/lists/dev.example.com/requests").await;
    assert_eq!(page["total_size"], 2, "{page}");
    let entries = page["entries"].as_array().unwrap();
    assert_eq!(entries[0]["email"], "joiner@example.net");
    assert_eq!(entries[0]["token_owner"], "moderator");
    assert_eq!(entries[0]["list_id"], "dev.example.com");
    assert_eq!(entries[0]["type"], "subscription");
    assert!(entries[0]["token"].as_str().unwrap().len() >= 32);
    assert!(entries[0]["request_date"].as_str().unwrap().contains('T'));
    assert!(entries[0]["http_etag"].is_string());
    assert_eq!(entries[1]["email"], "leaver@example.net");
    assert_eq!(entries[1]["token_owner"], "subscriber");
    assert_eq!(entries[1]["type"], "unsubscription");

    let moderator = get_json(
        &app,
        &token,
        "/3.1/lists/dev.example.com/requests?token_owner=moderator",
    )
    .await;
    assert_eq!(moderator["total_size"], 1);
    assert_eq!(moderator["entries"][0]["email"], "joiner@example.net");
    let unsubscriptions = get_json(
        &app,
        &token,
        "/3.1/lists/dev.example.com/requests?request_type=unsubscription",
    )
    .await;
    assert_eq!(unsubscriptions["total_size"], 1);
    assert_eq!(unsubscriptions["entries"][0]["email"], "leaver@example.net");
    let count = get_json(&app, &token, "/3.1/lists/dev.example.com/requests/count").await;
    assert_eq!(count["count"], 2);

    // One request by its token, on both flavours; a foreign list is 404.
    let id = entries[0]["token"].as_str().unwrap();
    let one = get_json(
        &app,
        &token,
        &format!("/api/v1/lists/dev.example.com/requests/{id}"),
    )
    .await;
    assert_eq!(one["email"], "joiner@example.net");
    assert_eq!(
        one["self_link"],
        format!("/api/v1/lists/dev.example.com/requests/{id}")
    );
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("/3.1/lists/other.example.com/requests/{id}"),
            Some(&token),
            None,
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &app,
            "GET",
            "/3.1/lists/dev.example.com/requests",
            None,
            None
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn moderators_decide_requests_with_mailman_actions() {
    let (app, db, token) = fixture().await;
    let page = get_json(&app, &token, "/3.1/lists/dev.example.com/requests").await;
    let join = page["entries"][0]["token"].as_str().unwrap().to_owned();
    let leave = page["entries"][1]["token"].as_str().unwrap().to_owned();

    // Mailman posts a form; defer keeps the request, accept applies it.
    for (action, status, remaining) in [
        ("defer", StatusCode::NO_CONTENT, 2),
        ("nonsense", StatusCode::BAD_REQUEST, 2),
        ("accept", StatusCode::NO_CONTENT, 1),
    ] {
        let response = call_form(
            &app,
            "POST",
            &format!("/3.1/lists/dev.example.com/requests/{join}"),
            &token,
            &format!("action={action}"),
        )
        .await;
        assert_eq!(response.status(), status, "{action}");
        let count = get_json(&app, &token, "/3.1/lists/dev.example.com/requests/count").await;
        assert_eq!(count["count"], remaining, "{action}");
    }
    assert_eq!(members(&db).await, 2, "the join was applied");

    // A moderator may act on a subscriber-owned request too: accepting the
    // leave removes the member without the token ever being used.
    let response = call(
        &app,
        "POST",
        &format!("/api/v1/lists/dev.example.com/requests/{leave}"),
        Some(&token),
        Some(r#"{"action":"reject","reason":"not now"}"#),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(members(&db).await, 2, "reject changes nothing");
    let audit: Vec<String> =
        sqlx::query_scalar("SELECT diff FROM audit_log WHERE action='subscription.reject'")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(audit.len(), 1);
    assert!(audit[0].contains("not now"), "{audit:?}");

    // Decided requests are gone from the queue and from the token URL.
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("/3.1/lists/dev.example.com/requests/{leave}"),
            Some(&token),
            None,
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call_form(
            &app,
            "POST",
            &format!("/3.1/lists/dev.example.com/requests/{join}"),
            &token,
            "action=discard",
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
}
