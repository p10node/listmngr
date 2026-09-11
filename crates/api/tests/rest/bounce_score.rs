use super::*;
use serde_json::json;

#[tokio::test]
async fn member_score_reads_persisted_value_and_rejects_writes() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains().create("example.com", "", None).await.unwrap();
    db.lists()
        .create(listmngr_db::NewList {
            list_id: "dev.example.com".parse().unwrap(),
            display_name: "Dev".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let member = db
        .members()
        .create(listmngr_db::NewMember {
            list_id: "dev.example.com".parse().unwrap(),
            email: "signal@example.com".into(),
            display_name: String::new(),
            role: listmngr_core::MemberRole::Member,
            subscription_mode: listmngr_core::SubscriptionMode::AsAddress,
        })
        .await
        .unwrap();
    sqlx::query(
        "UPDATE members SET bounce_score=3.5,last_bounce_received='2026-09-01T12:00:00+00:00'",
    )
    .execute(db.pool())
    .await
    .unwrap();
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
    let app = listmngr_api::router(db, config_with_rate(100));
    for prefix in ["/api/v1", "/3.1"] {
        let path = format!("{prefix}/members/{}", member.id);
        let response = call(&app, "GET", &path, Some(&token), None).await;
        assert_eq!(response.status(), StatusCode::OK);
        let value = response_json(response).await;
        assert_eq!(value["bounce_score"], 3.5);
        assert_eq!(value["last_bounce_received"], "2026-09-01T12:00:00Z");
        for body in [r#"{"bounce_score":0}"#, r#"{"last_bounce_received":null}"#] {
            assert_eq!(
                call(&app, "PATCH", &path, Some(&token), Some(body))
                    .await
                    .status(),
                StatusCode::BAD_REQUEST
            );
        }
    }
}

async fn config_fixture() -> (axum::Router, String) {
    let (app, token, _) = setup(&["admin"]).await;
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/domains",
            Some(&token),
            Some(r#"{"mail_host":"example.com"}"#)
        )
        .await
        .status(),
        StatusCode::CREATED
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/lists",
            Some(&token),
            Some(r#"{"fqdn_listname":"dev@example.com"}"#)
        )
        .await
        .status(),
        StatusCode::CREATED
    );
    (app, token)
}

#[tokio::test]
async fn bounce_config_json_form_patch_put_and_openapi() {
    let (app, token) = config_fixture().await;
    for prefix in ["/api/v1", "/3.1"] {
        let path = format!("{prefix}/lists/dev.example.com/config");
        let response = call_form(&app, "PATCH", &path, &token, "process_bounces=True").await;
        assert_eq!(response.status(), StatusCode::OK, "form enablement");
        assert_eq!(response_json(response).await["process_bounces"], true);
        assert_eq!(
            response_json(call(&app, "PATCH", &path, Some(&token), Some("{}")).await).await["process_bounces"],
            true
        );
        for bad in [
            r#"{"process_bounces":"true"}"#,
            r#"{"process_bounces":null}"#,
            r#"{"process_bounces":1}"#,
        ] {
            assert_eq!(
                call(&app, "PATCH", &path, Some(&token), Some(bad))
                    .await
                    .status(),
                StatusCode::BAD_REQUEST
            );
        }
        assert_eq!(
            response_json(call(&app, "PUT", &path, Some(&token), Some("{}")).await).await["process_bounces"],
            false
        );
        assert_eq!(
            response_json(
                call(
                    &app,
                    "PATCH",
                    &path,
                    Some(&token),
                    Some(r#"{"process_bounces":true}"#)
                )
                .await
            )
            .await["process_bounces"],
            true
        );
        assert_eq!(
            call(
                &app,
                "PATCH",
                &path,
                None,
                Some(r#"{"process_bounces":false}"#)
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            response_json(call(&app, "GET", &path, Some(&token), None).await).await["process_bounces"],
            true
        );
        let response = call_form(&app, "PATCH", &path, &token, "bounce_score_threshold=2.5").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response_json(response).await["bounce_score_threshold"], 2.5);
        assert_eq!(
            response_json(call(&app, "PATCH", &path, Some(&token), Some("{}")).await).await["bounce_score_threshold"],
            2.5
        );
        assert_eq!(
            response_json(call(&app, "PUT", &path, Some(&token), Some("{}")).await).await["bounce_score_threshold"],
            5.0
        );
        let attr = format!("{path}/bounce_score_threshold");
        let response = call(&app, "PUT", &attr, Some(&token), Some("1.25")).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response_json(call(&app, "GET", &attr, Some(&token), None).await).await,
            1.25
        );
        for bad in ["0", "-1", "1000000.1", "null", "true", "\"2.5\""] {
            let body = format!("{{\"bounce_score_threshold\":{bad}}}");
            assert_eq!(
                call(&app, "PATCH", &path, Some(&token), Some(&body))
                    .await
                    .status(),
                StatusCode::BAD_REQUEST
            );
        }
        for bad in ["NaN", "inf", "-inf", "1e999"] {
            assert_eq!(
                call_form(
                    &app,
                    "PATCH",
                    &path,
                    &token,
                    &format!("bounce_score_threshold={bad}")
                )
                .await
                .status(),
                StatusCode::BAD_REQUEST
            );
        }
        duration_config(&app, &token, &path, prefix).await;
    }
}

#[tokio::test]
async fn bounce_threshold_openapi() {
    let (app, _, _) = setup(&["admin"]).await;
    let doc = response_json(call(&app, "GET", "/openapi.json", None, None).await).await;
    for schema in ["ListConfigInput", "ListConfigResponse", "MailingList"] {
        let definition = &doc["components"]["schemas"][schema];
        let field = definition
            .get("properties")
            .and_then(|p| p.get("bounce_score_threshold"))
            .or_else(|| {
                definition["allOf"].as_array().and_then(|items| {
                    items
                        .iter()
                        .find_map(|item| item["properties"].get("bounce_score_threshold"))
                })
            })
            .expect("numeric threshold schema");
        assert!(field["type"].to_string().contains("number"), "{field}");
        assert_eq!(field["exclusiveMinimum"], 0);
        assert_eq!(field["maximum"], 1_000_000);
        assert_eq!(field["default"], 5);
        assert!(
            doc["components"]["schemas"][schema]
                .to_string()
                .contains("process_bounces"),
            "{schema}"
        );
    }
}

async fn duration_config(app: &axum::Router, token: &str, path: &str, prefix: &str) {
    let response = call_form(app, "PATCH", path, token, "bounce_info_stale_after=30").await;
    assert_eq!(response.status(), StatusCode::OK);
    let expected_days = if prefix == "/3.1" {
        json!("30d")
    } else {
        json!(30)
    };
    assert_eq!(
        response_json(response).await["bounce_info_stale_after"],
        expected_days
    );
    let kept = response_json(call(app, "PATCH", path, Some(token), Some("{}")).await).await;
    assert_eq!(kept["bounce_info_stale_after"], expected_days);
    let reset = response_json(call(app, "PUT", path, Some(token), Some("{}")).await).await;
    assert_eq!(
        reset["bounce_info_stale_after"],
        if prefix == "/3.1" {
            json!("7d")
        } else {
            json!(7)
        }
    );
    if prefix == "/3.1" {
        compat_duration(app, token, path).await;
    }
    for bad in ["0", "3651", "-1", "1.5", "null", "true", "\"7\""] {
        let body = format!("{{\"bounce_info_stale_after\":{bad}}}");
        assert_eq!(
            call(app, "PATCH", path, Some(token), Some(&body))
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
}

async fn compat_duration(app: &axum::Router, token: &str, path: &str) {
    let response = call_form(app, "PATCH", path, token, "bounce_info_stale_after=5d").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response_json(response).await["bounce_info_stale_after"],
        "5d"
    );
    let attr = format!("{path}/bounce_info_stale_after");
    assert_eq!(
        response_json(call(app, "GET", &attr, Some(token), None).await).await,
        "5d"
    );
    let response = call(app, "PUT", &attr, Some(token), Some(r#""9d""#)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response_json(response).await["bounce_info_stale_after"],
        "9d"
    );
    for invalid in ["0d", "3651d", "1.5d", "1h", "+7d", " 7d"] {
        let body = json!({"bounce_info_stale_after": invalid}).to_string();
        assert_eq!(
            call(app, "PATCH", path, Some(token), Some(&body))
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
}
