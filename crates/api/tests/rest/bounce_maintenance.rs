use super::*;
use serde_json::{Value, json};

#[tokio::test]
async fn maintenance_config_native_compat_form_json_attributes_and_reset() {
    let (app, token) = maintenance_fixture().await;
    for prefix in ["/api/v1", "/3.1"] {
        let path = format!("{prefix}/lists/dev.example.com/config");
        let days = if prefix == "/3.1" {
            json!("7d")
        } else {
            json!(7)
        };
        let defaults = response_json(call(&app, "GET", &path, Some(&token), None).await).await;
        assert_eq!(defaults["bounce_you_are_disabled_warnings"], 3);
        assert_eq!(defaults["bounce_you_are_disabled_warnings_interval"], days);
        assert_eq!(defaults["bounce_notify_owner_on_removal"], true);
        let form = "bounce_you_are_disabled_warnings=0&bounce_you_are_disabled_warnings_interval=0&bounce_notify_owner_on_removal=False";
        assert_eq!(
            call_form(&app, "PATCH", &path, &token, form).await.status(),
            StatusCode::OK
        );
        let kept = response_json(call(&app, "PATCH", &path, Some(&token), Some("{}")).await).await;
        assert_eq!(kept["bounce_notify_owner_on_removal"], false);
        assert_eq!(kept["bounce_you_are_disabled_warnings"], 0);
        let interval = if prefix == "/3.1" {
            json!("0d")
        } else {
            json!(0)
        };
        assert_eq!(kept["bounce_you_are_disabled_warnings_interval"], interval);
        let attr = format!("{path}/bounce_you_are_disabled_warnings_interval");
        let got = response_json(call(&app, "GET", &attr, Some(&token), None).await).await;
        assert_eq!(got, interval);
        for (key, bad) in [
            ("bounce_you_are_disabled_warnings", json!(101)),
            ("bounce_you_are_disabled_warnings_interval", json!(36501)),
            ("bounce_notify_owner_on_removal", json!(1)),
        ] {
            assert_eq!(
                call(
                    &app,
                    "PATCH",
                    &path,
                    Some(&token),
                    Some(&json!({key:bad}).to_string())
                )
                .await
                .status(),
                StatusCode::BAD_REQUEST
            );
        }
        if prefix == "/3.1" {
            for body in [
                r#"{"bounce_you_are_disabled_warnings_interval":"9d"}"#,
                r#"{"bounce_you_are_disabled_warnings_interval":9}"#,
            ] {
                let got =
                    response_json(call(&app, "PATCH", &path, Some(&token), Some(body)).await).await;
                assert_eq!(got["bounce_you_are_disabled_warnings_interval"], "9d");
            }
            assert_eq!(call_form(&app,"PATCH",&path,&token,"bounce_you_are_disabled_warnings_interval=36500d&bounce_notify_owner_on_removal=True").await.status(),StatusCode::OK);
            for bad in ["1.5d", "-1d", "1h", "+7d", " 7d"] {
                assert_eq!(
                    call(
                        &app,
                        "PATCH",
                        &path,
                        Some(&token),
                        Some(&json!({"bounce_you_are_disabled_warnings_interval":bad}).to_string())
                    )
                    .await
                    .status(),
                    StatusCode::BAD_REQUEST
                );
            }
        }
        let reset = response_json(call(&app, "PUT", &path, Some(&token), Some("{}")).await).await;
        assert_eq!(reset["bounce_you_are_disabled_warnings"], 3);
        assert_eq!(reset["bounce_you_are_disabled_warnings_interval"], days);
        assert_eq!(reset["bounce_notify_owner_on_removal"], true);
    }
    let doc = response_json(call(&app, "GET", "/openapi.json", None, None).await).await;
    for schema in ["ListConfigInput", "ListConfigResponse", "MailingList"] {
        check_schema(&doc["components"]["schemas"][schema]);
    }
}
async fn maintenance_fixture() -> (axum::Router, String) {
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
    (app, token)
}

fn check_schema(value: &Value) {
    fn property<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
        v.get("properties").and_then(|p| p.get(key)).or_else(|| {
            v.get("allOf")
                .and_then(Value::as_array)
                .and_then(|a| a.iter().find_map(|v| property(v, key)))
        })
    }
    for (key, default, max) in [
        ("bounce_you_are_disabled_warnings", json!(3), Some(100)),
        (
            "bounce_you_are_disabled_warnings_interval",
            json!(7),
            Some(36500),
        ),
        ("bounce_notify_owner_on_removal", json!(true), None),
    ] {
        let field = property(value, key).unwrap();
        assert_eq!(field["default"], default);
        if let Some(max) = max {
            assert_eq!(field["minimum"], 0);
            assert_eq!(field["maximum"], max);
        }
    }
}
