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
    request.extensions_mut().insert(axum::extract::ConnectInfo(
        "127.0.0.1:4242".parse::<std::net::SocketAddr>().unwrap(),
    ));
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
async fn openapi_covers_every_live_v1_method_and_declares_bearer_security() {
    let (app, _, _) = setup(&["admin"]).await;
    let response = call(&app, "GET", "/openapi.json", None, None).await;
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let document: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let expected = [
        ("/api/v1/system/versions", "get"),
        ("/api/v1/system/configuration", "get"),
        ("/api/v1/system/configuration/{section}", "get"),
        ("/api/v1/system/preferences", "get"),
        ("/api/v1/system/pipelines", "get"),
        ("/api/v1/system/chains", "get"),
        ("/api/v1/domains", "get"),
        ("/api/v1/domains", "post"),
        ("/api/v1/domains/{host}", "get"),
        ("/api/v1/domains/{host}", "delete"),
        ("/api/v1/domains/{host}/lists", "get"),
        ("/api/v1/domains/{host}/owners", "get"),
        ("/api/v1/domains/{host}/uris", "get"),
        ("/api/v1/lists", "get"),
        ("/api/v1/lists", "post"),
        ("/api/v1/lists/styles", "get"),
        ("/api/v1/lists/{id}", "get"),
        ("/api/v1/lists/{id}", "delete"),
        ("/api/v1/lists/{id}/config", "get"),
        ("/api/v1/lists/{id}/config", "put"),
        ("/api/v1/lists/{id}/config", "patch"),
        ("/api/v1/lists/{id}/config/{attr}", "get"),
        ("/api/v1/lists/{id}/config/{attr}", "put"),
        ("/api/v1/lists/{id}/config/{attr}", "patch"),
        ("/api/v1/lists/{id}/archivers", "get"),
        ("/api/v1/lists/{id}/uris", "get"),
        ("/api/v1/lists/{id}/templates", "get"),
        ("/api/v1/lists/{id}/roster/{role}", "get"),
        ("/api/v1/lists/{id}/member/{email}", "get"),
        ("/api/v1/members", "post"),
        ("/api/v1/members/mass", "post"),
        ("/api/v1/members/find", "post"),
        ("/api/v1/members/{id}", "get"),
        ("/api/v1/members/{id}", "patch"),
        ("/api/v1/members/{id}", "delete"),
        ("/api/v1/members/{id}/preferences", "get"),
        ("/api/v1/members/{id}/preferences", "put"),
        ("/api/v1/members/{id}/preferences", "patch"),
        ("/api/v1/members/{id}/all/preferences", "get"),
        ("/api/v1/users", "get"),
        ("/api/v1/users", "post"),
        ("/api/v1/users/{id}", "get"),
        ("/api/v1/users/{id}", "patch"),
        ("/api/v1/users/{id}", "delete"),
        ("/api/v1/users/{id}/addresses", "get"),
        ("/api/v1/users/{id}/addresses", "post"),
        ("/api/v1/users/{id}/preferences", "get"),
        ("/api/v1/users/{id}/preferences", "put"),
        ("/api/v1/users/{id}/preferences", "patch"),
        ("/api/v1/users/{id}/all/preferences", "get"),
        ("/api/v1/users/{id}/login", "post"),
        ("/api/v1/addresses/{email}", "get"),
        ("/api/v1/addresses/{email}/verify", "post"),
        ("/api/v1/addresses/{email}/unverify", "post"),
        ("/api/v1/addresses/{email}/user", "get"),
        ("/api/v1/addresses/{email}/user", "post"),
        ("/api/v1/addresses/{email}/user", "delete"),
        ("/api/v1/addresses/{email}/memberships", "get"),
        ("/api/v1/addresses/{email}/preferences", "get"),
        ("/api/v1/addresses/{email}/preferences", "put"),
        ("/api/v1/addresses/{email}/preferences", "patch"),
        ("/api/v1/addresses/{email}/all/preferences", "get"),
        ("/api/v1/owners", "get"),
    ];
    for (path, method) in expected {
        let operation = &document["paths"][path][method];
        assert!(operation.is_object(), "missing {method} {path}");
        assert_eq!(
            operation["security"][0]["bearerAuth"],
            serde_json::json!([]),
            "security for {method} {path}"
        );
    }
    assert_eq!(
        document["components"]["securitySchemes"]["bearerAuth"]["scheme"],
        "bearer"
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
