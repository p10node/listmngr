//! Mailman's `/lists/{id}/header-matches` resource on both prefixes.
use super::*;

const URI: &str = "/3.1/lists/dev.example.com/header-matches";

/// An admin app with the list and two rules: `x-spam-flag` (discard, tagged)
/// at 0 and `subject` at 1.
async fn two_rules() -> (axum::Router, String) {
    let (app, token, _) = setup(&["admin"]).await;
    create_configurable_list(&app, &token).await;
    // mailmanclient posts a form with Mailman's `action` name for the chain
    // and gets the new row's location; the header name is lowercased.
    let created = call_form(
        &app,
        "POST",
        URI,
        &token,
        "header=X-Spam-Flag&pattern=%5Eyes%24&action=discard&tag=spam",
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    assert_eq!(created.headers()[header::LOCATION], format!("{URI}/0"));
    let body = serde_json::json!({"header":"subject","pattern":"viagra"}).to_string();
    let second = call(&app, "POST", URI, Some(&token), Some(&body)).await;
    assert_eq!(second.status(), StatusCode::CREATED);
    assert_eq!(second.headers()[header::LOCATION], format!("{URI}/1"));
    (app, token)
}

#[tokio::test]
async fn header_matches_are_listed_in_mailman_shape_and_bad_rules_refused() {
    let (app, token) = two_rules().await;
    // The same header and pattern again is Mailman's 400, not a second row.
    assert_eq!(
        call_form(&app, "POST", URI, &token, "header=Subject&pattern=viagra")
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    // Mailman's `defer` action names no chain; an unknown one is refused too.
    for form in [
        "header=x&pattern=y&action=defer",
        "header=x&pattern=y&action=bounce",
        "header=x&pattern=%5E%28",
        "header=&pattern=y",
    ] {
        assert_eq!(
            call_form(&app, "POST", URI, &token, form).await.status(),
            StatusCode::BAD_REQUEST
        );
    }
    let listed = response_json(call(&app, "GET", URI, Some(&token), None).await).await;
    assert_eq!(listed["total_size"], 2);
    assert_eq!(
        listed["entries"][0],
        serde_json::json!({
            "header": "x-spam-flag",
            "pattern": "^yes$",
            "position": 0,
            "action": "discard",
            "tag": "spam",
            "self_link": format!("{URI}/0"),
            "http_etag": "phase1"
        })
    );
    // A row without a chain or tag omits them, as Mailman does.
    assert_eq!(
        listed["entries"][1],
        serde_json::json!({
            "header": "subject",
            "pattern": "viagra",
            "position": 1,
            "self_link": format!("{URI}/1"),
            "http_etag": "phase1"
        })
    );
    assert_eq!(
        call(
            &app,
            "GET",
            "/3.1/lists/missing.example.com/header-matches",
            Some(&token),
            None
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn patch_changes_and_moves_one_rule_while_put_replaces_it() {
    let (app, token) = two_rules().await;
    let at = |position: &str| format!("{URI}/{position}");
    let patched = call_form(&app, "PATCH", &at("1"), &token, "position=0&tag=ads").await;
    assert_eq!(patched.status(), StatusCode::NO_CONTENT);
    let moved = response_json(call(&app, "GET", &at("0"), Some(&token), None).await).await;
    assert_eq!(moved["header"], "subject");
    assert_eq!(moved["tag"], "ads");
    assert_eq!(moved["position"], 0);
    // PUT replaces the whole row, clearing the optional fields it lacks.
    let put = call_form(
        &app,
        "PUT",
        &at("1"),
        &token,
        "header=X-Spam-Level&pattern=%5E%5C%2A%7B5%2C%7D",
    )
    .await;
    assert_eq!(put.status(), StatusCode::NO_CONTENT);
    let replaced = response_json(call(&app, "GET", &at("1"), Some(&token), None).await).await;
    assert_eq!(replaced["header"], "x-spam-level");
    assert_eq!(replaced["pattern"], "^\\*{5,}");
    assert!(replaced.get("action").is_none() && replaced.get("tag").is_none());
    // PUT needs the header and the pattern; a position past the end is 400.
    assert_eq!(
        call_form(&app, "PUT", &at("1"), &token, "header=x")
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call_form(&app, "PATCH", &at("1"), &token, "position=2")
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    // Mailman's route only matches integers; anything else is not found.
    for position in ["2", "-1", "+1", "first"] {
        for method in ["GET", "PATCH", "PUT", "DELETE"] {
            let body =
                (method == "PATCH" || method == "PUT").then_some(r#"{"header":"x","pattern":"y"}"#);
            assert_eq!(
                call(&app, method, &at(position), Some(&token), body)
                    .await
                    .status(),
                StatusCode::NOT_FOUND,
                "{method} {position}"
            );
        }
    }
}

#[tokio::test]
async fn find_filters_by_header_tag_and_action_keeping_real_positions() {
    let (app, token) = two_rules().await;
    let find = format!("{URI}/find");
    for (form, expected) in [
        ("header=Subject", vec![1]),
        ("tag=spam", vec![0]),
        ("action=discard", vec![0]),
        ("action=hold", vec![]),
        ("header=x-spam-flag&tag=spam&action=discard", vec![0]),
        ("header=x-spam-flag&tag=ads", vec![]),
        ("", vec![0, 1]),
    ] {
        let found = response_json(call_form(&app, "POST", &find, &token, form).await).await;
        let positions: Vec<u64> = found["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["position"].as_u64().unwrap())
            .collect();
        assert_eq!(positions, expected, "{form}");
        assert_eq!(found["total_size"], expected.len(), "{form}");
    }
    assert_eq!(
        call_form(&app, "POST", &find, &token, "action=defer")
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    let body = serde_json::json!({"chain":"discard"}).to_string();
    let native = response_json(
        call(
            &app,
            "POST",
            "/api/v1/lists/dev.example.com/header-matches/find",
            Some(&token),
            Some(&body),
        )
        .await,
    )
    .await;
    assert_eq!(native["total"], 1);
    assert_eq!(native["items"][0]["chain"], "discard");
}

#[tokio::test]
async fn deleting_one_rule_shifts_the_rest_and_deleting_all_empties_the_list() {
    let (app, token) = two_rules().await;
    assert_eq!(
        call(&app, "DELETE", &format!("{URI}/0"), Some(&token), None)
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    let remaining = response_json(call(&app, "GET", URI, Some(&token), None).await).await;
    assert_eq!(remaining["total_size"], 1);
    assert_eq!(remaining["entries"][0]["header"], "subject");
    assert_eq!(remaining["entries"][0]["position"], 0);
    assert_eq!(
        call(&app, "DELETE", URI, Some(&token), None).await.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        response_json(call(&app, "GET", URI, Some(&token), None).await).await["total_size"],
        0
    );
}

#[tokio::test]
async fn native_header_matches_use_the_typed_chain_name_and_json_bodies() {
    let (app, token, _) = setup(&["admin"]).await;
    create_configurable_list(&app, &token).await;
    let uri = "/api/v1/lists/dev.example.com/header-matches";
    let body =
        serde_json::json!({"header":"List-Id","pattern":"other","chain":"reject","tag":null})
            .to_string();
    let created = call(&app, "POST", uri, Some(&token), Some(&body)).await;
    assert_eq!(created.status(), StatusCode::CREATED);
    assert_eq!(
        created.headers()[header::LOCATION],
        "/api/v1/lists/dev.example.com/header-matches/0"
    );
    assert_eq!(
        response_json(created).await,
        serde_json::json!({
            "header": "list-id",
            "pattern": "other",
            "position": 0,
            "chain": "reject",
            "tag": null,
            "self_link": "/api/v1/lists/dev.example.com/header-matches/0"
        })
    );
    // The typed prefix still accepts Mailman's `action` spelling.
    let body = serde_json::json!({"header":"x","pattern":"y","action":"accept"}).to_string();
    assert_eq!(
        call(&app, "POST", uri, Some(&token), Some(&body))
            .await
            .status(),
        StatusCode::CREATED
    );
    let body = serde_json::json!({"chain":null}).to_string();
    assert_eq!(
        call(
            &app,
            "PATCH",
            &format!("{uri}/1"),
            Some(&token),
            Some(&body)
        )
        .await
        .status(),
        StatusCode::NO_CONTENT
    );
    let listed = response_json(call(&app, "GET", uri, Some(&token), None).await).await;
    assert_eq!(listed["total"], 2);
    assert_eq!(listed["items"][1]["chain"], serde_json::Value::Null);
    assert!(listed["items"][1].get("action").is_none());
    // Unknown fields are refused on both prefixes.
    let body = serde_json::json!({"header":"x","pattern":"z","extra":1}).to_string();
    assert_eq!(
        call(&app, "POST", uri, Some(&token), Some(&body))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn header_matches_enforce_token_scope_and_list_boundaries() {
    let (app, token, _) = scoped_app().await;
    for prefix in ["/api/v1", "/3.1"] {
        for (method, suffix, body) in [
            ("GET", "", None),
            ("GET", "/0", None),
            ("POST", "", Some(r#"{"header":"x","pattern":"y"}"#)),
            ("PATCH", "/0", Some(r#"{"tag":"t"}"#)),
            ("PUT", "/0", Some(r#"{"header":"x","pattern":"y"}"#)),
            ("DELETE", "/0", None),
            ("DELETE", "", None),
            ("POST", "/find", Some(r#"{"tag":"t"}"#)),
        ] {
            let uri = format!("{prefix}/lists/two.second.example/header-matches{suffix}");
            assert_eq!(
                call(&app, method, &uri, Some(&token), body).await.status(),
                StatusCode::FORBIDDEN,
                "{method} {uri}"
            );
        }
        let uri = format!("{prefix}/lists/one.first.example/header-matches");
        assert_eq!(
            call(&app, "GET", &uri, Some(&token), None).await.status(),
            StatusCode::OK
        );
    }
    let (app, token, _) = setup(&["system:read"]).await;
    for (method, body) in [
        ("GET", None),
        ("POST", Some(r#"{"header":"x","pattern":"y"}"#)),
        ("DELETE", None),
    ] {
        assert_eq!(
            call(
                &app,
                method,
                "/api/v1/lists/dev.example.com/header-matches",
                Some(&token),
                body
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/lists/dev.example.com/header-matches",
            None,
            None
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
}
