use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
    response::Response,
};
use listmngr_core::{Config, UserId};
use listmngr_db::{Database, NewUser};
use tower::ServiceExt;

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
    let app = listmngr_api::router(db, Config::default(), 100);
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

#[test]
fn openapi_source_rejects_dummy_doc_functions() {
    let source = include_str!("../src/lib.rs");
    assert!(
        !source.contains("fn doc_"),
        "OpenAPI metadata must be attached to real axum handlers, never empty doc_* stubs"
    );
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
    let limited = listmngr_api::router(db, Config::default(), 1);
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
        assert_eq!(
            call(
                app,
                "PUT",
                &path,
                Some(token),
                Some(r#"{"hide_address":true,"delivery_mode":"mime_digests"}"#),
            )
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
                Some(r#"{"acknowledge_posts":true}"#),
            )
            .await
            .status(),
            StatusCode::OK
        );
        let merged = json_body(call(app, "GET", &path, Some(token), None).await).await;
        assert_eq!(merged["hide_address"], true);
        assert_eq!(merged["delivery_mode"], "mime_digests");
        assert_eq!(merged["acknowledge_posts"], true);
        assert_eq!(
            call(
                app,
                "PUT",
                &path,
                Some(token),
                Some(r#"{"acknowledge_posts":false}"#),
            )
            .await
            .status(),
            StatusCode::OK
        );
        let replaced = json_body(call(app, "GET", &path, Some(token), None).await).await;
        assert_eq!(replaced["acknowledge_posts"], false);
        assert!(replaced["hide_address"].is_null());
        assert!(replaced["delivery_mode"].is_null());
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
    let app = listmngr_api::router(db, Config::default(), 100);
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
    let app = listmngr_api::router(db, Config::default(), 100);
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
    let app = listmngr_api::router(db.clone(), Config::default(), 100);

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

#[tokio::test]
async fn invalid_auth_is_rate_limited_before_database_lookup() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let app = listmngr_api::router(db, Config::default(), 1);
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
    let disabled = listmngr_api::router(db.clone(), Config::default(), 100);
    let mut config = Config::default();
    config.api.compat_basic_auth = true;
    let enabled = listmngr_api::router(db, config, 100);

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
        call_authorization(enabled, &format!("Basic {basic}"))
            .await
            .status(),
        StatusCode::OK
    );

    for uri in ["/api/v1/system/versions", "/3.1/system/versions"] {
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
    let app = listmngr_api::router(db.clone(), Config::default(), 100);
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
    let app = listmngr_api::router(db.clone(), Config::default(), 1000);
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

async fn assert_phase_one_catalogs(app: &axum::Router, token: &str) {
    let cases: [(&str, &[&str]); 2] = [
        (
            "/api/v1/system/pipelines",
            &[
                "default-posting-pipeline",
                "virgin",
                "default-owner-pipeline",
            ],
        ),
        (
            "/api/v1/system/chains",
            &[
                "default-posting-chain",
                "default-owner-chain",
                "accept",
                "hold",
                "reject",
                "discard",
                "moderation",
                "header-match",
                "dmarc-mitigation",
            ],
        ),
    ];
    for (path, expected) in cases {
        let body = response_json(call(app, "GET", path, Some(token), None).await).await;
        let entries = body["items"].as_array().unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            expected
        );
        for entry in entries {
            assert_eq!(entry["phase"], "phase1");
            assert_eq!(entry["executable"], false);
            assert_eq!(entry["status"], "catalog_only");
            assert!(entry.get("handlers").is_none());
            assert!(entry.get("rules").is_none());
        }
    }
}

#[tokio::test]
async fn phase_one_catalogs_and_resource_projections_are_truthful() {
    let (app, token, user_id) = setup(&["admin"]).await;
    assert_phase_one_catalogs(&app, &token).await;

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
}
