//! Mailman's Automatic Responses on the list configuration resource:
//! defaults, JSON and form writes, `PUT` reset and validation.
use super::*;
use serde_json::json;

fn config_uri(prefix: &str) -> String {
    format!("{prefix}/lists/dev.example.com/config")
}

#[tokio::test]
async fn automatic_response_settings_round_trip_and_validate() {
    let (app, token, _) = setup(&["admin"]).await;
    create_configurable_list(&app, &token).await;
    for prefix in ["/api/v1", "/3.1"] {
        let uri = config_uri(prefix);
        let initial = response_json(call(&app, "GET", &uri, Some(&token), None).await).await;
        for (key, expected) in [
            ("autorespond_owner", json!("none")),
            ("autoresponse_owner_text", json!("")),
            ("autorespond_postings", json!("none")),
            ("autoresponse_postings_text", json!("")),
            ("autorespond_requests", json!("none")),
            ("autoresponse_request_text", json!("")),
            ("autoresponse_grace_period", json!(90)),
        ] {
            assert_eq!(initial[key], expected, "{prefix} default {key}");
        }
        let patch = json!({
            "autorespond_owner": "respond_and_discard",
            "autoresponse_owner_text": "Owners of $listname answer weekly.",
            "autorespond_requests": "respond_and_continue",
            "autoresponse_grace_period": 7,
        });
        let saved =
            response_json(call(&app, "PATCH", &uri, Some(&token), Some(&patch.to_string())).await)
                .await;
        assert_eq!(
            saved["autorespond_owner"], "respond_and_discard",
            "{prefix}"
        );
        assert_eq!(
            saved["autoresponse_owner_text"],
            "Owners of $listname answer weekly."
        );
        assert_eq!(saved["autorespond_requests"], "respond_and_continue");
        assert_eq!(saved["autorespond_postings"], "none");
        assert_eq!(saved["autoresponse_grace_period"], 7);

        // mailmanclient posts forms.
        let form = "autorespond_postings=respond&autoresponse_postings_text=Got%20it.&autoresponse_grace_period=0";
        let saved = response_json(call_form(&app, "PATCH", &uri, &token, form).await).await;
        assert_eq!(
            saved["autorespond_postings"], "respond_and_continue",
            "{prefix}"
        );
        assert_eq!(saved["autoresponse_postings_text"], "Got it.");
        assert_eq!(saved["autoresponse_grace_period"], 0);

        for (label, body) in [
            ("unknown action", json!({"autorespond_owner": "always"})),
            ("negative grace", json!({"autoresponse_grace_period": -1})),
            (
                "grace beyond ten years",
                json!({"autoresponse_grace_period": 3651}),
            ),
            (
                "oversized text",
                json!({"autoresponse_request_text": "x".repeat(70_000)}),
            ),
        ] {
            assert_eq!(
                call(&app, "PATCH", &uri, Some(&token), Some(&body.to_string()))
                    .await
                    .status(),
                StatusCode::BAD_REQUEST,
                "{prefix} {label}"
            );
        }

        // PUT resets what it omits to Mailman's defaults.
        let reset = response_json(
            call(
                &app,
                "PUT",
                &uri,
                Some(&token),
                Some(&json!({"display_name": "Dev"}).to_string()),
            )
            .await,
        )
        .await;
        assert_eq!(reset["autorespond_owner"], "none", "{prefix}");
        assert_eq!(reset["autoresponse_grace_period"], 90, "{prefix}");
    }
}
