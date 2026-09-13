//! Mailman's Digest settings on the list configuration resource.
use super::*;
use serde_json::json;

#[tokio::test]
async fn digest_settings_round_trip_and_validate() {
    let (app, token, _) = setup(&["admin"]).await;
    create_configurable_list(&app, &token).await;
    for prefix in ["/api/v1", "/3.1"] {
        let uri = format!("{prefix}/lists/dev.example.com/config");
        let initial = response_json(call(&app, "GET", &uri, Some(&token), None).await).await;
        assert_eq!(initial["digests_enabled"], true, "{prefix}");
        assert_eq!(initial["digest_size_threshold"], 30.0, "{prefix}");
        assert_eq!(initial["digest_send_periodic"], true, "{prefix}");
        assert_eq!(initial["digest_volume_frequency"], "monthly", "{prefix}");
        let saved = response_json(
            call_form(
                &app,
                "PATCH",
                &uri,
                &token,
                "digests_enabled=False&digest_size_threshold=512.5&digest_send_periodic=false&digest_volume_frequency=weekly",
            )
            .await,
        )
        .await;
        assert_eq!(saved["digests_enabled"], false, "{prefix}");
        assert_eq!(saved["digest_size_threshold"], 512.5, "{prefix}");
        assert_eq!(saved["digest_send_periodic"], false, "{prefix}");
        assert_eq!(saved["digest_volume_frequency"], "weekly", "{prefix}");
        for (label, body) in [
            ("negative threshold", json!({"digest_size_threshold": -1})),
            (
                "unknown frequency",
                json!({"digest_volume_frequency": "hourly"}),
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
        assert_eq!(reset["digest_volume_frequency"], "monthly", "{prefix}");
        assert_eq!(reset["digests_enabled"], true, "{prefix}");
    }
}
