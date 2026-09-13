//! Mailman's site-wide `/bans` on both prefixes: server administration for
//! unbound tokens, matched on every list, kept apart from list bans.
use super::*;

/// The prefix's collection and total keys.
const fn keys(prefix: &str) -> (&'static str, &'static str) {
    if matches!(prefix.as_bytes(), b"/3.1") {
        ("entries", "total_size")
    } else {
        ("items", "total")
    }
}

#[tokio::test]
async fn site_bans_round_trip_on_both_prefixes_and_stay_apart_from_list_bans() {
    let (app, token, _) = setup(&["admin"]).await;
    create_configurable_list(&app, &token).await;
    for prefix in ["/api/v1", "/3.1"] {
        let uri = format!("{prefix}/bans");
        two_site_bans(&app, &token, &uri).await;
        site_bans_are_listed_and_fetched(&app, &token, &uri, keys(prefix)).await;
        site_and_list_bans_are_distinct(&app, &token, prefix, keys(prefix).1).await;
        for email in ["spammer%40example.org", "%5Ebulk%40"] {
            let path = format!("{uri}/{email}");
            assert_eq!(
                call(&app, "DELETE", &path, Some(&token), None)
                    .await
                    .status(),
                StatusCode::NO_CONTENT
            );
            assert_eq!(
                call(&app, "DELETE", &path, Some(&token), None)
                    .await
                    .status(),
                StatusCode::NOT_FOUND
            );
        }
        assert_eq!(
            response_json(call(&app, "GET", &uri, Some(&token), None).await).await[keys(prefix).1],
            0
        );
    }
}

async fn two_site_bans(app: &axum::Router, token: &str, uri: &str) {
    let created = call_form(app, "POST", uri, token, "email=Spammer%40Example.ORG").await;
    assert_eq!(created.status(), StatusCode::CREATED);
    assert_eq!(
        created.headers()[header::LOCATION],
        format!("{uri}/spammer%40example.org")
    );
    assert_eq!(
        response_json(created).await,
        serde_json::json!({
            "email": "spammer@example.org",
            "self_link": format!("{uri}/spammer%40example.org")
        })
    );
    let body = serde_json::json!({"email":"^bulk@"}).to_string();
    assert_eq!(
        call(app, "POST", uri, Some(token), Some(&body))
            .await
            .status(),
        StatusCode::CREATED
    );
    // The same value again is a conflict, not a second row.
    assert_eq!(
        call_form(app, "POST", uri, token, "email=spammer%40example.org")
            .await
            .status(),
        StatusCode::CONFLICT
    );
    for bad in ["email=not%20an%20address", "email=%5E%5B", "email="] {
        assert_eq!(
            call_form(app, "POST", uri, token, bad).await.status(),
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }
}

async fn site_bans_are_listed_and_fetched(
    app: &axum::Router,
    token: &str,
    uri: &str,
    (items, total): (&str, &str),
) {
    let listed = response_json(call(app, "GET", uri, Some(token), None).await).await;
    assert_eq!(listed[total], 2);
    assert_eq!(listed[items][0]["email"], "^bulk@");
    assert_eq!(listed[items][1]["email"], "spammer@example.org");
    let one = response_json(
        call(
            app,
            "GET",
            &format!("{uri}/SPAMMER%40example.org"),
            Some(token),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(one["email"], "spammer@example.org");
    assert_eq!(
        call(
            app,
            "GET",
            &format!("{uri}/nobody%40example.org"),
            Some(token),
            None
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
}

/// Site bans are not the list's bans, and the list's are not the site's.
async fn site_and_list_bans_are_distinct(
    app: &axum::Router,
    token: &str,
    prefix: &str,
    total: &str,
) {
    let uri = format!("{prefix}/bans");
    let list_bans = format!("{prefix}/lists/dev.example.com/bans");
    assert_eq!(
        response_json(call(app, "GET", &list_bans, Some(token), None).await).await[total],
        0
    );
    assert_eq!(
        call_form(app, "POST", &list_bans, token, "email=local%40example.org")
            .await
            .status(),
        StatusCode::CREATED
    );
    for method in ["GET", "DELETE"] {
        assert_eq!(
            call(
                app,
                method,
                &format!("{uri}/local%40example.org"),
                Some(token),
                None
            )
            .await
            .status(),
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        call(
            app,
            "DELETE",
            &format!("{list_bans}/local%40example.org"),
            Some(token),
            None
        )
        .await
        .status(),
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn site_bans_need_an_unbound_token_with_the_list_scopes() {
    // A token bound to one list holds the scopes but is not the server's.
    let (app, token, _) = scoped_app().await;
    for prefix in ["/api/v1", "/3.1"] {
        for (method, suffix, body) in [
            ("GET", "", None),
            ("GET", "/spammer%40example.org", None),
            ("POST", "", Some(r#"{"email":"spammer@example.org"}"#)),
            ("DELETE", "/spammer%40example.org", None),
        ] {
            let uri = format!("{prefix}/bans{suffix}");
            assert_eq!(
                call(&app, method, &uri, Some(&token), body).await.status(),
                StatusCode::FORBIDDEN,
                "{method} {uri}"
            );
        }
    }
    // An unbound reader may list but not write; no token is unauthorized.
    let (app, token, _) = setup(&["lists:read"]).await;
    assert_eq!(
        call(&app, "GET", "/api/v1/bans", Some(&token), None)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/bans",
            Some(&token),
            Some(r#"{"email":"spammer@example.org"}"#)
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    let (app, token, _) = setup(&["system:read"]).await;
    assert_eq!(
        call(&app, "GET", "/api/v1/bans", Some(&token), None)
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "GET", "/api/v1/bans", None, None).await.status(),
        StatusCode::UNAUTHORIZED
    );
}
