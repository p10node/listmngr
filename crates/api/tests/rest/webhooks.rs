use super::*;
use listmngr_db::Database;

const KEY: &str = "0123456789abcdef0123456789abcdef";

async fn setup_webhooks() -> (axum::Router, String, Database, UserId) {
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
    let mut config = config_with_rate(100);
    config.webhooks.signing_key = Some(KEY.into());
    let app = listmngr_api::router(db.clone(), config);
    (app, token, db, user.id)
}

fn entries(page: &serde_json::Value) -> Vec<serde_json::Value> {
    page.get("entries")
        .or_else(|| page.get("items"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default()
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One ordered contract per prefix.
async fn webhooks_are_made_shown_once_changed_pinged_rotated_and_deleted() {
    let (app, token, _, _) = setup_webhooks().await;
    for prefix in ["/api/v1", "/3.1"] {
        let collection = format!("{prefix}/webhooks");
        let body = serde_json::json!({
            "url": format!("https://hooks.example.invalid{prefix}"),
            "events": ["list.*", "member.create"],
            "description": "ops"
        })
        .to_string();
        let created = call(&app, "POST", &collection, Some(&token), Some(&body)).await;
        assert_eq!(created.status(), StatusCode::CREATED);
        let location = created.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .to_owned();
        let created = response_json(created).await;
        let id = created["id"].as_str().unwrap().to_owned();
        assert_eq!(location, format!("{collection}/{id}"));
        assert_eq!(created["self_link"], location);
        let secret = created["secret"].as_str().unwrap().to_owned();
        assert_eq!(secret.len(), 64);
        assert!(secret.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(created["secret_fingerprint"].as_str().unwrap().len(), 8);
        assert_eq!(created["enabled"], true);
        assert_eq!(created["list_id"], serde_json::Value::Null);
        assert_eq!(
            created["events"],
            serde_json::json!(["list.*", "member.create"])
        );
        assert_eq!(created["description"], "ops");
        // Read back: never the secret again.
        let read = response_json(call(&app, "GET", &location, Some(&token), None).await).await;
        assert_eq!(read["id"], id);
        assert!(read.get("secret").is_none(), "{read}");
        assert_eq!(read["secret_fingerprint"], created["secret_fingerprint"]);
        let page = response_json(call(&app, "GET", &collection, Some(&token), None).await).await;
        assert!(
            entries(&page).iter().any(|entry| entry["id"] == id),
            "{page}"
        );
        // Change what a patch names.
        let patched = call(
            &app,
            "PATCH",
            &location,
            Some(&token),
            Some(r#"{"enabled": false, "description": "paused"}"#),
        )
        .await;
        assert_eq!(patched.status(), StatusCode::OK);
        let patched = response_json(patched).await;
        assert_eq!(patched["enabled"], false);
        assert_eq!(patched["description"], "paused");
        assert_eq!(patched["url"], created["url"]);
        // Rotate: a new secret, shown once.
        let rotated = call(
            &app,
            "POST",
            &format!("{location}/rotate"),
            Some(&token),
            None,
        )
        .await;
        assert_eq!(rotated.status(), StatusCode::OK);
        let rotated = response_json(rotated).await;
        assert_ne!(rotated["secret"], secret);
        assert_ne!(rotated["secret_fingerprint"], created["secret_fingerprint"]);
        // Ping: a delivery queued for the runner.
        let ping = call(
            &app,
            "POST",
            &format!("{location}/ping"),
            Some(&token),
            None,
        )
        .await;
        assert_eq!(ping.status(), StatusCode::CREATED);
        let ping = response_json(ping).await;
        assert_eq!(ping["event"], "ping");
        assert_eq!(ping["state"], "pending");
        assert_eq!(ping["attempts"], 0);
        assert_eq!(ping["webhook_id"], id);
        let deliveries = response_json(
            call(
                &app,
                "GET",
                &format!("{location}/deliveries"),
                Some(&token),
                None,
            )
            .await,
        )
        .await;
        let deliveries = entries(&deliveries);
        assert_eq!(deliveries.len(), 1, "{deliveries:?}");
        assert_eq!(deliveries[0]["id"], ping["id"]);
        assert_eq!(deliveries[0]["payload"]["event"], "ping");
        // Gone, with its deliveries.
        assert_eq!(
            call(&app, "DELETE", &location, Some(&token), None)
                .await
                .status(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            call(&app, "GET", &location, Some(&token), None)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            call(
                &app,
                "GET",
                &format!("{location}/deliveries"),
                Some(&token),
                None
            )
            .await
            .status(),
            StatusCode::NOT_FOUND
        );
    }
}

#[tokio::test]
async fn webhooks_need_their_scope_and_a_bound_token_sees_its_list_only() {
    let (app, admin, db, user) = setup_webhooks().await;
    create_configurable_list(&app, &admin).await;
    let site = response_json(
        call(
            &app,
            "POST",
            "/api/v1/webhooks",
            Some(&admin),
            Some(r#"{"url":"https://hooks.example.invalid/site","events":["*"]}"#),
        )
        .await,
    )
    .await;
    let site_id = site["id"].as_str().unwrap().to_owned();
    // Without the scope, or a token: refused.
    let reader = db
        .tokens()
        .create(user, "reader", &["lists:read"], None)
        .await
        .unwrap()
        .token;
    assert_eq!(
        call(&app, "GET", "/api/v1/webhooks", Some(&reader), None)
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "GET", "/api/v1/webhooks", None, None)
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    // A token bound to a list: its list's webhooks, and no other.
    let list: listmngr_core::ListId = "dev.example.com".parse().unwrap();
    let bound = db
        .tokens()
        .create_scoped(user, "bound", &["webhooks"], Some(&list), None, None)
        .await
        .unwrap()
        .token;
    let own = call(
        &app,
        "POST",
        "/api/v1/webhooks",
        Some(&bound),
        Some(r#"{"url":"https://hooks.example.invalid/dev","events":["member.*"]}"#),
    )
    .await;
    assert_eq!(own.status(), StatusCode::CREATED);
    let own = response_json(own).await;
    assert_eq!(
        own["list_id"], "dev.example.com",
        "bound to the token's list"
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/webhooks",
            Some(&bound),
            Some(r#"{"url":"https://hooks.example.invalid/x","events":["*"],"list_id":"other.example.com"}"#),
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    let seen = response_json(call(&app, "GET", "/api/v1/webhooks", Some(&bound), None).await).await;
    let seen: Vec<_> = entries(&seen)
        .iter()
        .map(|entry| entry["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(seen, [own["id"].as_str().unwrap().to_owned()]);
    for (method, path) in [
        ("GET", format!("/api/v1/webhooks/{site_id}")),
        ("DELETE", format!("/api/v1/webhooks/{site_id}")),
        ("POST", format!("/api/v1/webhooks/{site_id}/ping")),
    ] {
        assert_eq!(
            call(&app, method, &path, Some(&bound), None).await.status(),
            StatusCode::NOT_FOUND,
            "{method} {path}: another list's webhook is not there"
        );
    }
    let all = response_json(call(&app, "GET", "/api/v1/webhooks", Some(&admin), None).await).await;
    assert_eq!(entries(&all).len(), 2);
    // A token bound to a domain has no webhooks to see.
    let domain = db.domains().get("example.com").await.unwrap().id;
    let domain_bound = db
        .tokens()
        .create_scoped(user, "domain", &["webhooks"], None, Some(domain), None)
        .await
        .unwrap()
        .token;
    assert_eq!(
        call(&app, "GET", "/api/v1/webhooks", Some(&domain_bound), None)
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn a_webhook_is_refused_what_it_cannot_take() {
    let (app, token, _, _) = setup_webhooks().await;
    for (body, status) in [
        (
            r#"{"url":"http://hooks.example.invalid/x","events":["*"]}"#,
            StatusCode::BAD_REQUEST,
        ),
        (
            r#"{"url":"https://hooks.example.invalid/x","events":[]}"#,
            StatusCode::BAD_REQUEST,
        ),
        (
            r#"{"url":"https://hooks.example.invalid/x","events":["Member.*"]}"#,
            StatusCode::BAD_REQUEST,
        ),
        (
            r#"{"url":"https://hooks.example.invalid/x","events":["*"],"list_id":"nobody.example.com"}"#,
            StatusCode::NOT_FOUND,
        ),
    ] {
        assert_eq!(
            call(&app, "POST", "/api/v1/webhooks", Some(&token), Some(body))
                .await
                .status(),
            status,
            "{body}"
        );
    }
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/webhooks/not-a-webhook-id",
            Some(&token),
            None
        )
        .await
        .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/webhooks/00000000-0000-0000-0000-000000000001",
            Some(&token),
            None
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
}
