use super::*;

async fn admin_role_fixture() -> (axum::Router, String) {
    let (app, token, _) = setup(&["admin"]).await;
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/domains",
            Some(&token),
            Some(r#"{"mail_host":"roles.example"}"#)
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
            Some(r#"{"fqdn_listname":"test@roles.example"}"#)
        )
        .await
        .status(),
        StatusCode::CREATED
    );
    (app, token)
}

#[tokio::test]
async fn mailmanclient_admin_roles_do_not_require_or_imply_address_verification() {
    let (app, token) = admin_role_fixture().await;
    for role in ["owner", "moderator"] {
        let form =
            format!("list_id=test.roles.example&subscriber={role}%40roles.example&role={role}");
        assert_eq!(
            call_form(&app, "POST", "/3.1/members", &token, &form)
                .await
                .status(),
            StatusCode::CREATED
        );
        let response = call(
            &app,
            "GET",
            &format!("/api/v1/addresses/{role}%40roles.example"),
            Some(&token),
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response_json(response).await.get("verified_on"),
            Some(&serde_json::Value::Null)
        );
    }
    for prefix in ["/api/v1", "/3.1"] {
        for role in ["member", "nonmember"] {
            let body = format!(
                r#"{{"list_id":"test.roles.example","subscriber":"unconfirmed@roles.example","role":"{role}"}}"#
            );
            assert_eq!(
                call(
                    &app,
                    "POST",
                    &format!("{prefix}/members"),
                    Some(&token),
                    Some(&body)
                )
                .await
                .status(),
                StatusCode::BAD_REQUEST
            );
        }
    }
    let body = r#"{"list_id":"test.roles.example","subscriber":"unauthorized@roles.example","role":"owner"}"#;
    assert_eq!(
        call(&app, "POST", "/3.1/members", None, Some(body))
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, "POST", "/api/v1/members", Some(&token), Some(body))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    let invitation = r#"{"list_id":"test.roles.example","subscriber":"invite@roles.example","role":"owner","invitation":true}"#;
    assert_eq!(
        call(&app, "POST", "/3.1/members", Some(&token), Some(invitation))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    let verified = r#"{"list_id":"test.roles.example","subscriber":"verified@roles.example","role":"owner","pre_verified":true}"#;
    assert_eq!(
        call(&app, "POST", "/3.1/members", Some(&token), Some(verified))
            .await
            .status(),
        StatusCode::CREATED
    );
    let address = response_json(
        call(
            &app,
            "GET",
            "/api/v1/addresses/verified%40roles.example",
            Some(&token),
            None,
        )
        .await,
    )
    .await;
    assert!(
        address
            .get("verified_on")
            .is_some_and(|value| !value.is_null())
    );
}

#[tokio::test]
async fn bounce_notice_config_json_form_patch_put_and_openapi() {
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
    for prefix in ["/api/v1", "/3.1"] {
        let path = format!("{prefix}/lists/dev.example.com/config");
        assert_eq!(
            response_json(call(&app, "GET", &path, Some(&token), None).await).await["bounce_notify_owner_on_disable"],
            true
        );
        let response = call_form(
            &app,
            "PATCH",
            &path,
            &token,
            "bounce_notify_owner_on_disable=False",
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response_json(response).await["bounce_notify_owner_on_disable"],
            false
        );
        assert_eq!(
            response_json(call(&app, "PATCH", &path, Some(&token), Some("{}")).await).await["bounce_notify_owner_on_disable"],
            false
        );
        for bad in [
            r#"{"bounce_notify_owner_on_disable":"true"}"#,
            r#"{"bounce_notify_owner_on_disable":null}"#,
            r#"{"bounce_notify_owner_on_disable":1}"#,
        ] {
            assert_eq!(
                call(&app, "PATCH", &path, Some(&token), Some(bad))
                    .await
                    .status(),
                StatusCode::BAD_REQUEST
            );
        }
        assert_eq!(
            response_json(call(&app, "PUT", &path, Some(&token), Some("{}")).await).await["bounce_notify_owner_on_disable"],
            true
        );
        assert_eq!(
            response_json(
                call_form(
                    &app,
                    "PATCH",
                    &path,
                    &token,
                    "bounce_notify_owner_on_disable=True"
                )
                .await
            )
            .await["bounce_notify_owner_on_disable"],
            true
        );
        let attr = format!("{path}/bounce_notify_owner_on_disable");
        assert_eq!(
            call(&app, "GET", &attr, Some(&token), None).await.status(),
            StatusCode::OK
        );
    }
    let doc = response_json(call(&app, "GET", "/openapi.json", None, None).await).await;
    for schema in ["ListConfigInput", "ListConfigResponse", "MailingList"] {
        assert!(
            doc["components"]["schemas"][schema]
                .to_string()
                .contains("bounce_notify_owner_on_disable")
        );
    }
}
