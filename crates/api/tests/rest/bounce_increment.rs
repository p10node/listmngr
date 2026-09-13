use super::*;

#[tokio::test]
async fn bounce_increment_config_json_form_patch_put_and_openapi() {
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
            response_json(call(&app, "GET", &path, Some(&token), None).await).await["bounce_notify_owner_on_bounce_increment"],
            false
        );
        let response = call_form(
            &app,
            "PATCH",
            &path,
            &token,
            "bounce_notify_owner_on_bounce_increment=True",
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response_json(response).await["bounce_notify_owner_on_bounce_increment"],
            true
        );
        assert_eq!(
            response_json(call(&app, "PATCH", &path, Some(&token), Some("{}")).await).await["bounce_notify_owner_on_bounce_increment"],
            true
        );
        for bad in [
            r#"{"bounce_notify_owner_on_bounce_increment":"true"}"#,
            r#"{"bounce_notify_owner_on_bounce_increment":null}"#,
            r#"{"bounce_notify_owner_on_bounce_increment":1}"#,
        ] {
            assert_eq!(
                call(&app, "PATCH", &path, Some(&token), Some(bad))
                    .await
                    .status(),
                StatusCode::BAD_REQUEST
            );
        }
        assert_eq!(
            response_json(call(&app, "PUT", &path, Some(&token), Some("{}")).await).await["bounce_notify_owner_on_bounce_increment"],
            false
        );
        assert_eq!(
            response_json(
                call_form(
                    &app,
                    "PATCH",
                    &path,
                    &token,
                    "bounce_notify_owner_on_bounce_increment=False"
                )
                .await
            )
            .await["bounce_notify_owner_on_bounce_increment"],
            false
        );
        let attr = format!("{path}/bounce_notify_owner_on_bounce_increment");
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
                .contains("bounce_notify_owner_on_bounce_increment")
        );
    }
}
