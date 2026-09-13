//! Mailman's `/lists/{id}/digest` on both prefixes: the counters, `bump`,
//! `send` and `periodic`.
use super::*;
use listmngr_db::digests::DigestRecipient;
use listmngr_db::mail_queue::{NewMessage, Queue};

/// An admin app that also hands back its database.
async fn setup_with_db(scopes: &[&str]) -> (axum::Router, String, listmngr_db::Database) {
    let db = listmngr_db::Database::connect("sqlite::memory:", 1)
        .await
        .unwrap();
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
    let app = listmngr_api::router(db.clone(), config_with_rate(100));
    (app, token, db)
}

/// Put one post into the list's digest collection the way the digest
/// runner does.
async fn collect_one(db: &listmngr_db::Database, list: &listmngr_core::ListId, now: i64) {
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: b"Subject: post\r\n\r\nbody".to_vec(),
                external_id: format!("<{}@example.com>", uuid::Uuid::now_v7()),
                context: "{}".into(),
                queue: Queue::Digest,
                max_attempts: 3,
            },
            now,
        )
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::Digest, "test", now, 1000)
        .await
        .unwrap()
        .unwrap();
    db.digests()
        .collect(
            &lease,
            list,
            b"From: author@example.com\r\nSubject: safe\r\nMessage-ID: <safe@example.com>\r\n\r\nbody",
            &[DigestRecipient {
                email: "digest@example.com".into(),
                mode: "mime_digests".into(),
            }],
            now,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn digest_counters_read_bump_and_send_on_both_prefixes() {
    let (app, token, db) = setup_with_db(&["admin"]).await;
    create_configurable_list(&app, &token).await;
    let list: listmngr_core::ListId = "dev.example.com".parse().unwrap();
    for (prefix, etag) in [("/3.1", true), ("/api/v1", false)] {
        let uri = format!("{prefix}/lists/dev.example.com/digest");
        let counters = response_json(call(&app, "GET", &uri, Some(&token), None).await).await;
        assert_eq!(counters["volume"], 1, "{prefix}");
        assert_eq!(counters["next_digest_number"], 1, "{prefix}");
        assert_eq!(counters["self_link"], uri);
        assert_eq!(counters.get("http_etag").is_some(), etag);
    }
    let uri = "/3.1/lists/dev.example.com/digest";
    // mailmanclient's `bump_digest()` and `send_digest()` post forms.
    let bumped = call_form(&app, "POST", uri, &token, "bump=True").await;
    assert_eq!(bumped.status(), StatusCode::ACCEPTED);
    assert_eq!(
        response_json(bumped).await,
        serde_json::json!({"published": 0, "bumped": true})
    );
    let counters = response_json(call(&app, "GET", uri, Some(&token), None).await).await;
    assert_eq!(counters["volume"], 2);
    assert_eq!(counters["next_digest_number"], 1);
    // Nothing collected: a send publishes nothing and stays accepted.
    let sent = call_form(&app, "POST", uri, &token, "send=True").await;
    assert_eq!(sent.status(), StatusCode::ACCEPTED);
    assert_eq!(response_json(sent).await["published"], 0);

    collect_one(&db, &list, chrono::Utc::now().timestamp_millis()).await;
    // Under the size threshold and younger than a day, periodic is not due.
    let periodic = call_form(&app, "POST", uri, &token, "periodic=True").await;
    assert_eq!(periodic.status(), StatusCode::ACCEPTED);
    assert_eq!(response_json(periodic).await["published"], 0);
    // A forced send publishes the issue and advances the number.
    let body = serde_json::json!({"send": true}).to_string();
    let sent = call(&app, "POST", uri, Some(&token), Some(&body)).await;
    assert_eq!(sent.status(), StatusCode::ACCEPTED);
    assert_eq!(response_json(sent).await["published"], 1);
    let counters = response_json(call(&app, "GET", uri, Some(&token), None).await).await;
    assert_eq!(counters["volume"], 2);
    assert_eq!(counters["next_digest_number"], 2);
    let issue = db
        .mail_queue()
        .claim(
            Queue::Out,
            "out",
            chrono::Utc::now().timestamp_millis() + 1_000,
            100,
        )
        .await
        .unwrap()
        .expect("the issue is queued for delivery");
    let message = db.mail_queue().message(issue.job.message_id).await.unwrap();
    let raw = String::from_utf8_lossy(&message.raw);
    assert!(raw.contains("Subject: Dev Digest, Vol 2, Issue 1"), "{raw}");
    assert_eq!(
        db.mail_queue()
            .pending_recipients(issue.job.id)
            .await
            .unwrap(),
        ["digest@example.com"]
    );

    for bad in ["send=maybe", "flush=True"] {
        assert_eq!(
            call_form(&app, "POST", uri, &token, bad).await.status(),
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/lists/missing.example.com/digest",
            Some(&token),
            None
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn digest_routes_enforce_token_scope_and_list_boundaries() {
    let (app, token, _) = scoped_app().await;
    for prefix in ["/api/v1", "/3.1"] {
        for (method, body) in [("GET", None), ("POST", Some(r#"{"bump":true}"#))] {
            let uri = format!("{prefix}/lists/two.second.example/digest");
            assert_eq!(
                call(&app, method, &uri, Some(&token), body).await.status(),
                StatusCode::FORBIDDEN,
                "{method} {uri}"
            );
        }
        let uri = format!("{prefix}/lists/one.first.example/digest");
        assert_eq!(
            call(&app, "GET", &uri, Some(&token), None).await.status(),
            StatusCode::OK
        );
    }
    let (app, token, _) = setup(&["lists:read"]).await;
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/lists/dev.example.com/digest",
            Some(&token),
            Some(r#"{"send":true}"#)
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/lists/dev.example.com/digest",
            None,
            None
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
}
