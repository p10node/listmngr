use super::*;

#[tokio::test]
async fn individual_ban_reads_use_stored_identity_not_effective_matching() {
    let (app, token, _) = setup(&["admin"]).await;
    create_configurable_list(&app, &token).await;
    for prefix in ["/api/v1", "/3.1"] {
        let uri = format!("{prefix}/lists/dev.example.com/bans");
        for email in ["blocked@example.net", "^match@"] {
            let body = serde_json::json!({"email":email}).to_string();
            assert_eq!(
                call(&app, "POST", &uri, Some(&token), Some(&body))
                    .await
                    .status(),
                StatusCode::CREATED
            );
        }
        let exact = format!("{uri}/BLOCKED%40Example.NET.");
        let result = call(&app, "GET", &exact, Some(&token), None).await;
        assert_eq!(result.status(), StatusCode::OK);
        assert_eq!(response_json(result).await["email"], "blocked@example.net");
        for email in ["match%40example.net", "missing%40example.net"] {
            assert_eq!(
                call(&app, "GET", &format!("{uri}/{email}"), Some(&token), None)
                    .await
                    .status(),
                StatusCode::NOT_FOUND
            );
        }
        assert_eq!(
            call(&app, "GET", &exact, None, None).await.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(
                &app,
                "GET",
                &format!("{uri}/not-an-address"),
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
                &format!("{prefix}/lists/missing.example.com/bans/blocked%40example.net"),
                Some(&token),
                None
            )
            .await
            .status(),
            StatusCode::NOT_FOUND
        );
        for email in ["blocked%40example.net", "%5Ematch%40"] {
            let path = format!("{uri}/{email}");
            assert_eq!(
                call(&app, "DELETE", &path, Some(&token), None)
                    .await
                    .status(),
                StatusCode::NO_CONTENT
            );
            assert_eq!(
                call(&app, "GET", &path, Some(&token), None).await.status(),
                StatusCode::NOT_FOUND
            );
        }
    }
    let (app, token, _) = setup(&["system:read"]).await;
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/lists/dev.example.com/bans/blocked%40example.net",
            Some(&token),
            None
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn list_bans_enforce_token_scope_and_list_boundaries() {
    let (app, token, _) = scoped_app().await;
    for prefix in ["/api/v1", "/3.1"] {
        for (method, suffix, body) in [
            ("GET", "", None),
            ("GET", "/blocked%40example.net", None),
            ("POST", "", Some(r#"{"email":"blocked@example.net"}"#)),
            ("DELETE", "/blocked%40example.net", None),
        ] {
            let uri = format!("{prefix}/lists/two.second.example/bans{suffix}");
            assert_eq!(
                call(&app, method, &uri, Some(&token), body).await.status(),
                StatusCode::FORBIDDEN
            );
        }
        let uri = format!("{prefix}/lists/one.first.example/bans");
        assert_eq!(
            call(&app, "GET", &uri, Some(&token), None).await.status(),
            StatusCode::OK
        );
    }
    let (app, token, _) = setup(&["system:read"]).await;
    for method in ["GET", "POST", "DELETE"] {
        let suffix = if method == "DELETE" {
            "/blocked%40example.net"
        } else {
            ""
        };
        let uri = format!("/api/v1/lists/dev.example.com/bans{suffix}");
        assert_eq!(
            call(
                &app,
                method,
                &uri,
                Some(&token),
                Some(r#"{"email":"blocked@example.net"}"#)
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
    }
}

#[tokio::test]
async fn regex_ban_location_roundtrips_reserved_characters() {
    let (app, token, _) = setup(&["admin"]).await;
    create_configurable_list(&app, &token).await;
    for prefix in ["/api/v1", "/3.1"] {
        let uri = format!("{prefix}/lists/dev.example.com/bans");
        let regex = r"^tag[+/?#]@outside[.]net$";
        let body = serde_json::json!({"email":regex}).to_string();
        let response = call(&app, "POST", &uri, Some(&token), Some(&body)).await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let location = response.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .to_owned();
        assert_eq!(response_json(response).await["email"], regex);
        let fetched = call(&app, "GET", &location, Some(&token), None).await;
        assert_eq!(fetched.status(), StatusCode::OK);
        let fetched = response_json(fetched).await;
        assert_eq!(fetched["email"], regex);
        assert_eq!(fetched["self_link"], location);
        assert_eq!(
            call(&app, "DELETE", &location, Some(&token), None)
                .await
                .status(),
            StatusCode::NO_CONTENT
        );
    }
}

#[tokio::test]
async fn list_bans_roundtrip_json_form_and_pagination_on_both_prefixes() {
    let (app, token, _) = setup(&["admin"]).await;
    create_configurable_list(&app, &token).await;
    for (prefix, key) in [("/api/v1", "items"), ("/3.1", "entries")] {
        let uri = format!("{prefix}/lists/dev.example.com/bans");
        let response = call(
            &app,
            "POST",
            &uri,
            Some(&token),
            Some(r#"{"email":"BLOCKED@Example.NET."}"#),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(
            response_json(response).await["email"],
            "blocked@example.net"
        );
        assert_eq!(
            call_form(
                &app,
                "POST",
                &uri,
                &token,
                "email=other%2Btag%40example.net"
            )
            .await
            .status(),
            StatusCode::CREATED
        );
        let first =
            response_json(call(&app, "GET", &format!("{uri}?count=1"), Some(&token), None).await)
                .await;
        assert_eq!(first[key][0]["email"], "blocked@example.net");
        let second = response_json(
            call(
                &app,
                "GET",
                &format!("{uri}?count=1&page=2"),
                Some(&token),
                None,
            )
            .await,
        )
        .await;
        assert_eq!(second[key][0]["email"], "other+tag@example.net");
        assert_eq!(
            call(
                &app,
                "POST",
                &uri,
                Some(&token),
                Some(r#"{"email":"blocked@example.net"}"#)
            )
            .await
            .status(),
            StatusCode::CONFLICT
        );
        for email in ["blocked%40example.net", "other%2Btag%40example.net"] {
            assert_eq!(
                call(
                    &app,
                    "DELETE",
                    &format!("{uri}/{email}"),
                    Some(&token),
                    None
                )
                .await
                .status(),
                StatusCode::NO_CONTENT
            );
        }
        let empty = response_json(call(&app, "GET", &uri, Some(&token), None).await).await;
        assert_eq!(empty[key], serde_json::json!([]));
        assert_eq!(
            call(
                &app,
                "DELETE",
                &format!("{uri}/blocked%40example.net"),
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
async fn list_bans_reject_invalid_input_and_require_authorization() {
    let (app, token, _) = setup(&["admin"]).await;
    create_configurable_list(&app, &token).await;
    let uri = "/api/v1/lists/dev.example.com/bans";
    assert_eq!(
        call(&app, "GET", uri, None, None).await.status(),
        StatusCode::UNAUTHORIZED
    );
    for email in ["", "not-an-address", "^[", "a\r\n@example.net"] {
        let body = serde_json::json!({"email":email}).to_string();
        assert_eq!(
            call(&app, "POST", uri, Some(&token), Some(&body))
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        call(&app, "GET", &format!("{uri}?count=0"), Some(&token), None)
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/lists/missing.example.com/bans",
            Some(&token),
            None
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
}
