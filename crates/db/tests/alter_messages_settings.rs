//! Mailman's Alter Messages, Member Policy, DMARC text and unrecognized
//! bounce settings: Mailman defaults on a new list, validated patches that
//! persist with their audit event, and rejections that leave the row alone.
use listmngr_core::ListId;
use listmngr_db::{Database, NewList};
use serde_json::{Value, json};

async fn fixture(db: &Database) -> ListId {
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "example", None)
        .await
        .unwrap();
    let list: ListId = "dev.example.invalid".parse().unwrap();
    db.lists()
        .create(NewList {
            list_id: list.clone(),
            display_name: "Dev".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    list
}

async fn config(db: &Database, list: &ListId) -> Value {
    serde_json::to_value(db.lists().get(list).await.unwrap()).unwrap()
}

/// Mailman's `BasicOperation` style defaults for every setting this WP adds.
fn mailman_defaults() -> Value {
    json!({
        "filter_content": false,
        "filter_types": [],
        "pass_types": [],
        "filter_extensions": [],
        "pass_extensions": [],
        "collapse_alternatives": true,
        "convert_html_to_plaintext": false,
        "filter_action": "discard",
        "include_rfc2369_headers": true,
        "allow_list_posts": true,
        "reply_goes_to_list": "no_munging",
        "reply_to_address": "",
        "first_strip_reply_to": false,
        "personalize": "none",
        "include_sender_header": true,
        "subscription_policy": "confirm",
        "unsubscription_policy": "confirm",
        "member_roster_visibility": "moderators",
        "dmarc_addresses": [],
        "dmarc_moderation_notice": "",
        "dmarc_wrapped_message_text": "",
        "forward_unrecognized_bounces_to": "administrators",
    })
}

fn full_patch() -> Value {
    json!({
        "filter_content": true,
        "filter_types": ["image/jpeg", "application/octet-stream", "video"],
        "pass_types": ["multipart/mixed", "multipart/alternative", "text/plain"],
        "filter_extensions": ["exe", "bat", "cmd"],
        "pass_extensions": ["txt", "pdf"],
        "collapse_alternatives": false,
        "convert_html_to_plaintext": true,
        "filter_action": "forward",
        "include_rfc2369_headers": false,
        "allow_list_posts": false,
        "reply_goes_to_list": "explicit_header",
        "reply_to_address": "Replies@Example.invalid",
        "first_strip_reply_to": true,
        "personalize": "full",
        "include_sender_header": false,
        "subscription_policy": "confirm_then_moderate",
        "unsubscription_policy": "open",
        "member_roster_visibility": "public",
        "dmarc_addresses": ["^.*@yahoo\\.com$", "friend@example.invalid"],
        "dmarc_moderation_notice": "Your post was wrapped because of $listname DMARC policy.\n",
        "dmarc_wrapped_message_text": "The original message is attached.\n",
        "forward_unrecognized_bounces_to": "site_owner",
    })
}

async fn scenario(db: &Database) {
    let list = fixture(db).await;
    let initial = config(db, &list).await;
    for (key, expected) in mailman_defaults().as_object().unwrap() {
        assert_eq!(&initial[key], expected, "default {key}");
    }

    let patch = full_patch();
    db.lists().update(&list, &patch).await.unwrap();
    let saved = config(db, &list).await;
    for (key, expected) in patch.as_object().unwrap() {
        // `reply_to_address` is validated as a mailbox and stored as written.
        assert_eq!(&saved[key], expected, "saved {key}");
    }
    let audit: String = sqlx::query_scalar(
        "SELECT diff FROM audit_log WHERE action='list.config' ORDER BY at DESC LIMIT 1",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    let audit: Value = serde_json::from_str(&audit).unwrap();
    assert_eq!(audit["filter_action"], "forward");
    assert_eq!(audit["personalize"], "full");
    assert_eq!(
        audit["dmarc_addresses"],
        json!(["^.*@yahoo\\.com$", "friend@example.invalid"])
    );

    // Clearing works with the empty values Mailman uses.
    db.lists()
        .update(
            &list,
            &json!({"reply_to_address": "", "filter_types": [], "dmarc_addresses": [], "dmarc_moderation_notice": ""}),
        )
        .await
        .unwrap();
    let cleared = config(db, &list).await;
    assert_eq!(cleared["reply_to_address"], "");
    assert_eq!(cleared["filter_types"], json!([]));
    assert_eq!(cleared["dmarc_addresses"], json!([]));
    assert_eq!(cleared["dmarc_moderation_notice"], "");

    rejected_patches_leave_the_row_alone(db, &list).await;
}

async fn rejected_patches_leave_the_row_alone(db: &Database, list: &ListId) {
    let before = config(db, list).await;
    let too_long = "x".repeat(65_537);
    for (label, bad) in [
        ("unknown filter action", json!({"filter_action": "explode"})),
        (
            "unknown munging",
            json!({"reply_goes_to_list": "point_to_owner"}),
        ),
        ("unknown personalization", json!({"personalize": "yes"})),
        ("unknown policy", json!({"subscription_policy": "closed"})),
        (
            "unknown visibility",
            json!({"member_roster_visibility": "everyone"}),
        ),
        (
            "unknown disposition",
            json!({"forward_unrecognized_bounces_to": "trash"}),
        ),
        ("boolean as string", json!({"filter_content": "true"})),
        (
            "mailbox syntax",
            json!({"reply_to_address": "not an address"}),
        ),
        (
            "mailbox with newline",
            json!({"reply_to_address": "a@example.invalid\nBcc: x"}),
        ),
        ("type list as string", json!({"filter_types": "text/html"})),
        (
            "type with whitespace",
            json!({"filter_types": ["text/ html"]}),
        ),
        ("type with two slashes", json!({"pass_types": ["a/b/c"]})),
        ("empty type", json!({"pass_types": [""]})),
        ("extension with slash", json!({"pass_extensions": ["a/b"]})),
        ("extension with dot", json!({"filter_extensions": [".exe"]})),
        ("bad dmarc regex", json!({"dmarc_addresses": ["^("]})),
        (
            "notice too long",
            json!({"dmarc_moderation_notice": too_long}),
        ),
        (
            "wrapped text too long",
            json!({"dmarc_wrapped_message_text": too_long}),
        ),
    ] {
        let mut patch = bad.clone();
        patch["display_name"] = json!("must not persist");
        let error = db.lists().update(list, &patch).await.unwrap_err();
        assert!(
            matches!(error, listmngr_core::Error::Validation(_)),
            "{label}: {error:?}"
        );
        assert_eq!(config(db, list).await, before, "{label} leaked a write");
    }
}

#[tokio::test]
async fn alter_messages_member_policy_and_dmarc_settings_round_trip_on_sqlite() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    scenario(&db).await;
}

#[tokio::test]
#[ignore = "requires explicit disposable TEST_POSTGRES_URL; uses own schema"]
async fn postgres_alter_messages_settings_contract() {
    sqlx::any::install_default_drivers();
    let url = std::env::var("TEST_POSTGRES_URL").expect("explicit fixture database required");
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("alter_messages_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let isolated = format!(
        "{url}{}options=-csearch_path%3D{schema}",
        if url.contains('?') { '&' } else { '?' }
    );
    let result = tokio::spawn(async move {
        let db = Database::connect(&isolated, 2).await.unwrap();
        scenario(&db).await;
    })
    .await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
    result.unwrap();
}
