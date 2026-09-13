//! Mailman's template URI resource on every scope: `GET/PATCH/PUT/DELETE
//! /{scope}/uris` and `/{scope}/uris/{name}`, on both prefixes, plus the
//! listmngr inline-body extension on `/lists/{id}/templates/{name}`.
use super::*;

const WELCOME: &str = "list:user:notice:welcome";
const GOODBYE: &str = "list:user:notice:goodbye";

async fn entries(app: &axum::Router, token: &str, uri: &str) -> Vec<serde_json::Value> {
    let body = response_json(call(app, "GET", uri, Some(token), None).await).await;
    let key = if uri.starts_with("/3.1") {
        "entries"
    } else {
        "items"
    };
    body[key].as_array().cloned().unwrap_or_default()
}

#[tokio::test]
async fn template_uris_round_trip_on_every_scope_and_prefix() {
    let (app, token, _) = setup(&["admin"]).await;
    create_configurable_list(&app, &token).await;
    for prefix in ["/api/v1", "/3.1"] {
        for scope in ["/lists/dev.example.com", "/domains/example.com", ""] {
            let uri = format!("{prefix}{scope}/uris");
            assert!(entries(&app, &token, &uri).await.is_empty(), "{uri}");

            // mailmanclient's set_template: a form PATCH of name=uri pairs.
            assert_eq!(
                call_form(
                    &app,
                    "PATCH",
                    &uri,
                    &token,
                    &format!(
                        "{WELCOME}=https%3A%2F%2Ftemplates.example.com%2Fwelcome.txt&username=reader&password=hunter2"
                    )
                )
                .await
                .status(),
                StatusCode::NO_CONTENT,
                "{uri}"
            );
            assert_eq!(
                call(
                    &app,
                    "PATCH",
                    &uri,
                    Some(&token),
                    Some(&format!(r#"{{"{GOODBYE}":"mailman:///{GOODBYE}"}}"#))
                )
                .await
                .status(),
                StatusCode::NO_CONTENT,
                "{uri}"
            );
            let listed = entries(&app, &token, &uri).await;
            assert_eq!(listed.len(), 2, "{uri}: {listed:?}");
            assert_eq!(listed[0]["name"], GOODBYE);
            assert_eq!(listed[0]["uri"], format!("mailman:///{GOODBYE}"));
            assert_eq!(listed[0]["self_link"], format!("{uri}/{GOODBYE}"));
            assert_eq!(listed[1]["name"], WELCOME);
            assert_eq!(
                listed[1]["uri"],
                "https://templates.example.com/welcome.txt"
            );
            assert!(
                listed[1].get("password").is_none(),
                "password must never be projected"
            );

            let one = response_json(
                call(&app, "GET", &format!("{uri}/{WELCOME}"), Some(&token), None).await,
            )
            .await;
            assert_eq!(one["name"], WELCOME);
            assert_eq!(one["uri"], "https://templates.example.com/welcome.txt");
            assert_eq!(one["self_link"], format!("{uri}/{WELCOME}"));

            assert_per_name_patch_and_delete(&app, &token, &uri).await;
            assert_put_replaces_and_delete_clears(&app, &token, &uri).await;
            assert!(entries(&app, &token, &uri).await.is_empty());
        }
    }
}

async fn assert_per_name_patch_and_delete(app: &axum::Router, token: &str, uri: &str) {
    assert_eq!(
        call_form(
            app,
            "PATCH",
            &format!("{uri}/{WELCOME}"),
            token,
            &format!("uri=mailman%3A%2F%2F%2F{WELCOME}")
        )
        .await
        .status(),
        StatusCode::NO_CONTENT
    );
    let one =
        response_json(call(app, "GET", &format!("{uri}/{WELCOME}"), Some(token), None).await).await;
    assert_eq!(one["uri"], format!("mailman:///{WELCOME}"), "{uri}");
    assert_eq!(
        call(
            app,
            "DELETE",
            &format!("{uri}/{WELCOME}"),
            Some(token),
            None
        )
        .await
        .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(app, "GET", &format!("{uri}/{WELCOME}"), Some(token), None)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(entries(app, token, uri).await.len(), 1);
}

async fn assert_put_replaces_and_delete_clears(app: &axum::Router, token: &str, uri: &str) {
    assert_eq!(
        call(
            app,
            "PUT",
            uri,
            Some(token),
            Some(&format!(r#"{{"{WELCOME}":"mailman:///{WELCOME}"}}"#))
        )
        .await
        .status(),
        StatusCode::NO_CONTENT
    );
    let listed = entries(app, token, uri).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["name"], WELCOME);
    assert_eq!(
        call(app, "DELETE", uri, Some(token), None).await.status(),
        StatusCode::NO_CONTENT
    );
    assert!(entries(app, token, uri).await.is_empty());
}

#[tokio::test]
async fn template_uris_reject_unknown_names_bad_uris_and_wrong_scopes() {
    let (app, token, _) = setup(&["admin"]).await;
    create_configurable_list(&app, &token).await;
    let uri = "/api/v1/lists/dev.example.com/uris";
    for payload in [
        r#"{"list:user:notice:made-up":"mailman:///list:user:notice:welcome"}"#,
        r#"{"list:user:notice:welcome":"http://plain.example.com/x.txt"}"#,
        r#"{"list:user:notice:welcome":"file://relative.txt"}"#,
        r#"{"list:user:notice:welcome":7}"#,
        r#"{"list:user:notice:welcome":""}"#,
        "[]",
    ] {
        assert_eq!(
            call(&app, "PATCH", uri, Some(&token), Some(payload))
                .await
                .status(),
            StatusCode::BAD_REQUEST,
            "{payload}"
        );
    }
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/lists/ghost.example.com/uris",
            Some(&token),
            None
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/domains/ghost.com/uris",
            Some(&token),
            None
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("{uri}/list:user:notice:made-up"),
            Some(&token),
            None
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn inline_template_bodies_are_a_listmngr_extension_with_language() {
    let (app, token, _) = setup(&["admin"]).await;
    create_configurable_list(&app, &token).await;
    let uri = format!("/api/v1/lists/dev.example.com/templates/{WELCOME}");
    assert_eq!(
        call(
            &app,
            "PUT",
            &uri,
            Some(&token),
            Some(r#"{"language":"vi","body":"Chào mừng $display_name\n"}"#)
        )
        .await
        .status(),
        StatusCode::NO_CONTENT
    );
    let listed = response_json(
        call(
            &app,
            "GET",
            "/api/v1/lists/dev.example.com/templates",
            Some(&token),
            None,
        )
        .await,
    )
    .await;
    let items = listed["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["name"], WELCOME);
    assert_eq!(items[0]["language"], "vi");
    assert_eq!(items[0]["body"], "Chào mừng $display_name\n");
    for payload in [
        r#"{"language":"","body":"x"}"#,
        r#"{"language":"vi"}"#,
        r#"{"body":"x"}"#,
    ] {
        assert_eq!(
            call(&app, "PUT", &uri, Some(&token), Some(payload))
                .await
                .status(),
            StatusCode::BAD_REQUEST,
            "{payload}"
        );
    }
    assert_eq!(
        call(&app, "DELETE", &uri, Some(&token), None)
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    let listed = response_json(
        call(
            &app,
            "GET",
            "/api/v1/lists/dev.example.com/templates",
            Some(&token),
            None,
        )
        .await,
    )
    .await;
    assert!(listed["items"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn template_writes_need_write_scope_and_site_uris_need_admin() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains().create("example.com", "", None).await.unwrap();
    db.lists()
        .create(listmngr_db::NewList {
            list_id: "dev.example.com".parse().unwrap(),
            display_name: "Dev".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let user = db
        .users()
        .create(NewUser {
            display_name: "reader".into(),
            email: "reader@example.com".into(),
            password: "very secure password".into(),
            server_owner: false,
        })
        .await
        .unwrap();
    let reader = db
        .tokens()
        .create(user.id, "reader", &["lists:read", "lists:write"], None)
        .await
        .unwrap()
        .token;
    let read_only = db
        .tokens()
        .create(user.id, "read-only", &["lists:read"], None)
        .await
        .unwrap()
        .token;
    let app = listmngr_api::router(db, config_with_rate(100));
    assert_eq!(
        call(
            &app,
            "PATCH",
            "/api/v1/lists/dev.example.com/uris",
            Some(&read_only),
            Some(&format!(r#"{{"{WELCOME}":"mailman:///{WELCOME}"}}"#))
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            "PATCH",
            "/api/v1/lists/dev.example.com/uris",
            Some(&reader),
            Some(&format!(r#"{{"{WELCOME}":"mailman:///{WELCOME}"}}"#))
        )
        .await
        .status(),
        StatusCode::NO_CONTENT
    );
    // Site templates are an administrator concern.
    assert_eq!(
        call(&app, "GET", "/api/v1/uris", Some(&reader), None)
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
}
