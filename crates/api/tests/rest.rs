#[path = "rest/alter_messages.rs"]
mod alter_messages;
#[path = "rest/autoresponder.rs"]
mod autoresponder;
#[path = "rest/bans.rs"]
mod bans;
#[path = "rest/bounce_increment.rs"]
mod bounce_increment;
#[path = "rest/bounce_maintenance.rs"]
mod bounce_maintenance;
#[path = "rest/bounce_notice.rs"]
mod bounce_notice;
#[path = "rest/bounce_score.rs"]
mod bounce_score;
#[path = "rest/goodbye.rs"]
mod goodbye;
#[path = "rest/goodbye_unsubscribe.rs"]
mod goodbye_unsubscribe;
#[path = "rest/mta_maps.rs"]
mod mta_maps;
#[path = "rest/requests.rs"]
mod requests;
#[path = "rest/smtp_bounces.rs"]
mod smtp_bounces;
#[path = "rest/subject_prefix.rs"]
mod subject_prefix;
#[path = "rest/subscribe.rs"]
mod subscribe;
#[path = "rest/template_uris.rs"]
mod template_uris;
#[path = "rest/welcome.rs"]
mod welcome;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
    response::Response,
};
use listmngr_core::{Config, UserId};
use listmngr_db::{Database, NewUser};
use tower::ServiceExt;

fn config_with_rate(limit: u32) -> Config {
    let mut config = Config::default();
    config.security.rate_limit.api = format!("{limit}/min");
    config
}

async fn setup(scopes: &[&str]) -> (axum::Router, String, UserId) {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
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
    let app = listmngr_api::router(db, config_with_rate(100));
    (app, token, user.id)
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    json: Option<&str>,
) -> Response {
    call_from(
        app,
        method,
        uri,
        token,
        json,
        "127.0.0.1:4242".parse().unwrap(),
    )
    .await
}

async fn call_from(
    app: &axum::Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    json: Option<&str>,
    peer: std::net::SocketAddr,
) -> Response {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    if json.is_some() {
        request = request.header(header::CONTENT_TYPE, "application/json");
    }
    let mut request = request
        .body(Body::from(json.unwrap_or_default().to_owned()))
        .unwrap();
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(peer));
    app.clone().oneshot(request).await.unwrap()
}

async fn call_form(
    app: &axum::Router,
    method: &str,
    uri: &str,
    token: &str,
    form: &str,
) -> Response {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(form.to_owned()))
        .unwrap();
    request.extensions_mut().insert(axum::extract::ConnectInfo(
        "127.0.0.1:4242".parse::<std::net::SocketAddr>().unwrap(),
    ));
    app.clone().oneshot(request).await.unwrap()
}

async fn call_authorization(app: &axum::Router, authorization: &str) -> Response {
    call_authorization_from(
        app,
        "/api/v1/system/versions",
        authorization,
        "127.0.0.1:4242".parse().unwrap(),
        None,
    )
    .await
}

async fn call_authorization_from(
    app: &axum::Router,
    uri: &str,
    authorization: &str,
    peer: std::net::SocketAddr,
    forwarded_for: Option<&str>,
) -> Response {
    let mut builder = Request::builder()
        .uri(uri)
        .header(header::AUTHORIZATION, authorization);
    if let Some(value) = forwarded_for {
        builder = builder.header("x-forwarded-for", value);
    }
    let mut request = builder.body(Body::empty()).unwrap();
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(peer));
    app.clone().oneshot(request).await.unwrap()
}

async fn response_json(response: Response) -> serde_json::Value {
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

fn assert_json_subset(expected: &serde_json::Value, actual: &serde_json::Value, path: &str) {
    match expected {
        serde_json::Value::Object(expected) => {
            let actual = actual
                .as_object()
                .unwrap_or_else(|| panic!("{path} must be an object: {actual}"));
            for (key, value) in expected {
                assert_json_subset(
                    value,
                    actual
                        .get(key)
                        .unwrap_or_else(|| panic!("{path}.{key} missing from {actual:?}")),
                    &format!("{path}.{key}"),
                );
            }
        }
        serde_json::Value::Array(expected) => {
            let actual = actual
                .as_array()
                .unwrap_or_else(|| panic!("{path} must be an array: {actual}"));
            assert_eq!(actual.len(), expected.len(), "{path} length");
            for (index, value) in expected.iter().enumerate() {
                assert_json_subset(value, &actual[index], &format!("{path}[{index}]"));
            }
        }
        _ => assert_eq!(expected, actual, "{path}"),
    }
}

#[tokio::test]
async fn health_ready_metrics_and_openapi_are_public() {
    let (app, _, _) = setup(&["admin"]).await;
    for path in [
        "/healthz",
        "/readyz",
        "/metrics",
        "/openapi.json",
        "/api/docs",
    ] {
        assert_eq!(
            call(&app, "GET", path, None, None).await.status(),
            StatusCode::OK,
            "{path}"
        );
    }
    let response = call(&app, "GET", "/openapi.json", None, None).await;
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(value["paths"]["/api/v1/domains"].is_object());
}

#[tokio::test]
async fn typed_get_responses_have_stable_content_etags_but_compat_responses_do_not() {
    let (app, token, _) = setup(&["admin"]).await;
    let first = call(&app, "GET", "/api/v1/system/versions", Some(&token), None).await;
    let second = call(&app, "GET", "/api/v1/system/versions", Some(&token), None).await;
    let first_etag = first
        .headers()
        .get(header::ETAG)
        .expect("typed GET response must have an ETag")
        .clone();
    assert_eq!(second.headers().get(header::ETAG), Some(&first_etag));

    let compat = call(&app, "GET", "/3.1/system/versions", Some(&token), None).await;
    assert!(compat.headers().get(header::ETAG).is_none());
}

#[tokio::test]
async fn mailman_create_domain_returns_followable_canonical_resource_links() {
    let (app, token, _) = setup(&["admin"]).await;
    let response = call(
        &app,
        "POST",
        "/3.1/domains",
        Some(&token),
        Some(r#"{"mail_host":"example.com","description":"Example domain"}"#),
    )
    .await;

    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(
        response.headers()[header::LOCATION],
        "/3.1/domains/example.com"
    );
    let body = response_json(response).await;
    assert_eq!(body["self_link"], "/3.1/domains/example.com");
    assert_eq!(body["mail_host"], "example.com");

    let followed = call(
        &app,
        "GET",
        body["self_link"].as_str().unwrap(),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(followed.status(), StatusCode::OK);
    assert_eq!(
        response_json(followed).await["self_link"],
        "/3.1/domains/example.com"
    );
}

#[tokio::test]
async fn mailman_create_list_accepts_fqdn_and_returns_followable_canonical_links() {
    let (app, token, _) = setup(&["admin"]).await;
    assert_eq!(
        call_form(
            &app,
            "POST",
            "/3.1/domains",
            &token,
            "mail_host=example.com",
        )
        .await
        .status(),
        StatusCode::CREATED
    );

    let response = call_form(
        &app,
        "POST",
        "/3.1/lists",
        &token,
        "fqdn_listname=dev%40example.com&style_name=legacy-default",
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(
        response.headers()[header::LOCATION],
        "/3.1/lists/dev.example.com"
    );
    let body = response_json(response).await;
    assert_eq!(body["self_link"], "/3.1/lists/dev.example.com");
    assert_eq!(body["list_id"], "dev.example.com");
    assert_eq!(body["fqdn_listname"], "dev@example.com");
    assert_eq!(body["display_name"], "dev");

    let followed = call(
        &app,
        "GET",
        body["self_link"].as_str().unwrap(),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(followed.status(), StatusCode::OK);
    assert_eq!(
        response_json(followed).await["self_link"],
        "/3.1/lists/dev.example.com"
    );
}

#[tokio::test]
async fn mailman_subscribe_form_accepts_python_boolean_spelling() {
    let (app, token, _) = setup(&["admin"]).await;
    assert_eq!(
        call(
            &app,
            "POST",
            "/3.1/domains",
            Some(&token),
            Some(r#"{"mail_host":"example.com"}"#),
        )
        .await
        .status(),
        StatusCode::CREATED
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/3.1/lists",
            Some(&token),
            Some(r#"{"list_id":"dev.example.com","display_name":"Dev"}"#),
        )
        .await
        .status(),
        StatusCode::CREATED
    );

    let response = call_form(
        &app,
        "POST",
        "/3.1/members",
        &token,
        "list_id=dev.example.com&subscriber=person%40example.net&pre_verified=True&pre_confirmed=True&pre_approved=True",
    )
    .await;

    assert_eq!(response.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn mailman_list_config_accepts_fqdn_listname_path() {
    let (app, token, _) = setup(&["admin"]).await;
    assert_eq!(
        call(
            &app,
            "POST",
            "/3.1/domains",
            Some(&token),
            Some(r#"{"mail_host":"example.com"}"#),
        )
        .await
        .status(),
        StatusCode::CREATED
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/3.1/lists",
            Some(&token),
            Some(r#"{"list_id":"dev.example.com","display_name":"Dev"}"#),
        )
        .await
        .status(),
        StatusCode::CREATED
    );

    let response = call(
        &app,
        "GET",
        "/3.1/lists/dev@example.com/config",
        Some(&token),
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response_json(response).await["list_id"], "dev.example.com");
}

#[tokio::test]
async fn mailman_list_config_patch_accepts_form_at_fqdn_path() {
    let (app, token, _) = setup(&["admin"]).await;
    assert_eq!(
        call(
            &app,
            "POST",
            "/3.1/domains",
            Some(&token),
            Some(r#"{"mail_host":"example.com"}"#),
        )
        .await
        .status(),
        StatusCode::CREATED
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/3.1/lists",
            Some(&token),
            Some(r#"{"list_id":"dev.example.com","display_name":"Dev"}"#),
        )
        .await
        .status(),
        StatusCode::CREATED
    );

    let response = call_form(
        &app,
        "PATCH",
        "/3.1/lists/dev@example.com/config",
        &token,
        "description=mailmanclient+round+trip",
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response_json(response).await["description"],
        "mailmanclient round trip"
    );
}

#[tokio::test]
async fn mailman_roster_entries_include_address_and_member_links() {
    let (app, token, _) = setup(&["admin"]).await;
    assert_eq!(
        call(
            &app,
            "POST",
            "/3.1/domains",
            Some(&token),
            Some(r#"{"mail_host":"example.com"}"#),
        )
        .await
        .status(),
        StatusCode::CREATED
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/3.1/lists",
            Some(&token),
            Some(r#"{"list_id":"dev.example.com","display_name":"Dev"}"#),
        )
        .await
        .status(),
        StatusCode::CREATED
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/3.1/members",
            Some(&token),
            Some(r#"{"list_id":"dev.example.com","subscriber":"person@example.net","pre_verified":true,"pre_confirmed":true,"pre_approved":true}"#),
        )
        .await
        .status(),
        StatusCode::CREATED
    );

    let body = response_json(
        call(
            &app,
            "GET",
            "/3.1/lists/dev.example.com/roster/member",
            Some(&token),
            None,
        )
        .await,
    )
    .await;
    let entry = &body["entries"][0];

    assert_eq!(entry["address"], "/3.1/addresses/person@example.net");
    assert!(
        entry["self_link"]
            .as_str()
            .unwrap()
            .starts_with("/3.1/members/")
    );
}

#[tokio::test]
async fn mailman_subscribe_returns_followable_member_location() {
    let (app, token, _) = setup(&["admin"]).await;
    assert_eq!(
        call(
            &app,
            "POST",
            "/3.1/domains",
            Some(&token),
            Some(r#"{"mail_host":"example.com"}"#),
        )
        .await
        .status(),
        StatusCode::CREATED
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/3.1/lists",
            Some(&token),
            Some(r#"{"list_id":"dev.example.com","display_name":"Dev"}"#),
        )
        .await
        .status(),
        StatusCode::CREATED
    );

    let response = call_form(
        &app,
        "POST",
        "/3.1/members",
        &token,
        "list_id=dev.example.com&subscriber=person%40example.net&pre_verified=True&pre_confirmed=True&pre_approved=True",
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let location = response.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(location.starts_with("/3.1/members/"));

    let followed = call(&app, "GET", &location, Some(&token), None).await;
    assert_eq!(followed.status(), StatusCode::OK);
    let body = response_json(followed).await;
    assert_eq!(body["email"], "person@example.net");
    assert_eq!(body["self_link"], location);
    assert_eq!(body["address"], "/3.1/addresses/person@example.net");
}

#[tokio::test]
async fn committed_mailman_json_fixtures_conform_to_live_compat_responses() {
    let (app, token, _) = setup(&["admin"]).await;
    assert_eq!(
        call(
            &app,
            "POST",
            "/3.1/domains",
            Some(&token),
            Some(r#"{"mail_host":"example.com","description":"Example domain"}"#)
        )
        .await
        .status(),
        StatusCode::CREATED
    );
    assert_eq!(call(&app, "POST", "/3.1/lists", Some(&token), Some(r#"{"list_id":"dev.example.com","display_name":"Developers","style":"legacy-default"}"#)).await.status(), StatusCode::CREATED);
    assert_eq!(call(&app, "PATCH", "/3.1/lists/dev.example.com/config", Some(&token), Some(r#"{"description":"Discussion","advertised":true,"preferred_language":"en","archive_policy":"public","archive_rendering_mode":"text"}"#)).await.status(), StatusCode::OK);
    assert_eq!(call(&app, "POST", "/3.1/members", Some(&token), Some(r#"{"list_id":"dev.example.com","subscriber":"subscriber@example.net","role":"member","display_name":"Subscriber","pre_verified":true,"pre_confirmed":true,"pre_approved":true}"#)).await.status(), StatusCode::CREATED);

    for (fixture, uri) in [
        (
            include_str!("../../../tests/compat/fixtures/mailman-3.3/system-versions.json"),
            "/3.1/system/versions",
        ),
        (
            include_str!("../../../tests/compat/fixtures/mailman-3.3/domains.json"),
            "/3.1/domains",
        ),
        (
            include_str!("../../../tests/compat/fixtures/mailman-3.3/list-config.json"),
            "/3.1/lists/dev.example.com/config",
        ),
        (
            include_str!("../../../tests/compat/fixtures/mailman-3.3/member-roster.json"),
            "/3.1/lists/dev.example.com/roster/member",
        ),
    ] {
        let expected: serde_json::Value = serde_json::from_str(fixture).unwrap();
        let actual = response_json(call(&app, "GET", uri, Some(&token), None).await).await;
        assert_json_subset(&expected, &actual, uri);
    }
}

#[tokio::test]
async fn bearer_scopes_protect_writes_and_both_prefixes_share_crud() {
    let (app, token, _) = setup(&["admin"]).await;
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/domains",
            None,
            Some(r#"{"mail_host":"example.com"}"#)
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/domains",
            Some(&token),
            Some(r#"{"mail_host":"example.com","description":"Example"}"#)
        )
        .await
        .status(),
        StatusCode::CREATED
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/3.1/lists",
            Some(&token),
            Some(r#"{"list_id":"dev.example.com","display_name":"Dev","style":"legacy-default"}"#)
        )
        .await
        .status(),
        StatusCode::CREATED
    );
    assert_eq!(
        call(
            &app,
            "PATCH",
            "/api/v1/lists/dev.example.com/config",
            Some(&token),
            Some(r#"{"advertised":false,"description":"Rust"}"#)
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert_eq!(call(&app, "POST", "/3.1/members", Some(&token), Some(r#"{"list_id":"dev.example.com","subscriber":"person@example.com","role":"member","pre_verified":true,"pre_confirmed":true,"pre_approved":true}"#)).await.status(), StatusCode::CREATED);
    let roster = call(
        &app,
        "GET",
        "/api/v1/lists/dev.example.com/roster/member",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(roster.status(), StatusCode::OK);
    let body = to_bytes(roster.into_body(), usize::MAX).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["items"].as_array().unwrap().len(), 1);
}

async fn create_configurable_list(app: &axum::Router, token: &str) {
    assert_eq!(
        call(
            app,
            "POST",
            "/api/v1/domains",
            Some(token),
            Some(r#"{"mail_host":"example.com"}"#),
        )
        .await
        .status(),
        StatusCode::CREATED
    );
    assert_eq!(
        call(
            app,
            "POST",
            "/api/v1/lists",
            Some(token),
            Some(r#"{"list_id":"dev.example.com","display_name":"Dev","style":"legacy-default"}"#),
        )
        .await
        .status(),
        StatusCode::CREATED
    );
}

#[tokio::test]
async fn dmarc_config_roundtrips_and_accepts_every_mailman_action() {
    let (app, token, _) = setup(&["admin"]).await;
    create_configurable_list(&app, &token).await;
    for prefix in ["/api/v1", "/3.1"] {
        let uri = format!("{prefix}/lists/dev.example.com/config");
        for (method, payload, status) in [
            (
                "PATCH",
                r#"{"dmarc_mitigate_action":"munge_from","dmarc_mitigate_unconditionally":true}"#,
                StatusCode::OK,
            ),
            (
                "PATCH",
                r#"{"dmarc_mitigate_action":"wrap_message"}"#,
                StatusCode::BAD_REQUEST,
            ),
            (
                "PATCH",
                r#"{"dmarc_mitigate_action":"reject"}"#,
                StatusCode::OK,
            ),
            (
                "PATCH",
                r#"{"dmarc_mitigate_action":"discard"}"#,
                StatusCode::OK,
            ),
            (
                "PATCH",
                r#"{"dmarc_mitigate_action":"munge_from","dmarc_mitigate_unconditionally":false}"#,
                StatusCode::OK,
            ),
            (
                "PUT",
                r#"{"dmarc_mitigate_action":"munge_from","dmarc_mitigate_unconditionally":true}"#,
                StatusCode::OK,
            ),
        ] {
            assert_eq!(
                call(&app, method, &uri, Some(&token), Some(payload))
                    .await
                    .status(),
                status,
                "{prefix} {payload}"
            );
        }
        let response = call(&app, "GET", &uri, Some(&token), None).await;
        let value: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(value["dmarc_mitigate_action"], "munge_from");
        assert_eq!(value["dmarc_mitigate_unconditionally"], true);
        assert_eq!(
            call(&app, "PUT", &uri, Some(&token), Some("{}"))
                .await
                .status(),
            StatusCode::OK
        );
        let response = call(&app, "GET", &uri, Some(&token), None).await;
        let value: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(value["dmarc_mitigate_action"], "no_mitigation");
        assert_eq!(value["dmarc_mitigate_unconditionally"], false);
        // Conditional munging: the From domain's published policy decides.
        assert_eq!(
            call(
                &app,
                "PATCH",
                &uri,
                Some(&token),
                Some(r#"{"dmarc_mitigate_action":"munge_from"}"#)
            )
            .await
            .status(),
            StatusCode::OK
        );
        for method in ["PATCH", "PUT"] {
            assert_eq!(
                call_form(
                    &app,
                    method,
                    &uri,
                    &token,
                    "dmarc_mitigate_action=munge_from&dmarc_mitigate_unconditionally=true"
                )
                .await
                .status(),
                StatusCode::OK
            );
        }
        assert_eq!(
            call(&app, "PUT", &uri, Some(&token), Some("{}"))
                .await
                .status(),
            StatusCode::OK
        );
    }
}

#[tokio::test]
async fn message_size_config_roundtrips_and_put_resets_on_both_prefixes() {
    let (app, token, _) = setup(&["admin"]).await;
    create_configurable_list(&app, &token).await;
    for prefix in ["/api/v1", "/3.1"] {
        let uri = format!("{prefix}/lists/dev.example.com/config");
        for (method, payload, expected) in [
            ("PATCH", r#"{"max_message_size":7}"#, 7),
            ("PATCH", r#"{"description":"retains size"}"#, 7),
            ("PUT", r#"{"description":"resets size"}"#, 0),
        ] {
            assert_eq!(
                call(&app, method, &uri, Some(&token), Some(payload))
                    .await
                    .status(),
                StatusCode::OK
            );
            let response = call(&app, "GET", &uri, Some(&token), None).await;
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value["max_message_size"], expected, "{prefix} {method}");
        }
        assert_eq!(
            call_form(
                &app,
                "PATCH",
                &uri,
                &token,
                "max_message_size=2&next_digest_number=9"
            )
            .await
            .status(),
            StatusCode::OK
        );
        for (field, expected) in [("max_message_size", 2), ("next_digest_number", 9)] {
            let response = call(&app, "GET", &format!("{uri}/{field}"), Some(&token), None).await;
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
                expected
            );
        }
        assert_eq!(
            call(
                &app,
                "PATCH",
                &uri,
                Some(&token),
                Some(r#"{"max_message_size":"2"}"#)
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            call_form(&app, "PATCH", &uri, &token, "max_message_size=invalid")
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            call(
                &app,
                "PATCH",
                &uri,
                Some(&token),
                Some(r#"{"max_message_size":-1}"#)
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
    }
}

#[tokio::test]
async fn recipient_limit_config_roundtrips_and_put_resets_on_both_prefixes() {
    let (app, token, _) = setup(&["admin"]).await;
    create_configurable_list(&app, &token).await;
    for prefix in ["/api/v1", "/3.1"] {
        let uri = format!("{prefix}/lists/dev.example.com/config");
        for (method, payload, expected) in [
            ("PATCH", r#"{"max_num_recipients":7}"#, 7),
            ("PATCH", r#"{"description":"retains size"}"#, 7),
            ("PUT", r#"{"description":"resets size"}"#, 0),
        ] {
            assert_eq!(
                call(&app, method, &uri, Some(&token), Some(payload))
                    .await
                    .status(),
                StatusCode::OK
            );
            let response = call(&app, "GET", &uri, Some(&token), None).await;
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value["max_num_recipients"], expected, "{prefix} {method}");
        }
        assert_eq!(
            call_form(
                &app,
                "PATCH",
                &uri,
                &token,
                "max_num_recipients=2&next_digest_number=9"
            )
            .await
            .status(),
            StatusCode::OK
        );
        for (field, expected) in [("max_num_recipients", 2), ("next_digest_number", 9)] {
            let response = call(&app, "GET", &format!("{uri}/{field}"), Some(&token), None).await;
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
                expected
            );
        }
        assert_eq!(
            call(
                &app,
                "PATCH",
                &uri,
                Some(&token),
                Some(r#"{"max_num_recipients":"2"}"#)
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            call_form(&app, "PATCH", &uri, &token, "max_num_recipients=invalid")
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            call(
                &app,
                "PATCH",
                &uri,
                Some(&token),
                Some(r#"{"max_num_recipients":-1}"#)
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
    }
}

#[tokio::test]
async fn acceptance_rules_config_roundtrips_and_password_is_write_only() {
    let (app, token, _) = setup(&["admin"]).await;
    create_configurable_list(&app, &token).await;
    for prefix in ["/api/v1", "/3.1"] {
        let uri = format!("{prefix}/lists/dev.example.com/config");
        let initial = response_json(call(&app, "GET", &uri, Some(&token), None).await).await;
        assert_eq!(initial["administrivia"], true, "{prefix}");
        assert_eq!(initial["require_explicit_destination"], true, "{prefix}");
        assert_eq!(
            initial["acceptable_aliases"],
            serde_json::json!([]),
            "{prefix}"
        );
        assert_eq!(
            initial["hold_these_nonmembers"],
            serde_json::json!([]),
            "{prefix}"
        );
        assert!(initial.get("moderator_password").is_none(), "{prefix}");

        let patch = r#"{
            "administrivia": false,
            "require_explicit_destination": false,
            "acceptable_aliases": ["^.*@example\\.com$"],
            "accept_these_nonmembers": ["friend@example.com"],
            "hold_these_nonmembers": ["^held@"],
            "reject_these_nonmembers": [],
            "discard_these_nonmembers": ["junk@example.com"],
            "moderator_password": "let me in"
        }"#;
        assert_eq!(
            call(&app, "PATCH", &uri, Some(&token), Some(patch))
                .await
                .status(),
            StatusCode::OK,
            "{prefix}"
        );
        let saved = response_json(call(&app, "GET", &uri, Some(&token), None).await).await;
        assert_eq!(saved["administrivia"], false, "{prefix}");
        assert_eq!(saved["require_explicit_destination"], false, "{prefix}");
        assert_eq!(
            saved["acceptable_aliases"],
            serde_json::json!(["^.*@example\\.com$"]),
            "{prefix}"
        );
        assert_eq!(
            saved["accept_these_nonmembers"],
            serde_json::json!(["friend@example.com"])
        );
        assert_eq!(
            saved["hold_these_nonmembers"],
            serde_json::json!(["^held@"])
        );
        assert_eq!(
            saved["discard_these_nonmembers"],
            serde_json::json!(["junk@example.com"])
        );
        assert!(
            saved.get("moderator_password").is_none(),
            "{prefix}: password must never be projected"
        );
        let attr = response_json(
            call(
                &app,
                "GET",
                &format!("{uri}/moderator_password"),
                Some(&token),
                None,
            )
            .await,
        )
        .await;
        assert!(
            attr.is_null() || attr.as_str().is_none_or(|text| !text.contains("let me in")),
            "{prefix}: attribute read leaked the password: {attr}"
        );

        // Form encoding toggles the booleans too.
        assert_eq!(
            call_form(
                &app,
                "PATCH",
                &uri,
                &token,
                "administrivia=true&require_explicit_destination=true"
            )
            .await
            .status(),
            StatusCode::OK,
            "{prefix}"
        );
        let saved = response_json(call(&app, "GET", &uri, Some(&token), None).await).await;
        assert_eq!(saved["administrivia"], true, "{prefix}");
        assert_eq!(saved["require_explicit_destination"], true, "{prefix}");

        assert_posting_pipeline_is_validated_against_the_registry(&app, &token, &uri).await;
        assert_put_resets_acceptance_settings(&app, &token, &uri).await;
        assert_invalid_acceptance_payloads_are_rejected(&app, &token, &uri).await;
    }
}

async fn assert_posting_pipeline_is_validated_against_the_registry(
    app: &axum::Router,
    token: &str,
    uri: &str,
) {
    let initial = response_json(call(app, "GET", uri, Some(token), None).await).await;
    assert_eq!(initial["posting_pipeline"], "default-posting-pipeline");
    // Registered but unable to deliver posts (no roster resolution), declared
    // (not executable), unknown, and non-string names are all refused.
    for invalid in [
        r#"{"posting_pipeline":"virgin"}"#,
        r#"{"posting_pipeline":"default-owner-pipeline"}"#,
        r#"{"posting_pipeline":"no-such-pipeline"}"#,
        r#"{"posting_pipeline":7}"#,
    ] {
        assert_eq!(
            call(app, "PATCH", uri, Some(token), Some(invalid))
                .await
                .status(),
            StatusCode::BAD_REQUEST,
            "{invalid}"
        );
    }
    assert_eq!(
        call(
            app,
            "PATCH",
            uri,
            Some(token),
            Some(r#"{"posting_pipeline":"default-posting-pipeline"}"#)
        )
        .await
        .status(),
        StatusCode::OK
    );
}

async fn assert_put_resets_acceptance_settings(app: &axum::Router, token: &str, uri: &str) {
    assert_eq!(
        call(
            app,
            "PUT",
            uri,
            Some(token),
            Some(r#"{"description":"reset"}"#)
        )
        .await
        .status(),
        StatusCode::OK
    );
    let reset = response_json(call(app, "GET", uri, Some(token), None).await).await;
    assert_eq!(reset["acceptable_aliases"], serde_json::json!([]), "{uri}");
    assert_eq!(
        reset["hold_these_nonmembers"],
        serde_json::json!([]),
        "{uri}"
    );
}

async fn assert_invalid_acceptance_payloads_are_rejected(
    app: &axum::Router,
    token: &str,
    uri: &str,
) {
    for invalid in [
        r#"{"acceptable_aliases":"not-a-list"}"#,
        r#"{"acceptable_aliases":["^("]}"#,
        r#"{"administrivia":"yes"}"#,
        r#"{"moderator_password":7}"#,
    ] {
        assert_eq!(
            call(app, "PATCH", uri, Some(token), Some(invalid))
                .await
                .status(),
            StatusCode::BAD_REQUEST,
            "{uri} {invalid}"
        );
    }
}

async fn assert_mutable_list_config_attributes_roundtrip(app: &axum::Router, token: &str) {
    let mutable = [
        ("display_name", r#""Developers""#),
        ("description", r#""Discussion""#),
        ("info", r#""Long form""#),
        ("subject_prefix", r#""[DEV] ""#),
        ("advertised", "false"),
        ("preferred_language", r#""vi""#),
        ("anonymous_list", "true"),
        ("next_digest_number", "42"),
        ("emergency", "true"),
        ("archive_policy", r#""private""#),
        ("archive_rendering_mode", r#""markdown""#),
    ];
    for (attribute, value) in mutable {
        let uri = format!("/api/v1/lists/dev.example.com/config/{attribute}");
        assert_eq!(
            call(app, "PUT", &uri, Some(token), Some(value))
                .await
                .status(),
            StatusCode::OK,
            "PUT {attribute}"
        );
        let response = call(app, "GET", &uri, Some(token), None).await;
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            serde_json::from_str::<serde_json::Value>(value).unwrap(),
            "GET {attribute}"
        );
    }
}

async fn assert_read_only_list_config_attributes_reject_writes(app: &axum::Router, token: &str) {
    for attribute in [
        "mail_host",
        "list_name",
        "fqdn_listname",
        "list_id",
        "created_at",
        "last_post_at",
        "post_id",
        "volume",
        "digest_last_sent_at",
        "posting_address",
        "bounces_address",
        "join_address",
        "leave_address",
        "owner_address",
        "request_address",
        "no_reply_address",
        "style_name",
    ] {
        let uri = format!("/api/v1/lists/dev.example.com/config/{attribute}");
        assert_eq!(
            call(app, "GET", &uri, Some(token), None).await.status(),
            StatusCode::OK,
            "GET read-only {attribute}"
        );
        assert_eq!(
            call(app, "PUT", &uri, Some(token), Some("null"))
                .await
                .status(),
            StatusCode::BAD_REQUEST,
            "PUT read-only {attribute}"
        );
    }
}

async fn assert_invalid_and_unknown_list_config_attributes_are_rejected(
    app: &axum::Router,
    token: &str,
) {
    for (attribute, value) in [
        ("advertised", r#""yes""#),
        ("archive_policy", r#""sometimes""#),
        ("archive_rendering_mode", r#""html""#),
        ("preferred_language", r#""""#),
        ("next_digest_number", "0"),
        ("emergency", r#""true""#),
    ] {
        let uri = format!("/api/v1/lists/dev.example.com/config/{attribute}");
        assert_eq!(
            call(app, "PUT", &uri, Some(token), Some(value))
                .await
                .status(),
            StatusCode::BAD_REQUEST,
            "invalid {attribute}"
        );
    }
    let unknown = "/api/v1/lists/dev.example.com/config/not_a_setting";
    assert_eq!(
        call(app, "GET", unknown, Some(token), None).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(app, "PUT", unknown, Some(token), Some("true"))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn all_phase_one_list_config_attributes_roundtrip_and_reject_invalid_writes() {
    let (app, token, _) = setup(&["admin"]).await;
    create_configurable_list(&app, &token).await;
    let config = response_json(
        call(
            &app,
            "GET",
            "/api/v1/lists/dev.example.com/config",
            Some(&token),
            None,
        )
        .await,
    )
    .await;
    let expected_identity = serde_json::json!({
        "list_id": "dev.example.com",
        "fqdn_listname": "dev@example.com",
        "list_name": "dev",
        "mail_host": "example.com",
        "posting_address": "dev@example.com",
        "bounces_address": "dev-bounces@example.com",
        "join_address": "dev-join@example.com",
        "leave_address": "dev-leave@example.com",
        "owner_address": "dev-owner@example.com",
        "request_address": "dev-request@example.com",
        "no_reply_address": "noreply@example.com",
        "style_name": "legacy-default",
        "archive_policy": "public",
        "archive_rendering_mode": "text"
    });
    for (field, expected) in expected_identity.as_object().unwrap() {
        assert_eq!(&config[field], expected, "P1 config field {field}");
    }
    assert!(config["created_at"].is_string());
    assert!(config["last_post_at"].is_null());
    assert_eq!(config["post_id"], 1);
    assert_eq!(config["volume"], 1);
    assert_mutable_list_config_attributes_roundtrip(&app, &token).await;
    assert_read_only_list_config_attributes_reject_writes(&app, &token).await;
    assert_invalid_and_unknown_list_config_attributes_are_rejected(&app, &token).await;
}

#[tokio::test]
async fn user_and_member_patch_persist_mutable_fields() {
    let (app, token, user_id) = setup(&["admin"]).await;
    let patched = call(
        &app,
        "PATCH",
        &format!("/api/v1/users/{user_id}"),
        Some(&token),
        Some(r#"{"display_name":"Renamed","locale":"vi","timezone":"Asia/Ho_Chi_Minh"}"#),
    )
    .await;
    assert_eq!(patched.status(), StatusCode::OK);
    let body = to_bytes(patched.into_body(), usize::MAX).await.unwrap();
    let user: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(user["display_name"], "Renamed");
    assert_eq!(user["locale"], "vi");

    call(
        &app,
        "POST",
        "/api/v1/domains",
        Some(&token),
        Some(r#"{"mail_host":"example.com"}"#),
    )
    .await;
    call(
        &app,
        "POST",
        "/api/v1/lists",
        Some(&token),
        Some(r#"{"list_id":"dev.example.com","display_name":"Dev","style":"legacy-default"}"#),
    )
    .await;
    let created = call(&app, "POST", "/api/v1/members", Some(&token), Some(r#"{"list_id":"dev.example.com","subscriber":"member@example.com","pre_verified":true,"pre_confirmed":true,"pre_approved":true}"#)).await;
    let body = to_bytes(created.into_body(), usize::MAX).await.unwrap();
    let member: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let member_id = member["id"].as_str().unwrap();
    let patched = call(
        &app,
        "PATCH",
        &format!("/api/v1/members/{member_id}"),
        Some(&token),
        Some(r#"{"display_name":"Member Name","delivery_mode":"mime_digests","delivery_status":"by_user"}"#),
    )
    .await;
    assert_eq!(patched.status(), StatusCode::OK);
    let body = to_bytes(patched.into_body(), usize::MAX).await.unwrap();
    let member: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(member["display_name"], "Member Name");
    assert_eq!(member["delivery_mode"], "mime_digests");
    assert_eq!(member["delivery_status"], "by_user");
}

#[tokio::test]
async fn insufficient_scope_and_rate_limit_are_enforced() {
    let (app, read_token, _) = setup(&["lists:read"]).await;
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/domains",
            Some(&read_token),
            Some(r#"{"mail_host":"example.com"}"#)
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );

    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let user = db
        .users()
        .create(NewUser {
            display_name: "A".into(),
            email: "a@example.com".into(),
            password: "very secure password".into(),
            server_owner: true,
        })
        .await
        .unwrap();
    let token = db
        .tokens()
        .create(user.id, "limited", &["system:read"], None)
        .await
        .unwrap()
        .token;
    let limited = listmngr_api::router(db, config_with_rate(1));
    assert_eq!(
        call(
            &limited,
            "GET",
            "/api/v1/system/versions",
            Some(&token),
            None
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert_eq!(
        call(
            &limited,
            "GET",
            "/api/v1/system/versions",
            Some(&token),
            None
        )
        .await
        .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn mass_members_rolls_back_all_rows_and_compat_serializer_is_distinct() {
    let (app, token, _) = setup(&["admin"]).await;
    call(
        &app,
        "POST",
        "/api/v1/domains",
        Some(&token),
        Some(r#"{"mail_host":"example.com"}"#),
    )
    .await;
    call(
        &app,
        "POST",
        "/api/v1/lists",
        Some(&token),
        Some(r#"{"list_id":"dev.example.com","display_name":"Dev","style":"legacy-default"}"#),
    )
    .await;
    let response = call(&app,"POST","/api/v1/members/mass",Some(&token),Some(r#"{"operation":"subscribe","members":[{"subscriber":"a@example.com"},{"subscriber":"bad"}],"list_id":"dev.example.com"}"#)).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let roster = call(
        &app,
        "GET",
        "/api/v1/lists/dev.example.com/roster/member",
        Some(&token),
        None,
    )
    .await;
    let body = to_bytes(roster.into_body(), usize::MAX).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["items"].as_array().unwrap().len(), 0);
    let compat = call(&app, "GET", "/3.1/domains", Some(&token), None).await;
    let body = to_bytes(compat.into_body(), usize::MAX).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(value["entries"].is_array());
    let native = call(&app, "GET", "/api/v1/domains", Some(&token), None).await;
    let body = to_bytes(native.into_body(), usize::MAX).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(value["items"].is_array());
    assert!(value.get("entries").is_none());
}

#[tokio::test]
async fn direct_and_effective_preferences_are_distinct_at_every_scope() {
    let (app, token, user_id) = setup(&["admin"]).await;
    call(
        &app,
        "POST",
        "/api/v1/domains",
        Some(&token),
        Some(r#"{"mail_host":"example.com"}"#),
    )
    .await;
    call(
        &app,
        "POST",
        "/api/v1/lists",
        Some(&token),
        Some(r#"{"list_id":"dev.example.com","display_name":"Dev","style":"legacy-default"}"#),
    )
    .await;
    let created = call(&app, "POST", "/api/v1/members", Some(&token), Some(r#"{"list_id":"dev.example.com","subscriber":"member@example.com","pre_verified":true,"pre_confirmed":true,"pre_approved":true}"#)).await;
    let body = to_bytes(created.into_body(), usize::MAX).await.unwrap();
    let member: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let member_id = member["id"].as_str().unwrap();

    call(
        &app,
        "PATCH",
        &format!("/api/v1/users/{user_id}/preferences"),
        Some(&token),
        Some(r#"{"hide_address":true}"#),
    )
    .await;
    call(
        &app,
        "PATCH",
        "/api/v1/addresses/member%40example.com/preferences",
        Some(&token),
        Some(r#"{"preferred_language":"vi"}"#),
    )
    .await;

    let direct = call(
        &app,
        "GET",
        &format!("/api/v1/members/{member_id}/preferences"),
        Some(&token),
        None,
    )
    .await;
    let body = to_bytes(direct.into_body(), usize::MAX).await.unwrap();
    let direct: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(direct["hide_address"].is_null());
    assert!(direct["preferred_language"].is_null());

    let effective = call(
        &app,
        "GET",
        &format!("/api/v1/members/{member_id}/all/preferences"),
        Some(&token),
        None,
    )
    .await;
    let body = to_bytes(effective.into_body(), usize::MAX).await.unwrap();
    let effective: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(effective["preferred_language"], "vi");

    let user_direct = call(
        &app,
        "GET",
        &format!("/api/v1/users/{user_id}/preferences"),
        Some(&token),
        None,
    )
    .await;
    let body = to_bytes(user_direct.into_body(), usize::MAX).await.unwrap();
    let user_direct: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(user_direct["delivery_mode"].is_null());
    let user_effective = call(
        &app,
        "GET",
        &format!("/api/v1/users/{user_id}/all/preferences"),
        Some(&token),
        None,
    )
    .await;
    let body = to_bytes(user_effective.into_body(), usize::MAX)
        .await
        .unwrap();
    let user_effective: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(user_effective["delivery_mode"], "regular");
}

async fn json_body(response: Response) -> serde_json::Value {
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[allow(clippy::too_many_lines)]
async fn assert_preferences_put_replaces_and_patch_merges(
    app: &axum::Router,
    token: &str,
    prefix: &str,
    member_id: listmngr_core::MemberId,
    user_id: UserId,
) {
    for path in [
        format!("{prefix}/members/{member_id}/preferences"),
        format!("{prefix}/users/{user_id}/preferences"),
        format!("{prefix}/addresses/member%40example.com/preferences"),
    ] {
        let initial = serde_json::json!({
            "acknowledge_posts": true,
            "hide_address": false,
            "preferred_language": "initial-language",
            "receive_list_copy": false,
            "receive_own_postings": true,
            "delivery_mode": "mime_digests",
            "delivery_status": "by_bounces"
        });
        assert_eq!(
            call(app, "PUT", &path, Some(token), Some(&initial.to_string()),)
                .await
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            call(
                app,
                "PATCH",
                &path,
                Some(token),
                Some(
                    r#"{"acknowledge_posts":false,"preferred_language":"patched-language","delivery_status":"by_moderator"}"#,
                ),
            )
            .await
            .status(),
            StatusCode::OK
        );
        let merged = json_body(call(app, "GET", &path, Some(token), None).await).await;
        assert_eq!(
            merged,
            serde_json::json!({
                "acknowledge_posts": false,
                "hide_address": false,
                "preferred_language": "patched-language",
                "receive_list_copy": false,
                "receive_own_postings": true,
                "delivery_mode": "mime_digests",
                "delivery_status": "by_moderator"
            }),
            "PATCH must merge differential values at {path}"
        );
        assert_eq!(
            call(
                app,
                "PATCH",
                &path,
                Some(token),
                Some(r#"{"delivery_mode":null}"#),
            )
            .await
            .status(),
            StatusCode::OK
        );
        let cleared = json_body(call(app, "GET", &path, Some(token), None).await).await;
        assert!(cleared["delivery_mode"].is_null());
        assert_eq!(cleared["preferred_language"], "patched-language");

        assert_eq!(
            call(
                app,
                "PUT",
                &path,
                Some(token),
                Some(r#"{"acknowledge_posts":true}"#),
            )
            .await
            .status(),
            StatusCode::OK
        );
        let replaced = json_body(call(app, "GET", &path, Some(token), None).await).await;
        assert_eq!(replaced["acknowledge_posts"], true);
        for nullable in [
            "hide_address",
            "preferred_language",
            "receive_list_copy",
            "receive_own_postings",
            "delivery_mode",
            "delivery_status",
        ] {
            assert!(
                replaced[nullable].is_null(),
                "PUT retained {nullable} at {path}"
            );
        }
        for method in ["PUT", "PATCH"] {
            assert_eq!(
                call(
                    app,
                    method,
                    &path,
                    Some(token),
                    Some(r#"{"id":"read-only"}"#),
                )
                .await
                .status(),
                StatusCode::BAD_REQUEST
            );
        }
    }
}

async fn put_patch_app() -> (
    axum::Router,
    String,
    listmngr_core::MemberId,
    listmngr_core::UserId,
) {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let user = db
        .users()
        .create(NewUser {
            display_name: "Admin".into(),
            email: "admin@example.com".into(),
            password: "Orbit!Cobalt7-River$Quartz".into(),
            server_owner: true,
        })
        .await
        .unwrap();
    db.domains().create("example.com", "", None).await.unwrap();
    let list = db
        .lists()
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
            list_id: list.id,
            email: "member@example.com".into(),
            role: listmngr_core::MemberRole::Member,
            subscription_mode: listmngr_core::SubscriptionMode::AsAddress,
            display_name: "Member".into(),
        })
        .await
        .unwrap();
    let issued = db
        .tokens()
        .create(user.id, "test", &["admin"], None)
        .await
        .unwrap();
    let app = listmngr_api::router(db, config_with_rate(100));
    (app, issued.token, member.id, user.id)
}

#[tokio::test]
async fn put_replaces_and_patch_merges_config_and_preferences_on_both_prefixes() {
    let (app, token, member_id, user_id) = put_patch_app().await;

    for prefix in ["/api/v1", "/3.1"] {
        let list_path = if prefix == "/3.1" {
            format!("{prefix}/lists/dev@example.com/config")
        } else {
            format!("{prefix}/lists/dev.example.com/config")
        };
        assert_eq!(
            call(
                &app,
                "PUT",
                &list_path,
                Some(&token),
                Some(r#"{"description":"keep","advertised":false,"info":"old"}"#),
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            call(
                &app,
                "PATCH",
                &list_path,
                Some(&token),
                Some(r#"{"info":"patched"}"#),
            )
            .await
            .status(),
            StatusCode::OK
        );
        let merged = json_body(call(&app, "GET", &list_path, Some(&token), None).await).await;
        assert_eq!(merged["description"], "keep");
        assert_eq!(merged["advertised"], false);
        assert_eq!(merged["info"], "patched");
        assert_eq!(
            call(
                &app,
                "PUT",
                &list_path,
                Some(&token),
                Some(r#"{"info":"replacement"}"#),
            )
            .await
            .status(),
            StatusCode::OK
        );
        let replaced = json_body(call(&app, "GET", &list_path, Some(&token), None).await).await;
        assert_eq!(replaced["description"], "");
        assert_eq!(replaced["advertised"], true);
        assert_eq!(replaced["preferred_language"], "en");
        for bad in [r#"{"list_id":"other.example.com"}"#, r#"{"unknown":true}"#] {
            assert_eq!(
                call(&app, "PATCH", &list_path, Some(&token), Some(bad))
                    .await
                    .status(),
                StatusCode::BAD_REQUEST
            );
            assert_eq!(
                call(&app, "PUT", &list_path, Some(&token), Some(bad))
                    .await
                    .status(),
                StatusCode::BAD_REQUEST
            );
        }

        assert_preferences_put_replaces_and_patch_merges(&app, &token, prefix, member_id, user_id)
            .await;
    }
}

async fn scoped_app() -> (axum::Router, String, listmngr_core::MemberId) {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let user = db
        .users()
        .create(NewUser {
            display_name: "Admin".into(),
            email: "scope@example.com".into(),
            password: "Orbit!Cobalt7-River$Quartz".into(),
            server_owner: true,
        })
        .await
        .unwrap();
    let first_domain = db
        .domains()
        .create("first.example", "", None)
        .await
        .unwrap();
    db.domains()
        .create("second.example", "", None)
        .await
        .unwrap();
    let first = db
        .lists()
        .create(listmngr_db::NewList {
            list_id: "one.first.example".parse().unwrap(),
            display_name: "One".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let second = db
        .lists()
        .create(listmngr_db::NewList {
            list_id: "two.second.example".parse().unwrap(),
            display_name: "Two".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let outsider = db
        .members()
        .create(listmngr_db::NewMember {
            list_id: second.id.clone(),
            email: "outsider@second.example".into(),
            role: listmngr_core::MemberRole::Member,
            subscription_mode: listmngr_core::SubscriptionMode::AsAddress,
            display_name: "Outsider".into(),
        })
        .await
        .unwrap();
    let token = db
        .tokens()
        .create_scoped(
            user.id,
            "bound",
            &["lists:read", "lists:write", "members:read", "members:write"],
            Some(&first.id),
            Some(first_domain.id),
            None,
        )
        .await
        .unwrap()
        .token;
    let app = listmngr_api::router(db, config_with_rate(100));
    (app, token, outsider.id)
}

#[tokio::test]
async fn scoped_tokens_cannot_cross_route_resource_boundaries() {
    let (app, token, outsider_id) = scoped_app().await;
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/lists/one.first.example",
            Some(&token),
            None
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/lists/two.second.example",
            Some(&token),
            None
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    for (method, uri, body) in [
        ("GET", "/api/v1/domains/second.example", None),
        ("GET", "/api/v1/domains/second.example/lists", None),
        ("GET", "/api/v1/domains/second.example/owners", None),
        ("GET", "/api/v1/domains/second.example/uris", None),
        ("DELETE", "/api/v1/domains/second.example", None),
        ("GET", "/api/v1/lists/two.second.example", None),
        ("GET", "/api/v1/lists/two.second.example/config", None),
        (
            "PATCH",
            "/api/v1/lists/two.second.example/config",
            Some(r#"{"description":"forbidden"}"#),
        ),
        (
            "GET",
            "/api/v1/lists/two.second.example/config/description",
            None,
        ),
        ("GET", "/api/v1/lists/two.second.example/archivers", None),
        ("GET", "/api/v1/lists/two.second.example/uris", None),
        ("GET", "/api/v1/lists/two.second.example/templates", None),
        (
            "GET",
            "/api/v1/lists/two.second.example/roster/member",
            None,
        ),
        (
            "GET",
            "/api/v1/lists/two.second.example/member/outsider%40second.example",
            None,
        ),
        ("DELETE", "/api/v1/lists/two.second.example", None),
    ] {
        assert_eq!(
            call(&app, method, uri, Some(&token), body).await.status(),
            StatusCode::FORBIDDEN,
            "{method} {uri} must enforce the scoped token boundary"
        );
    }
    for (method, suffix, body) in [
        ("GET", "", None),
        ("PATCH", "", Some(r#"{"display_name":"forbidden"}"#)),
        ("DELETE", "", None),
        ("GET", "/preferences", None),
        ("PATCH", "/preferences", Some(r#"{"hide_address":true}"#)),
        ("GET", "/all/preferences", None),
    ] {
        let uri = format!("/api/v1/members/{outsider_id}{suffix}");
        assert_eq!(
            call(&app, method, &uri, Some(&token), body).await.status(),
            StatusCode::FORBIDDEN,
            "{method} {uri} must enforce the member's list boundary"
        );
    }
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/members/mass",
            Some(&token),
            Some(r#"{"operation":"subscribe","members":[{"subscriber":"x@second.example"}],"list_id":"two.second.example"}"#),
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "GET", "/api/v1/owners", Some(&token), None)
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn token_expiry_revoke_and_last_used_are_enforced_across_both_prefixes() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let user = db
        .users()
        .create(NewUser {
            display_name: "Lifecycle".into(),
            email: "lifecycle@example.com".into(),
            password: "Orbit!Cobalt7-River$Quartz".into(),
            server_owner: true,
        })
        .await
        .unwrap();
    let active = db
        .tokens()
        .create(user.id, "active", &["system:read"], None)
        .await
        .unwrap();
    let expired = db
        .tokens()
        .create(
            user.id,
            "expired",
            &["system:read"],
            Some(chrono::Utc::now() - chrono::Duration::seconds(1)),
        )
        .await
        .unwrap();
    let app = listmngr_api::router(db.clone(), config_with_rate(100));

    let before: Option<String> =
        sqlx::query_scalar("SELECT last_used_at FROM api_tokens WHERE id=?")
            .bind(active.id.to_string())
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(before.is_none());
    for prefix in ["/api/v1", "/3.1"] {
        assert_eq!(
            call(
                &app,
                "GET",
                &format!("{prefix}/system/versions"),
                Some(&active.token),
                None,
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            call(
                &app,
                "GET",
                &format!("{prefix}/system/versions"),
                Some(&expired.token),
                None,
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    let active_last_used: Option<String> =
        sqlx::query_scalar("SELECT last_used_at FROM api_tokens WHERE id=?")
            .bind(active.id.to_string())
            .fetch_one(db.pool())
            .await
            .unwrap();
    let expired_last_used: Option<String> =
        sqlx::query_scalar("SELECT last_used_at FROM api_tokens WHERE id=?")
            .bind(expired.id.to_string())
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(
        active_last_used.is_some(),
        "successful auth must update last_used_at"
    );
    assert!(
        expired_last_used.is_none(),
        "expired auth must not update last_used_at"
    );

    db.tokens().revoke(active.id).await.unwrap();
    for prefix in ["/api/v1", "/3.1"] {
        assert_eq!(
            call(
                &app,
                "GET",
                &format!("{prefix}/system/versions"),
                Some(&active.token),
                None,
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
    }
}

struct PostAuthRateFixture {
    db: Database,
    app: axum::Router,
    first: listmngr_db::IssuedToken,
    second: listmngr_db::IssuedToken,
}

async fn post_auth_rate_fixture() -> PostAuthRateFixture {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let first_user = db
        .users()
        .create(NewUser {
            display_name: "First".into(),
            email: "first-rate@example.com".into(),
            password: "Orbit!Cobalt7-River$Quartz".into(),
            server_owner: true,
        })
        .await
        .unwrap();
    let second_user = db
        .users()
        .create(NewUser {
            display_name: "Second".into(),
            email: "second-rate@example.com".into(),
            password: "Orbit!Cobalt7-River$Quartz".into(),
            server_owner: true,
        })
        .await
        .unwrap();
    let first = db
        .tokens()
        .create(first_user.id, "first", &["admin"], None)
        .await
        .unwrap();
    let second = db
        .tokens()
        .create(second_user.id, "second", &["admin"], None)
        .await
        .unwrap();
    let config_path =
        std::env::temp_dir().join(format!("listmngr-rate-{}.toml", uuid::Uuid::now_v7()));
    std::fs::write(
        &config_path,
        "[security.rate_limit]\napi = \"1/min\"\napi_pre_auth = \"100/min\"\n",
    )
    .unwrap();
    let config = Config::load(Some(&config_path)).unwrap();
    std::fs::remove_file(&config_path).unwrap();
    let app = listmngr_api::router(db.clone(), config);
    PostAuthRateFixture {
        db,
        app,
        first,
        second,
    }
}

async fn assert_rate_limited_mutation_has_no_side_effects(fixture: &PostAuthRateFixture) {
    sqlx::query("UPDATE api_tokens SET last_used_at=NULL WHERE id=?")
        .bind(fixture.first.id.to_string())
        .execute(fixture.db.pool())
        .await
        .unwrap();
    let domains_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM domains")
        .fetch_one(fixture.db.pool())
        .await
        .unwrap();
    let audits_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(fixture.db.pool())
        .await
        .unwrap();
    let blocked = call(
        &fixture.app,
        "POST",
        "/api/v1/domains",
        Some(&fixture.first.token),
        Some(r#"{"mail_host":"must-not-exist.example"}"#),
    )
    .await;
    assert_eq!(blocked.status(), StatusCode::TOO_MANY_REQUESTS);
    let retry_after = blocked
        .headers()
        .get(axum::http::header::RETRY_AFTER)
        .unwrap()
        .to_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert!((1..=60).contains(&retry_after));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM domains")
            .fetch_one(fixture.db.pool())
            .await
            .unwrap(),
        domains_before,
        "a blocked handler must not mutate business state"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM audit_log")
            .fetch_one(fixture.db.pool())
            .await
            .unwrap(),
        audits_before,
        "a blocked handler must not emit an audit row"
    );
    assert!(
        sqlx::query_scalar::<_, Option<String>>("SELECT last_used_at FROM api_tokens WHERE id=?")
            .bind(fixture.first.id.to_string())
            .fetch_one(fixture.db.pool())
            .await
            .unwrap()
            .is_none(),
        "post-auth rejection must happen before last_used_at changes"
    );
}

#[tokio::test]
async fn post_auth_rate_limit_is_per_identity_and_blocks_before_mutation() {
    let fixture = post_auth_rate_fixture().await;
    assert_eq!(
        call(
            &fixture.app,
            "GET",
            "/api/v1/system/versions",
            Some(&fixture.first.token),
            None,
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert_rate_limited_mutation_has_no_side_effects(&fixture).await;
    assert_eq!(
        call(
            &fixture.app,
            "GET",
            "/api/v1/system/versions",
            Some(&fixture.second.token),
            None,
        )
        .await
        .status(),
        StatusCode::OK,
        "a second identity on the same socket IP needs an independent post-auth bucket"
    );
}

#[tokio::test]
async fn pre_auth_rate_limit_uses_socket_ip_not_forwarding_headers() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let user = db
        .users()
        .create(NewUser {
            display_name: "Pre auth".into(),
            email: "pre-auth@example.com".into(),
            password: "Orbit!Cobalt7-River$Quartz".into(),
            server_owner: true,
        })
        .await
        .unwrap();
    let issued = db
        .tokens()
        .create(user.id, "pre-auth", &["system:read"], None)
        .await
        .unwrap();
    let mut config = Config::default();
    config.security.rate_limit.api = "100/min".into();
    config.security.rate_limit.api_pre_auth = Some("1/min".into());
    let app = listmngr_api::router(db.clone(), config);
    let socket_peer = "203.0.113.20:4242".parse().unwrap();

    assert_eq!(
        call_authorization_from(
            &app,
            "/api/v1/system/versions",
            "Bearer invalid",
            socket_peer,
            Some("198.51.100.1"),
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    let blocked = call_authorization_from(
        &app,
        "/api/v1/system/versions",
        &format!("Bearer {}", issued.token),
        socket_peer,
        Some("198.51.100.2"),
    )
    .await;
    assert_eq!(blocked.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(
        blocked
            .headers()
            .get(axum::http::header::RETRY_AFTER)
            .unwrap()
            .to_str()
            .unwrap()
            .parse::<u64>()
            .is_ok()
    );
    assert!(
        sqlx::query_scalar::<_, Option<String>>("SELECT last_used_at FROM api_tokens WHERE id=?")
            .bind(issued.id.to_string())
            .fetch_one(db.pool())
            .await
            .unwrap()
            .is_none(),
        "pre-auth rejection must happen before token lookup/usage mutation"
    );
}

#[tokio::test]
async fn invalid_auth_is_rate_limited_before_database_lookup() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let app = listmngr_api::router(db, config_with_rate(1));
    assert_eq!(
        call(&app, "GET", "/api/v1/system/versions", Some("bad"), None)
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, "GET", "/api/v1/system/versions", None, None)
            .await
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
}

struct AuthTestFixture {
    disabled: axum::Router,
    enabled: axum::Router,
    token: String,
    basic: String,
}

async fn auth_test_fixture() -> AuthTestFixture {
    use base64::Engine;

    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let user = db
        .users()
        .create(NewUser {
            display_name: "Auth".into(),
            email: "auth@example.com".into(),
            password: "Orbit!Cobalt7-River$Quartz".into(),
            server_owner: true,
        })
        .await
        .unwrap();
    let issued = db
        .tokens()
        .create(user.id, "auth", &["admin"], None)
        .await
        .unwrap();
    let token_tail = issued.token.strip_prefix("lm_").unwrap();
    let (id, secret) = token_tail.split_once('_').unwrap();
    let basic = base64::engine::general_purpose::STANDARD.encode(format!("{id}:{secret}"));
    let disabled = listmngr_api::router(db.clone(), config_with_rate(100));
    let mut config = Config::default();
    config.api.compat_basic_auth = true;
    config.security.rate_limit.api = "100/min".into();
    let enabled = listmngr_api::router(db, config);

    AuthTestFixture {
        disabled,
        enabled,
        token: issued.token,
        basic,
    }
}

async fn assert_basic_auth_is_disabled_and_malformed_auth_fails_closed(
    disabled: &axum::Router,
    basic: &str,
) {
    assert_eq!(
        call_authorization(disabled, &format!("Basic {basic}"))
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    for malformed in [
        "Bearer",
        "Bearer ",
        "Bearer nonsense",
        "bearer lm_bad_bad",
        "Basic",
        "Basic !!!",
        "Basic bm9jb2xvbg==",
    ] {
        assert_eq!(
            call_authorization(disabled, malformed).await.status(),
            StatusCode::UNAUTHORIZED,
            "{malformed:?} must fail closed"
        );
    }
}

async fn assert_basic_auth_uses_the_socket_peer(enabled: &axum::Router, basic: &str) {
    assert_eq!(
        call_authorization_from(
            enabled,
            "/3.1/system/versions",
            &format!("Basic {basic}"),
            "127.0.0.1:4242".parse().unwrap(),
            None,
        )
        .await
        .status(),
        StatusCode::OK
    );

    assert_eq!(
        call_authorization_from(
            enabled,
            "/api/v1/system/versions",
            &format!("Basic {basic}"),
            "127.0.0.1:4242".parse().unwrap(),
            None,
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED,
        "Basic auth is compatibility-only and must never authenticate /api/v1"
    );

    for uri in ["/3.1/system/versions"] {
        assert_eq!(
            call_authorization_from(
                enabled,
                uri,
                &format!("Basic {basic}"),
                "203.0.113.9:4242".parse().unwrap(),
                Some("127.0.0.1"),
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED,
            "X-Forwarded-For must not spoof an allowlisted socket peer for {uri}"
        );
        assert_eq!(
            call_authorization_from(
                enabled,
                uri,
                &format!("Basic {basic}"),
                "127.0.0.1:4242".parse().unwrap(),
                Some("203.0.113.9"),
            )
            .await
            .status(),
            StatusCode::OK,
            "the actual allowlisted socket peer must control Basic auth for {uri}"
        );
    }
}

async fn assert_conflict_errors_are_correlated_and_redacted(enabled: &axum::Router, token: &str) {
    assert_eq!(
        call(
            enabled,
            "POST",
            "/api/v1/domains",
            Some(token),
            Some(r#"{"mail_host":"duplicate.example"}"#),
        )
        .await
        .status(),
        StatusCode::CREATED
    );
    let duplicate = call(
        enabled,
        "POST",
        "/api/v1/domains",
        Some(token),
        Some(r#"{"mail_host":"duplicate.example"}"#),
    )
    .await;
    assert_eq!(duplicate.status(), StatusCode::CONFLICT);
    let body = to_bytes(duplicate.into_body(), usize::MAX).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["code"], "conflict");
    assert!(uuid::Uuid::parse_str(body["correlation_id"].as_str().unwrap()).is_ok());
    let rendered = body.to_string().to_ascii_lowercase();
    for leaked in [
        "sql",
        "unique constraint",
        "postgres://",
        "sqlite:",
        "password",
    ] {
        assert!(
            !rendered.contains(leaked),
            "error leaked {leaked}: {rendered}"
        );
    }
}

#[tokio::test]
async fn basic_and_bearer_fail_closed_and_errors_are_correlated_and_redacted() {
    let fixture = auth_test_fixture().await;
    assert_basic_auth_is_disabled_and_malformed_auth_fails_closed(
        &fixture.disabled,
        &fixture.basic,
    )
    .await;
    assert_basic_auth_uses_the_socket_peer(&fixture.enabled, &fixture.basic).await;
    assert_conflict_errors_are_correlated_and_redacted(&fixture.enabled, &fixture.token).await;
}

#[tokio::test]
async fn http_mutations_record_exact_request_actor_token_and_socket_ip_once() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let actor = db
        .users()
        .create(NewUser {
            display_name: "Audit actor".into(),
            email: "audit-actor@example.com".into(),
            password: "Orbit!Cobalt7-River$Quartz".into(),
            server_owner: true,
        })
        .await
        .unwrap();
    let issued = db
        .tokens()
        .create(actor.id, "audit", &["admin"], None)
        .await
        .unwrap();
    let app = listmngr_api::router(db.clone(), config_with_rate(100));
    let peer: std::net::SocketAddr = "203.0.113.42:4242".parse().unwrap();
    let mut before = db.audit().list().await.unwrap().len();

    for (prefix, email) in [
        ("/api/v1", "native-audit@example.com"),
        ("/3.1", "compat-audit@example.com"),
    ] {
        let response = call_from(
            &app,
            "POST",
            &format!("{prefix}/users"),
            Some(&issued.token),
            Some(&format!(
                r#"{{"display_name":"Audited","email":"{email}","password":"DO-NOT-LOG-Password!9","server_owner":false}}"#
            )),
            peer,
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED, "{prefix}");
        let entries = db.audit().list().await.unwrap();
        assert_eq!(entries.len(), before + 1, "{prefix} audit cardinality");
        let entry = entries.last().unwrap();
        assert_eq!(entry.actor_user_id, Some(actor.id));
        assert_eq!(entry.actor_token_id, Some(issued.id));
        assert_eq!(entry.ip, Some(peer.ip()));
        assert_eq!(entry.action, "user.create");
        assert_eq!(entry.diff["email"], email);
        assert!(!entry.diff.to_string().contains("DO-NOT-LOG"));
        before = entries.len();
    }
}

#[derive(Clone)]
struct ScopedFixture {
    db: Database,
    app: axum::Router,
    list_token: listmngr_db::IssuedToken,
    domain_token: listmngr_db::IssuedToken,
    first: listmngr_core::MailingList,
    sibling: listmngr_core::MailingList,
    second: listmngr_core::MailingList,
    second_domain: listmngr_core::Domain,
    visible_user: listmngr_core::User,
    secret_user: listmngr_core::User,
    hidden_address_id: listmngr_core::AddressId,
}

async fn create_scoped_user(db: &Database, display_name: &str, email: &str) -> listmngr_core::User {
    db.users()
        .create(NewUser {
            display_name: display_name.into(),
            email: email.into(),
            password: "Orbit!Cobalt7-River$Quartz".into(),
            server_owner: false,
        })
        .await
        .unwrap()
}

async fn create_scoped_list(
    db: &Database,
    list_id: &str,
    display_name: &str,
) -> listmngr_core::MailingList {
    db.lists()
        .create(listmngr_db::NewList {
            list_id: list_id.parse().unwrap(),
            display_name: display_name.into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap()
}

async fn subscribe_scoped_user(db: &Database, list_id: listmngr_core::ListId, email: &str) {
    db.members()
        .subscribe_with_context(
            listmngr_db::NewMember {
                list_id,
                email: email.into(),
                role: listmngr_core::MemberRole::Member,
                subscription_mode: listmngr_core::SubscriptionMode::AsAddress,
                display_name: email.into(),
            },
            true,
            &listmngr_db::AuditContext::system(),
        )
        .await
        .unwrap();
}

async fn scoped_fixture() -> ScopedFixture {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let mut actor = create_scoped_user(&db, "Scoped actor", "actor@first.example").await;
    sqlx::query("UPDATE users SET is_server_owner=1 WHERE id=?")
        .bind(actor.id.to_string())
        .execute(db.pool())
        .await
        .unwrap();
    actor.is_server_owner = true;
    let first_domain = db
        .domains()
        .create("first.example", "", None)
        .await
        .unwrap();
    let second_domain = db
        .domains()
        .create("second.example", "", None)
        .await
        .unwrap();
    let first = create_scoped_list(&db, "one.first.example", "Visible").await;
    let sibling = create_scoped_list(&db, "sibling.first.example", "SECRET-SIBLING-LIST").await;
    let second = create_scoped_list(&db, "two.second.example", "SECRET-SECOND-LIST").await;
    let visible_user = create_scoped_user(&db, "Visible user", "visible@first.example").await;
    let secret_user = create_scoped_user(&db, "SECRET-SECOND-USER", "secret@second.example").await;
    subscribe_scoped_user(&db, first.id.clone(), "visible@first.example").await;
    subscribe_scoped_user(&db, sibling.id.clone(), "hidden@first.example").await;
    subscribe_scoped_user(&db, second.id.clone(), "secret@second.example").await;
    let hidden_address = db.addresses().get("hidden@first.example").await.unwrap();
    db.addresses()
        .link("hidden@first.example", Some(visible_user.id))
        .await
        .unwrap();
    sqlx::query("UPDATE users SET preferred_address_id=? WHERE id=?")
        .bind(hidden_address.id.to_string())
        .bind(visible_user.id.to_string())
        .execute(db.pool())
        .await
        .unwrap();
    let scopes = [
        "system:read",
        "lists:read",
        "lists:write",
        "members:read",
        "members:write",
        "users:write",
    ];
    let list_token = db
        .tokens()
        .create_scoped(actor.id, "list", &scopes, Some(&first.id), None, None)
        .await
        .unwrap();
    let domain_token = db
        .tokens()
        .create_scoped(
            actor.id,
            "domain",
            &scopes,
            None,
            Some(first_domain.id),
            None,
        )
        .await
        .unwrap();
    let app = listmngr_api::router(db.clone(), config_with_rate(1000));
    ScopedFixture {
        db,
        app,
        list_token,
        domain_token,
        first,
        sibling,
        second,
        second_domain,
        visible_user,
        secret_user,
        hidden_address_id: hidden_address.id,
    }
}

async fn assert_scoped_collections(fixture: &ScopedFixture) {
    for prefix in ["/api/v1", "/3.1"] {
        for token in [&fixture.list_token.token, &fixture.domain_token.token] {
            for (method, path, body) in [
                ("GET", "/domains", None),
                ("GET", "/lists", None),
                ("GET", "/domains/first.example/lists", None),
                (
                    "POST",
                    "/members/find",
                    Some(r#"{"subscriber":"secret@second.example"}"#),
                ),
                ("GET", "/users", None),
            ] {
                let response = call(
                    &fixture.app,
                    method,
                    &format!("{prefix}{path}"),
                    Some(token),
                    body,
                )
                .await;
                assert_eq!(response.status(), StatusCode::OK, "{method} {prefix}{path}");
                let rendered = response_json(response).await.to_string();
                for forbidden in [
                    fixture.second.id.as_str(),
                    &fixture.second_domain.mail_host,
                    "secret@second.example",
                    "SECRET-SECOND-LIST",
                ] {
                    assert!(
                        !rendered.contains(forbidden),
                        "{method} {prefix}{path} leaked {forbidden}: {rendered}"
                    );
                }
                if token == &fixture.list_token.token {
                    for sibling_secret in [fixture.sibling.id.as_str(), "SECRET-SIBLING-LIST"] {
                        assert!(
                            !rendered.contains(sibling_secret),
                            "{method} {prefix}{path} leaked {sibling_secret}: {rendered}"
                        );
                    }
                }
            }
        }
    }
}

async fn assert_nested_user_addresses_are_scoped(fixture: &ScopedFixture) {
    for prefix in ["/api/v1", "/3.1"] {
        let path = format!("{prefix}/users/{}/addresses", fixture.visible_user.id);
        let list_response = call(
            &fixture.app,
            "GET",
            &path,
            Some(&fixture.list_token.token),
            None,
        )
        .await;
        assert_eq!(list_response.status(), StatusCode::OK);
        assert!(
            !response_json(list_response)
                .await
                .to_string()
                .contains(&fixture.hidden_address_id.to_string()),
            "list-scoped nested user addresses leaked a sibling-list address"
        );

        let domain_response = call(
            &fixture.app,
            "GET",
            &path,
            Some(&fixture.domain_token.token),
            None,
        )
        .await;
        assert_eq!(domain_response.status(), StatusCode::OK);
        assert!(
            response_json(domain_response)
                .await
                .to_string()
                .contains(&fixture.hidden_address_id.to_string()),
            "domain-scoped nested user addresses must retain in-domain addresses"
        );
    }
}

fn cross_resource_routes(
    secret_user: listmngr_core::UserId,
) -> Vec<(&'static str, String, Option<String>)> {
    vec![
        ("GET", format!("/users/{secret_user}"), None),
        ("PATCH", format!("/users/{secret_user}"), Some(r#"{"display_name":"forbidden"}"#.into())),
        ("DELETE", format!("/users/{secret_user}"), None),
        ("GET", format!("/users/{secret_user}/preferences"), None),
        ("PUT", format!("/users/{secret_user}/preferences"), Some("{}".into())),
        ("PATCH", format!("/users/{secret_user}/preferences"), Some("{}".into())),
        ("GET", format!("/users/{secret_user}/all/preferences"), None),
        ("POST", format!("/users/{secret_user}/login"), Some(r#"{"password":"Orbit!Cobalt7-River$Quartz"}"#.into())),
        ("GET", format!("/users/{secret_user}/addresses"), None),
        ("POST", format!("/users/{secret_user}/addresses"), Some(r#"{"email":"secret@second.example"}"#.into())),
        ("GET", "/addresses/secret%40second.example".into(), None),
        ("POST", "/addresses/secret%40second.example/verify".into(), None),
        ("POST", "/addresses/secret%40second.example/unverify".into(), None),
        ("GET", "/addresses/secret%40second.example/user".into(), None),
        ("POST", "/addresses/secret%40second.example/user".into(), Some(format!(r#"{{"user_id":"{secret_user}"}}"#))),
        ("DELETE", "/addresses/secret%40second.example/user".into(), None),
        ("GET", "/addresses/secret%40second.example/memberships".into(), None),
        ("GET", "/addresses/secret%40second.example/preferences".into(), None),
        ("PUT", "/addresses/secret%40second.example/preferences".into(), Some("{}".into())),
        ("PATCH", "/addresses/secret%40second.example/preferences".into(), Some("{}".into())),
        ("GET", "/addresses/secret%40second.example/all/preferences".into(), None),
        ("GET", "/owners".into(), None),
        ("POST", "/users".into(), Some(r#"{"display_name":"forbidden","email":"forbidden@second.example","password":"Orbit!Cobalt7-River$Quartz","server_owner":false}"#.into())),
    ]
}

async fn reset_last_used(db: &Database, token: listmngr_core::TokenId) {
    sqlx::query("UPDATE api_tokens SET last_used_at=NULL WHERE id=?")
        .bind(token.to_string())
        .execute(db.pool())
        .await
        .unwrap();
}

async fn assert_last_used_is_unchanged(db: &Database, token: listmngr_core::TokenId, label: &str) {
    let last_used: Option<String> =
        sqlx::query_scalar("SELECT last_used_at FROM api_tokens WHERE id=?")
            .bind(token.to_string())
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(
        last_used.is_none(),
        "forbidden {label} updated last_used_at"
    );
}

async fn assert_cross_resource_requests(fixture: &ScopedFixture) {
    for prefix in ["/api/v1", "/3.1"] {
        for issued in [&fixture.list_token, &fixture.domain_token] {
            for (method, path, body) in cross_resource_routes(fixture.secret_user.id) {
                reset_last_used(&fixture.db, issued.id).await;
                let response = call(
                    &fixture.app,
                    method,
                    &format!("{prefix}{path}"),
                    Some(&issued.token),
                    body.as_deref(),
                )
                .await;
                assert_eq!(
                    response.status(),
                    StatusCode::FORBIDDEN,
                    "{method} {prefix}{path}"
                );
                assert_last_used_is_unchanged(
                    &fixture.db,
                    issued.id,
                    &format!("{method} {prefix}{path}"),
                )
                .await;
            }
            for (path, body) in [
                ("/domains", r#"{"mail_host":"forbidden.example"}"#),
                (
                    "/lists",
                    r#"{"list_id":"forbidden.second.example","display_name":"Forbidden"}"#,
                ),
                (
                    "/members",
                    r#"{"list_id":"two.second.example","subscriber":"new@second.example","pre_verified":true,"pre_confirmed":true,"pre_approved":true}"#,
                ),
                (
                    "/members/mass",
                    r#"{"operation":"subscribe","list_id":"two.second.example","members":[{"subscriber":"new@second.example"}]}"#,
                ),
            ] {
                reset_last_used(&fixture.db, issued.id).await;
                let response = call(
                    &fixture.app,
                    "POST",
                    &format!("{prefix}{path}"),
                    Some(&issued.token),
                    Some(body),
                )
                .await;
                assert_eq!(
                    response.status(),
                    StatusCode::FORBIDDEN,
                    "POST {prefix}{path}"
                );
                assert_last_used_is_unchanged(
                    &fixture.db,
                    issued.id,
                    &format!("POST {prefix}{path}"),
                )
                .await;
            }
        }
    }
}

async fn business_row_counts(db: &Database) -> (i64, i64, i64) {
    (
        sqlx::query_scalar("SELECT COUNT(*) FROM domains")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        sqlx::query_scalar("SELECT COUNT(*) FROM mailing_lists")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        sqlx::query_scalar("SELECT COUNT(*) FROM members")
            .fetch_one(db.pool())
            .await
            .unwrap(),
    )
}

#[tokio::test]
async fn scoped_tokens_filter_global_surfaces_forbid_cross_writes_and_preserve_last_used() {
    let fixture = scoped_fixture().await;
    assert_scoped_collections(&fixture).await;
    assert_nested_user_addresses_are_scoped(&fixture).await;
    let writes_before = business_row_counts(&fixture.db).await;
    assert_cross_resource_requests(&fixture).await;
    assert_eq!(
        business_row_counts(&fixture.db).await,
        writes_before,
        "forbidden matrix must not write"
    );
    assert!(
        fixture
            .db
            .users()
            .get(fixture.visible_user.id)
            .await
            .is_ok()
    );
    assert_eq!(
        fixture
            .db
            .users()
            .get(fixture.secret_user.id)
            .await
            .unwrap()
            .display_name,
        "SECRET-SECOND-USER"
    );
    assert_eq!(fixture.first.id.as_str(), "one.first.example");
}

/// Pipelines are projected from the live handler registry with their real
/// handler order; the owner pipeline is declared until its handlers exist.
async fn assert_phase_one_catalogs(app: &axum::Router, token: &str) {
    let body =
        response_json(call(app, "GET", "/api/v1/system/pipelines", Some(token), None).await).await;
    let entries = body["items"].as_array().unwrap();
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "default-posting-pipeline",
            "virgin",
            "default-owner-pipeline",
        ]
    );
    let posting = &entries[0];
    assert_eq!(posting["executable"], true);
    assert_eq!(posting["status"], "engine");
    assert_eq!(
        posting["handlers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|handler| handler.as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "validate-authenticity",
            "mime-delete",
            "tagger",
            "member-recipients",
            "cleanse",
            "cleanse-dkim",
            "cook-headers",
            "subject-prefix",
            "rfc-2369",
            "to-archive",
            "to-digest",
            "after-delivery",
            "acknowledge",
            "dmarc",
            "to-outgoing",
        ]
    );
    assert_eq!(entries[1]["executable"], true, "virgin");
    assert_eq!(
        entries[2]["executable"], false,
        "owner pipeline is declared"
    );
    assert_eq!(entries[2]["status"], "declared");
}

/// Chains are projected from the live engine registry, so this asserts the real
/// link order rather than a hand-kept name list.
async fn assert_chain_projection_matches_the_engine(app: &axum::Router, token: &str) {
    let body =
        response_json(call(app, "GET", "/api/v1/system/chains", Some(token), None).await).await;
    let entries = body["items"].as_array().unwrap();
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "default-posting-chain",
            "default-owner-chain",
            "accept",
            "hold",
            "reject",
            "discard",
            "moderation",
            "header-match",
            "dmarc-mitigation",
        ]
    );
    let by_name = |name: &str| {
        entries
            .iter()
            .find(|entry| entry["name"] == name)
            .unwrap_or_else(|| panic!("chain {name} missing"))
    };

    let posting = by_name("default-posting-chain");
    assert_eq!(posting["executable"], true);
    assert_eq!(posting["status"], "engine");
    assert_eq!(
        posting["rules"]
            .as_array()
            .unwrap()
            .iter()
            .map(|rule| rule.as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "dmarc-mitigation",
            "no-senders",
            "approved",
            "emergency",
            "loop",
            "banned-address",
            "member-moderation",
            "nonmember-moderation",
            "administrivia",
            "implicit-dest",
            "max-recipients",
            "max-size",
            "no-subject",
            "suspicious-header",
            "any",
            "truth",
            "truth",
        ]
    );

    for name in ["accept", "hold", "reject", "discard"] {
        assert_eq!(by_name(name)["status"], "terminal", "{name}");
        assert_eq!(by_name(name)["executable"], true, "{name}");
    }
    assert_eq!(by_name("moderation")["status"], "moderation");
    assert_eq!(by_name("header-match")["status"], "header-match");
    assert_eq!(by_name("header-match")["executable"], true);

    // The DMARC chain is a terminal decided by the list's action, so it
    // carries no links of its own.
    let dmarc = by_name("dmarc-mitigation");
    assert_eq!(dmarc["status"], "dmarc-mitigation");
    assert_eq!(dmarc["executable"], true);
    assert!(dmarc["rules"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn anti_stub_catalogs_collections_and_resource_projections_are_behavioral() {
    let (app, token, user_id) = setup(&["admin"]).await;
    assert_phase_one_catalogs(&app, &token).await;
    assert_chain_projection_matches_the_engine(&app, &token).await;

    let addresses = response_json(
        call(
            &app,
            "GET",
            &format!("/api/v1/users/{user_id}/addresses"),
            Some(&token),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(addresses["items"][0]["email"], "admin@example.com");
    assert!(addresses["items"][0]["id"].is_string());

    for (path, body) in [
        ("/api/v1/domains", r#"{"mail_host":"projection.example"}"#),
        (
            "/api/v1/lists",
            r#"{"list_id":"dev.projection.example","display_name":"Dev"}"#,
        ),
    ] {
        assert_eq!(
            call(&app, "POST", path, Some(&token), Some(body))
                .await
                .status(),
            StatusCode::CREATED
        );
    }
    let created = response_json(call(
        &app,
        "POST",
        "/api/v1/members",
        Some(&token),
        Some(r#"{"list_id":"dev.projection.example","subscriber":"projection@example.net","pre_verified":true,"pre_confirmed":true,"pre_approved":true}"#),
    ).await).await;
    let member_id = created["id"].as_str().unwrap();

    let native = response_json(
        call(
            &app,
            "GET",
            "/api/v1/lists/dev.projection.example/member/projection%40example.net",
            Some(&token),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(native["id"], member_id);
    assert!(native.get("email").is_none());
    assert!(native.get("self_link").is_none());

    let compat = response_json(
        call(
            &app,
            "GET",
            "/3.1/lists/dev.projection.example/member/projection%40example.net",
            Some(&token),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(compat["member_id"], member_id);
    assert_eq!(compat["email"], "projection@example.net");
    assert_eq!(compat["address"], "/3.1/addresses/projection@example.net");
    assert_eq!(compat["self_link"], format!("/3.1/members/{member_id}"));
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn collection_and_roster_pagination_has_deterministic_boundaries() {
    let (app, token, _) = setup(&["admin"]).await;
    for host in ["a.page.example", "b.page.example", "c.page.example"] {
        assert_eq!(
            call(
                &app,
                "POST",
                "/api/v1/domains",
                Some(&token),
                Some(&format!(r#"{{"mail_host":"{host}"}}"#)),
            )
            .await
            .status(),
            StatusCode::CREATED
        );
    }
    let first =
        response_json(call(&app, "GET", "/api/v1/domains?count=1", Some(&token), None).await).await;
    assert_eq!(first["total"], 3);
    assert_eq!(first["start"], 0);
    assert_eq!(first["count"], 1);
    assert_eq!(first["items"][0]["mail_host"], "a.page.example");
    assert_eq!(first["next_cursor"], "1");
    let last = response_json(
        call(
            &app,
            "GET",
            "/api/v1/domains?cursor=2&count=1",
            Some(&token),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(last["items"][0]["mail_host"], "c.page.example");
    assert!(last["next_cursor"].is_null());
    let beyond = response_json(
        call(
            &app,
            "GET",
            "/api/v1/domains?cursor=99&count=1",
            Some(&token),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(beyond["total"], 3);
    assert_eq!(beyond["start"], 3);
    assert_eq!(beyond["count"], 0);
    assert!(beyond["items"].as_array().unwrap().is_empty());

    let compat = response_json(
        call(
            &app,
            "GET",
            "/3.1/domains?page=2&count=1",
            Some(&token),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(compat["total_size"], 3);
    assert_eq!(compat["start"], 1);
    assert_eq!(compat["count"], 1);
    assert_eq!(compat["entries"][0]["mail_host"], "b.page.example");
    assert_eq!(
        compat["entries"][0]["self_link"],
        "/3.1/domains/b.page.example"
    );

    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/lists",
            Some(&token),
            Some(r#"{"list_id":"roster.a.page.example","display_name":"Roster"}"#),
        )
        .await
        .status(),
        StatusCode::CREATED
    );
    let lists = response_json(
        call(
            &app,
            "GET",
            "/3.1/domains/a.page.example/lists?count=1",
            Some(&token),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(
        lists["entries"][0]["fqdn_listname"],
        "roster@a.page.example"
    );
    assert_eq!(
        lists["entries"][0]["self_link"],
        "/3.1/lists/roster.a.page.example"
    );
    for subscriber in ["z@example.net", "a@example.net", "m@example.net"] {
        assert_eq!(call(
            &app, "POST", "/api/v1/members", Some(&token),
            Some(&format!(r#"{{"list_id":"roster.a.page.example","subscriber":"{subscriber}","pre_verified":true,"pre_confirmed":true,"pre_approved":true}}"#)),
        ).await.status(), StatusCode::CREATED);
    }
    let roster = response_json(
        call(
            &app,
            "GET",
            "/3.1/lists/roster.a.page.example/roster/member?page=2&count=1",
            Some(&token),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(roster["total_size"], 3);
    assert_eq!(roster["start"], 1);
    assert_eq!(roster["count"], 1);
    assert_eq!(roster["entries"][0]["email"], "m@example.net");
}

#[tokio::test]
async fn member_find_supports_exact_and_substring_with_scope_and_filters() {
    let fixture = scoped_fixture().await;
    let exact = response_json(
        call(
            &fixture.app,
            "POST",
            "/api/v1/members/find",
            Some(&fixture.domain_token.token),
            Some(r#"{"subscriber":"visible@first.example"}"#),
        )
        .await,
    )
    .await;
    assert_eq!(exact["total"], 1);

    let substring = response_json(
        call(
            &fixture.app,
            "POST",
            "/api/v1/members/find?count=10",
            Some(&fixture.domain_token.token),
            Some(r#"{"substring":"first.example","role":"member"}"#),
        )
        .await,
    )
    .await;
    assert_eq!(substring["total"], 2);
    let rendered = substring.to_string();
    assert!(rendered.contains("one.first.example"));
    assert!(rendered.contains("sibling.first.example"));
    assert!(!rendered.contains("two.second.example"));

    let list_filtered = response_json(
        call(
            &fixture.app,
            "POST",
            "/api/v1/members/find",
            Some(&fixture.domain_token.token),
            Some(r#"{"substring":"first.example","list_id":"one.first.example"}"#),
        )
        .await,
    )
    .await;
    assert_eq!(list_filtered["total"], 1);

    let list_scoped = response_json(
        call(
            &fixture.app,
            "POST",
            "/api/v1/members/find",
            Some(&fixture.list_token.token),
            Some(r#"{"substring":"example"}"#),
        )
        .await,
    )
    .await;
    assert_eq!(list_scoped["total"], 1);
    assert!(!list_scoped.to_string().contains("sibling.first.example"));

    let compat_form = response_json(
        call_form(
            &fixture.app,
            "POST",
            "/3.1/members/find",
            &fixture.domain_token.token,
            "subscriber=visible%40first.example",
        )
        .await,
    )
    .await;
    assert_eq!(compat_form["total_size"], 1);
}

#[tokio::test]
async fn bounded_shared_identity_writes_require_unbound_tokens() {
    let f = scoped_fixture().await;
    subscribe_scoped_user(&f.db, f.second.id.clone(), "visible@first.example").await;
    let member =
        f.db.members()
            .find("visible@first.example")
            .await
            .unwrap()
            .into_iter()
            .find(|m| m.list_id == f.first.id)
            .unwrap();
    let global =
        f.db.tokens()
            .create(f.visible_user.id, "global", &["users:write"], None)
            .await
            .unwrap();
    for prefix in ["/api/v1", "/3.1"] {
        for token in [&f.list_token.token, &f.domain_token.token] {
            for path in [
                format!("/users/{}", f.visible_user.id),
                "/addresses/visible@first.example".into(),
            ] {
                assert_eq!(
                    call(&f.app, "GET", &format!("{prefix}{path}"), Some(token), None)
                        .await
                        .status(),
                    StatusCode::OK
                );
            }
            for (method, path, body) in [
                (
                    "PATCH",
                    format!("/users/{}", f.visible_user.id),
                    Some(r#"{"display_name":"changed"}"#),
                ),
                (
                    "PATCH",
                    format!("/users/{}/preferences", f.visible_user.id),
                    Some(r#"{"preferred_language":"vi"}"#),
                ),
                (
                    "PATCH",
                    "/addresses/visible@first.example/preferences".into(),
                    Some(r#"{"preferred_language":"vi"}"#),
                ),
                (
                    "DELETE",
                    "/addresses/visible@first.example/user".into(),
                    None,
                ),
                (
                    "POST",
                    "/addresses/visible@first.example/user".into(),
                    Some(format!(r#"{{"user_id":"{}"}}"#, f.visible_user.id)).as_deref(),
                ),
            ] {
                assert_eq!(
                    call(
                        &f.app,
                        method,
                        &format!("{prefix}{path}"),
                        Some(token),
                        body
                    )
                    .await
                    .status(),
                    StatusCode::FORBIDDEN,
                    "{method} {path}"
                );
            }
            assert!(
                call(
                    &f.app,
                    "PATCH",
                    &format!("{prefix}/members/{}/preferences", member.id),
                    Some(token),
                    Some(r#"{"preferred_language":"vi"}"#)
                )
                .await
                .status()
                .is_success()
            );
        }
        assert!(
            call(
                &f.app,
                "PATCH",
                &format!("{prefix}/users/{}/preferences", f.visible_user.id),
                Some(&global.token),
                Some(r#"{"preferred_language":"vi"}"#)
            )
            .await
            .status()
            .is_success()
        );
    }
}

#[tokio::test]
async fn legacy_bounded_admin_cannot_mutate_global_identity() {
    let f = scoped_fixture().await;
    sqlx::query("UPDATE api_tokens SET scopes='admin' WHERE id=$1")
        .bind(f.list_token.id.to_string())
        .execute(f.db.pool())
        .await
        .unwrap();
    assert_eq!(
        call(
            &f.app,
            "PATCH",
            &format!("/api/v1/users/{}", f.visible_user.id),
            Some(&f.list_token.token),
            Some(r#"{"display_name":"changed"}"#)
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&f.app, "POST", "/api/v1/users", Some(&f.list_token.token),
            Some(r#"{"display_name":"New","email":"new@example.com","password":"Orbit!Cobalt7-River$Quartz","server_owner":false}"#))
            .await.status(),
        StatusCode::FORBIDDEN
    );
    let admin =
        f.db.tokens()
            .create(f.visible_user.id, "admin", &["admin"], None)
            .await
            .unwrap();
    assert_eq!(
        call(
            &f.app,
            "PATCH",
            &format!("/api/v1/users/{}", f.visible_user.id),
            Some(&admin.token),
            Some(r#"{"display_name":"changed"}"#)
        )
        .await
        .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn historical_member_user_id_does_not_grant_scoped_identity_access() {
    let f = scoped_fixture().await;
    // Simulate historical drift left by older versions, not current ownership.
    sqlx::query("UPDATE members SET user_id=$1 WHERE list_id=$2")
        .bind(f.secret_user.id.to_string())
        .bind(f.first.id.to_string())
        .execute(f.db.pool())
        .await
        .unwrap();
    assert_eq!(
        call(
            &f.app,
            "GET",
            &format!("/api/v1/users/{}", f.secret_user.id),
            Some(&f.list_token.token),
            None
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
}
