//! Mailman's Alter Messages, Member Policy, DMARC text and unrecognized
//! bounce settings on the list configuration resource: defaults, JSON and
//! mailmanclient-style form writes (repeated keys for lists, Python
//! booleans), `PUT` resets, and validation on both prefixes.
use super::*;
use serde_json::json;

fn config_uri(prefix: &str) -> String {
    format!("{prefix}/lists/dev.example.com/config")
}

#[tokio::test]
async fn alter_messages_settings_default_to_mailman_and_round_trip_as_json() {
    let (app, token, _) = setup(&["admin"]).await;
    create_configurable_list(&app, &token).await;
    for prefix in ["/api/v1", "/3.1"] {
        let uri = config_uri(prefix);
        let initial = response_json(call(&app, "GET", &uri, Some(&token), None).await).await;
        for (key, expected) in [
            ("filter_content", json!(false)),
            ("filter_types", json!([])),
            ("pass_types", json!([])),
            ("filter_extensions", json!([])),
            ("pass_extensions", json!([])),
            ("collapse_alternatives", json!(true)),
            ("convert_html_to_plaintext", json!(false)),
            ("filter_action", json!("discard")),
            ("include_rfc2369_headers", json!(true)),
            ("allow_list_posts", json!(true)),
            ("reply_goes_to_list", json!("no_munging")),
            ("reply_to_address", json!("")),
            ("first_strip_reply_to", json!(false)),
            ("personalize", json!("none")),
            ("include_sender_header", json!(true)),
            ("subscription_policy", json!("confirm")),
            ("unsubscription_policy", json!("confirm")),
            ("member_roster_visibility", json!("moderators")),
            ("dmarc_addresses", json!([])),
            ("dmarc_moderation_notice", json!("")),
            ("dmarc_wrapped_message_text", json!("")),
            ("forward_unrecognized_bounces_to", json!("administrators")),
        ] {
            assert_eq!(initial[key], expected, "{prefix} default {key}");
        }

        let patch = json!({
            "filter_content": true,
            "filter_types": ["image/jpeg", "video"],
            "pass_extensions": ["txt", "pdf"],
            "filter_action": "preserve",
            "reply_goes_to_list": "point_to_list",
            "personalize": "individual",
            "subscription_policy": "moderate",
            "member_roster_visibility": "members",
            "dmarc_addresses": ["^.*@yahoo\\.com$"],
            "dmarc_moderation_notice": "Held for DMARC.",
            "forward_unrecognized_bounces_to": "discard"
        });
        let saved = call(&app, "PATCH", &uri, Some(&token), Some(&patch.to_string())).await;
        assert_eq!(saved.status(), StatusCode::OK, "{prefix}");
        let saved = response_json(saved).await;
        for (key, expected) in patch.as_object().unwrap() {
            assert_eq!(&saved[key], expected, "{prefix} saved {key}");
        }
        let fetched = response_json(call(&app, "GET", &uri, Some(&token), None).await).await;
        assert_eq!(fetched["filter_types"], json!(["image/jpeg", "video"]));
        assert_eq!(fetched["personalize"], "individual");

        // PUT is a full replacement: omitted settings return to Mailman's defaults.
        assert_eq!(
            call(
                &app,
                "PUT",
                &uri,
                Some(&token),
                Some(r#"{"description":"reset"}"#)
            )
            .await
            .status(),
            StatusCode::OK
        );
        let reset = response_json(call(&app, "GET", &uri, Some(&token), None).await).await;
        assert_eq!(reset["filter_content"], false, "{prefix}");
        assert_eq!(reset["filter_types"], json!([]), "{prefix}");
        assert_eq!(reset["filter_action"], "discard", "{prefix}");
        assert_eq!(reset["personalize"], "none", "{prefix}");
        assert_eq!(reset["subscription_policy"], "confirm", "{prefix}");
        assert_eq!(reset["dmarc_addresses"], json!([]), "{prefix}");
        assert_eq!(
            reset["forward_unrecognized_bounces_to"], "administrators",
            "{prefix}"
        );
    }
}

#[tokio::test]
async fn alter_messages_settings_accept_mailmanclient_form_encoding() {
    let (app, token, _) = setup(&["admin"]).await;
    create_configurable_list(&app, &token).await;
    for prefix in ["/api/v1", "/3.1"] {
        let uri = config_uri(prefix);
        // mailmanclient posts `urlencode(data, doseq=True)`: lists become
        // repeated keys and booleans arrive as Python's `True`/`False`.
        let form = "filter_content=True&filter_types=image%2Fjpeg&filter_types=Application%2FPDF&pass_extensions=txt&collapse_alternatives=False&filter_action=reject&reply_goes_to_list=explicit_header&reply_to_address=replies%40example.com&personalize=full&unsubscription_policy=open";
        let saved = call_form(&app, "PATCH", &uri, &token, form).await;
        assert_eq!(saved.status(), StatusCode::OK, "{prefix}");
        let saved = response_json(saved).await;
        assert_eq!(saved["filter_content"], true, "{prefix}");
        assert_eq!(
            saved["filter_types"],
            json!(["image/jpeg", "application/pdf"]),
            "{prefix}: repeated keys and lowercase MIME types"
        );
        assert_eq!(
            saved["pass_extensions"],
            json!(["txt"]),
            "{prefix}: a single value is still a list"
        );
        assert_eq!(saved["collapse_alternatives"], false, "{prefix}");
        assert_eq!(saved["filter_action"], "reject", "{prefix}");
        assert_eq!(saved["reply_goes_to_list"], "explicit_header", "{prefix}");
        assert_eq!(saved["reply_to_address"], "replies@example.com", "{prefix}");
        assert_eq!(saved["personalize"], "full", "{prefix}");
        assert_eq!(saved["unsubscription_policy"], "open", "{prefix}");

        // An empty form value clears a list, as an empty JSON array does.
        let cleared = call_form(&app, "PATCH", &uri, &token, "filter_types=").await;
        assert_eq!(cleared.status(), StatusCode::OK, "{prefix}");
        assert_eq!(response_json(cleared).await["filter_types"], json!([]));

        // Existing address lists gain the same repeated-key form support.
        let aliases = call_form(
            &app,
            "PATCH",
            &uri,
            &token,
            "acceptable_aliases=other%40example.com&acceptable_aliases=%5E.*%40example.org%24",
        )
        .await;
        assert_eq!(aliases.status(), StatusCode::OK, "{prefix}");
        assert_eq!(
            response_json(aliases).await["acceptable_aliases"],
            json!(["other@example.com", "^.*@example.org$"])
        );
    }
}

#[tokio::test]
async fn alter_messages_settings_reject_invalid_values_atomically() {
    let (app, token, _) = setup(&["admin"]).await;
    create_configurable_list(&app, &token).await;
    for prefix in ["/api/v1", "/3.1"] {
        let uri = config_uri(prefix);
        let before = response_json(call(&app, "GET", &uri, Some(&token), None).await).await;
        for invalid in [
            r#"{"filter_action":"explode","display_name":"must not persist"}"#,
            r#"{"reply_goes_to_list":"owner"}"#,
            r#"{"personalize":true}"#,
            r#"{"subscription_policy":"closed"}"#,
            r#"{"member_roster_visibility":"everyone"}"#,
            r#"{"forward_unrecognized_bounces_to":"trash"}"#,
            r#"{"filter_types":"text/html"}"#,
            r#"{"filter_types":["text/ html"]}"#,
            r#"{"pass_extensions":["a/b"]}"#,
            r#"{"reply_to_address":"not a mailbox"}"#,
            r#"{"dmarc_addresses":["^("]}"#,
            r#"{"filter_content":"yes"}"#,
        ] {
            for method in ["PATCH", "PUT"] {
                assert_eq!(
                    call(&app, method, &uri, Some(&token), Some(invalid))
                        .await
                        .status(),
                    StatusCode::BAD_REQUEST,
                    "{prefix} {method} {invalid}"
                );
            }
        }
        for invalid in [
            "filter_action=explode&display_name=must+not+persist",
            "filter_content=maybe",
            "filter_types=text%2F+html",
        ] {
            assert_eq!(
                call_form(&app, "PATCH", &uri, &token, invalid)
                    .await
                    .status(),
                StatusCode::BAD_REQUEST,
                "{prefix} form {invalid}"
            );
        }
        assert_eq!(
            response_json(call(&app, "GET", &uri, Some(&token), None).await).await,
            before,
            "{prefix}: a rejected patch must not persist anything"
        );
    }
}

#[tokio::test]
async fn topics_are_a_json_only_listmngr_extension_on_the_config_resource() {
    let (app, token, _) = setup(&["admin"]).await;
    create_configurable_list(&app, &token).await;
    for prefix in ["/api/v1", "/3.1"] {
        let uri = config_uri(prefix);
        let initial = response_json(call(&app, "GET", &uri, Some(&token), None).await).await;
        assert_eq!(initial["topics_enabled"], false, "{prefix}");
        assert_eq!(initial["topics_bodylines_limit"], 5, "{prefix}");
        assert_eq!(initial["topics"], json!([]), "{prefix}");
        let patch = json!({
            "topics_enabled": true,
            "topics_bodylines_limit": 0,
            "topics": [{"name": "Rust", "pattern": "cargo", "description": "Rust talk"}]
        });
        let saved = call(&app, "PATCH", &uri, Some(&token), Some(&patch.to_string())).await;
        assert_eq!(saved.status(), StatusCode::OK, "{prefix}");
        let saved = response_json(saved).await;
        assert_eq!(saved["topics_enabled"], true);
        assert_eq!(saved["topics"][0]["name"], "Rust");
        // Form encoding still reaches the scalar settings.
        let form = call_form(
            &app,
            "PATCH",
            &uri,
            &token,
            "topics_enabled=False&topics_bodylines_limit=-1",
        )
        .await;
        assert_eq!(form.status(), StatusCode::OK, "{prefix}");
        let form = response_json(form).await;
        assert_eq!(form["topics_enabled"], false);
        assert_eq!(form["topics_bodylines_limit"], -1);
        assert_eq!(
            form["topics"][0]["name"], "Rust",
            "untouched by a patch that omits it"
        );
        assert_eq!(
            call(
                &app,
                "PATCH",
                &uri,
                Some(&token),
                Some(r#"{"topics":[{"name":"x","pattern":"("}]}"#)
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
        // PUT without topics resets them.
        assert_eq!(
            call(
                &app,
                "PUT",
                &uri,
                Some(&token),
                Some(r#"{"description":"reset"}"#)
            )
            .await
            .status(),
            StatusCode::OK
        );
        let reset = response_json(call(&app, "GET", &uri, Some(&token), None).await).await;
        assert_eq!(reset["topics"], json!([]), "{prefix}");
        assert_eq!(reset["topics_bodylines_limit"], 5, "{prefix}");
    }
}
