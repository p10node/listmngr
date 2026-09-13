//! Mailman's `admin_notify_mchanges` on the list configuration resource.
use super::*;

#[tokio::test]
async fn admin_notify_mchanges_defaults_off_and_round_trips_on_both_prefixes() {
    let (app, token, _) = setup(&["admin"]).await;
    create_configurable_list(&app, &token).await;
    for prefix in ["/api/v1", "/3.1"] {
        let uri = format!("{prefix}/lists/dev.example.com/config");
        let config = response_json(call(&app, "GET", &uri, Some(&token), None).await).await;
        assert_eq!(config["admin_notify_mchanges"], false, "{prefix}");
        // mailmanclient patches Python's spelling of a boolean as a form.
        assert_eq!(
            call_form(&app, "PATCH", &uri, &token, "admin_notify_mchanges=True")
                .await
                .status(),
            StatusCode::OK
        );
        let config = response_json(call(&app, "GET", &uri, Some(&token), None).await).await;
        assert_eq!(config["admin_notify_mchanges"], true, "{prefix}");
        let attribute = format!("{uri}/admin_notify_mchanges");
        assert_eq!(
            response_json(call(&app, "GET", &attribute, Some(&token), None).await).await,
            true
        );
        assert_eq!(
            call(
                &app,
                "PATCH",
                &uri,
                Some(&token),
                Some(r#"{"admin_notify_mchanges":"yes"}"#)
            )
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
                Some(r#"{"admin_notify_mchanges":false}"#)
            )
            .await
            .status(),
            StatusCode::OK
        );
        let config = response_json(call(&app, "GET", &uri, Some(&token), None).await).await;
        assert_eq!(config["admin_notify_mchanges"], false, "{prefix}");
    }
}
