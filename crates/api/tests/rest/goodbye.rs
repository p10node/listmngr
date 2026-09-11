use super::*;

#[tokio::test]
async fn goodbye_config_json_form_patch_put_and_openapi() {
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
        let response = call_form(&app, "PATCH", &path, &token, "send_goodbye_message=true").await;
        assert_eq!(response.status(), StatusCode::OK, "form enablement");
        assert_eq!(response_json(response).await["send_goodbye_message"], true);
        assert_eq!(
            response_json(call(&app, "PATCH", &path, Some(&token), Some("{}")).await).await["send_goodbye_message"],
            true
        );
        for bad in [
            r#"{"send_goodbye_message":"true"}"#,
            r#"{"send_goodbye_message":null}"#,
            r#"{"send_goodbye_message":1}"#,
        ] {
            assert_eq!(
                call(&app, "PATCH", &path, Some(&token), Some(bad))
                    .await
                    .status(),
                StatusCode::BAD_REQUEST
            );
        }
        assert_eq!(
            response_json(call(&app, "PUT", &path, Some(&token), Some("{}")).await).await["send_goodbye_message"],
            false
        );
        assert_eq!(
            response_json(
                call(
                    &app,
                    "PATCH",
                    &path,
                    Some(&token),
                    Some(r#"{"send_goodbye_message":true}"#)
                )
                .await
            )
            .await["send_goodbye_message"],
            true
        );
        assert_eq!(
            call(
                &app,
                "PATCH",
                &path,
                None,
                Some(r#"{"send_goodbye_message":false}"#)
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            response_json(call(&app, "GET", &path, Some(&token), None).await).await["send_goodbye_message"],
            true
        );
    }
    let doc = response_json(call(&app, "GET", "/openapi.json", None, None).await).await;
    for schema in ["ListConfigInput", "ListConfigResponse", "MailingList"] {
        assert!(
            doc["components"]["schemas"][schema]
                .to_string()
                .contains("send_goodbye_message"),
            "{schema}"
        );
    }
}
