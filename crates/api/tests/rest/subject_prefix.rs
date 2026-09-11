use super::*;

#[tokio::test]
async fn subject_prefix_json_and_form_reject_header_breaks_atomically() {
    let (app, token, _) = setup(&["admin"]).await;
    for (path, body) in [
        ("/api/v1/domains", r#"{"mail_host":"example.com"}"#),
        ("/api/v1/lists", r#"{"fqdn_listname":"dev@example.com"}"#),
    ] {
        assert_eq!(
            call(&app, "POST", path, Some(&token), Some(body))
                .await
                .status(),
            StatusCode::CREATED
        );
    }
    for prefix in ["/api/v1", "/3.1"] {
        let path = format!("{prefix}/lists/dev.example.com/config");
        let valid = serde_urlencoded::to_string([("subject_prefix", "[Tiếng Việt] ")]).unwrap();
        let saved = call_form(&app, "PATCH", &path, &token, &valid).await;
        assert_eq!(saved.status(), StatusCode::OK);
        assert_eq!(
            response_json(saved).await["subject_prefix"],
            "[Tiếng Việt] "
        );
        let before = response_json(call(&app, "GET", &path, Some(&token), None).await).await;
        for method in ["PATCH", "PUT"] {
            for bad in ["\r", "\n", "[bad]\r\nBcc: victim@fixture.invalid"] {
                let json =
                    serde_json::json!({"subject_prefix":bad, "display_name":"must not persist"})
                        .to_string();
                let form = serde_urlencoded::to_string([
                    ("subject_prefix", bad),
                    ("display_name", "must not persist"),
                ])
                .unwrap();
                assert_eq!(
                    call(&app, method, &path, Some(&token), Some(&json))
                        .await
                        .status(),
                    StatusCode::BAD_REQUEST
                );
                assert_eq!(
                    call_form(&app, method, &path, &token, &form).await.status(),
                    StatusCode::BAD_REQUEST
                );
                assert_eq!(
                    response_json(call(&app, "GET", &path, Some(&token), None).await).await,
                    before
                );
            }
        }
        let cleared = call(
            &app,
            "PATCH",
            &path,
            Some(&token),
            Some(r#"{"subject_prefix":""}"#),
        )
        .await;
        assert_eq!(cleared.status(), StatusCode::OK);
        assert_eq!(response_json(cleared).await["subject_prefix"], "");
    }
}
