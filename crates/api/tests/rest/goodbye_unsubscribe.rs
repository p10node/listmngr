use super::*;
use serde_json::json;

#[tokio::test]
async fn goodbye_unsubscribe_alias_is_authorized_confirmed_and_member_only() {
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
            Some(r#"{"fqdn_listname":"leave@example.com"}"#)
        )
        .await
        .status(),
        StatusCode::CREATED
    );
    let body = r#"{"pre_confirmed":true,"pre_approved":true}"#;
    for prefix in ["/api/v1", "/3.1"] {
        for role in ["owner", "member"] {
            let input = json!({"list_id":"leave.example.com","subscriber":"Exact+tag@example.com","role":role,"pre_verified":true,"pre_confirmed":true,"pre_approved":true});
            assert_eq!(
                call(
                    &app,
                    "POST",
                    "/api/v1/members",
                    Some(&token),
                    Some(&input.to_string())
                )
                .await
                .status(),
                StatusCode::CREATED
            );
        }
        let path = format!("{prefix}/lists/leave.example.com/member/exact%2Btag%40example.com");
        assert_eq!(
            call(&app, "DELETE", &path, None, Some(body)).await.status(),
            StatusCode::UNAUTHORIZED
        );
        for invalid in [
            "{}",
            r#"{"pre_confirmed":false,"pre_approved":true}"#,
            r#"{"pre_confirmed":true,"pre_approved":false}"#,
            r#"{"pre_confirmed":true,"pre_approved":true,"unexpected":true}"#,
        ] {
            assert_eq!(
                call(&app, "DELETE", &path, Some(&token), Some(invalid))
                    .await
                    .status(),
                StatusCode::BAD_REQUEST
            );
        }
        let response = if prefix == "/3.1" {
            call_form(
                &app,
                "DELETE",
                &path,
                &token,
                "pre_confirmed=True&pre_approved=True",
            )
            .await
        } else {
            call(&app, "DELETE", &path, Some(&token), Some(body)).await
        };
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            call(&app, "DELETE", &path, Some(&token), Some(body))
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_only_owner_and_remove(&app, &token).await;
    }
    let doc = response_json(call(&app, "GET", "/openapi.json", None, None).await).await;
    assert!(doc["paths"]["/api/v1/lists/{id}/member/{email}"]["delete"].is_object());
}

async fn assert_only_owner_and_remove(app: &axum::Router, token: &str) {
    let roster = response_json(
        call(
            app,
            "GET",
            "/api/v1/lists/leave.example.com/roster/member",
            Some(token),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(roster["items"].as_array().unwrap().len(), 0);
    let owners = response_json(
        call(
            app,
            "GET",
            "/api/v1/lists/leave.example.com/roster/owner",
            Some(token),
            None,
        )
        .await,
    )
    .await;
    let owners = owners["items"].as_array().unwrap();
    assert_eq!(owners.len(), 1);
    let owner = owners[0]["id"].as_str().unwrap();
    assert_eq!(
        call(
            app,
            "DELETE",
            &format!("/api/v1/members/{owner}"),
            Some(token),
            None
        )
        .await
        .status(),
        StatusCode::NO_CONTENT
    );
}
