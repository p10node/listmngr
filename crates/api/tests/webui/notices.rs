use super::{call, csrf, fixture, list_settings, login_as, member, text, user};
use axum::http::StatusCode;
use listmngr_core::MemberRole;
use listmngr_db::Database;

#[tokio::test]
async fn notice_settings_preserve_omissions_and_reject_invalid_boolean_forms() {
    let (db, app) = fixture().await;
    controls(&db, &app).await;
}

pub async fn controls(db: &Database, app: &axum::Router) {
    let user = user(db, "notice-controls@example.com", false).await;
    let owner = member(db, "notice-controls@example.com", MemberRole::Owner).await;
    let cookie = login_as(app, "notice-controls@example.com").await;
    let page = text(call(app, "GET", list_settings::URL, &cookie, "").await).await;
    let base = list_settings::form(&csrf(&page));
    let baseline: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workflow_notices")
        .fetch_one(db.pool())
        .await
        .unwrap();
    for (suffix, welcome, goodbye) in [
        (
            "&send_welcome_message=true&send_goodbye_message=true",
            true,
            true,
        ),
        ("", true, true),
        ("&send_welcome_message=false", false, true),
        ("&send_goodbye_message=false", false, false),
        (
            "&send_welcome_message=true&send_goodbye_message=false",
            true,
            false,
        ),
    ] {
        let before: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM audit_log WHERE action='list.config' AND actor_user_id=$1",
        )
        .bind(user.id.to_string())
        .fetch_all(db.pool())
        .await
        .unwrap();
        assert_eq!(
            call(
                app,
                "POST",
                list_settings::URL,
                &cookie,
                &format!("{base}{suffix}")
            )
            .await
            .status(),
            StatusCode::SEE_OTHER
        );
        let saved = db.lists().get(&owner.list_id).await.unwrap();
        assert_eq!(
            (saved.send_welcome_message, saved.send_goodbye_message),
            (welcome, goodbye)
        );
        let after: Vec<(String, String)> = sqlx::query_as(
            "SELECT id,diff FROM audit_log WHERE action='list.config' AND actor_user_id=$1",
        )
        .bind(user.id.to_string())
        .fetch_all(db.pool())
        .await
        .unwrap();
        assert_eq!(after.len(), before.len() + 1);
        let diff: serde_json::Value =
            serde_json::from_str(&after.iter().find(|(id, _)| !before.contains(id)).unwrap().1)
                .unwrap();
        for (key, expected) in [
            ("send_welcome_message", welcome),
            ("send_goodbye_message", goodbye),
        ] {
            if suffix.contains(key) {
                assert_eq!(diff[key], expected);
            } else {
                assert!(
                    diff.get(key).is_none(),
                    "omitted field must not enter audit"
                );
            }
        }
    }
    invalid_forms(db, app, &cookie, &base, &owner.list_id).await;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workflow_notices")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, baseline, "settings changes must not send notices");
}

async fn invalid_forms(
    db: &Database,
    app: &axum::Router,
    cookie: &str,
    base: &str,
    id: &listmngr_core::ListId,
) {
    let before = serde_json::to_value(db.lists().get(id).await.unwrap()).unwrap();
    let audits: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(db.pool())
        .await
        .unwrap();
    for key in ["send_welcome_message", "send_goodbye_message"] {
        for value in [
            "", "1", "0", "yes", "on", "True", "FALSE", "null", "%20true", "false%20",
        ] {
            assert!(
                call(
                    app,
                    "POST",
                    list_settings::URL,
                    cookie,
                    &format!("{base}&{key}={value}")
                )
                .await
                .status()
                .is_client_error()
            );
        }
        for value in ["true", "false"] {
            assert!(
                call(
                    app,
                    "POST",
                    list_settings::URL,
                    cookie,
                    &format!("{base}&{key}=true&{key}={value}")
                )
                .await
                .status()
                .is_client_error()
            );
        }
    }
    assert_eq!(
        serde_json::to_value(db.lists().get(id).await.unwrap()).unwrap(),
        before
    );
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(after, audits);
}

#[tokio::test]
async fn owner_notice_form_labels_and_current_selections() {
    let (db, app) = fixture().await;
    user(&db, "notice-ui@example.com", false).await;
    member(&db, "notice-ui@example.com", MemberRole::Owner).await;
    let cookie = login_as(&app, "notice-ui@example.com").await;
    let initial = text(call(&app, "GET", list_settings::URL, &cookie, "").await).await;
    for enabled in [true, false] {
        let body = format!(
            "{}&send_welcome_message={enabled}&send_goodbye_message={enabled}",
            list_settings::form(&csrf(&initial))
        );
        assert_eq!(
            call(&app, "POST", list_settings::URL, &cookie, &body)
                .await
                .status(),
            StatusCode::SEE_OTHER
        );
        let html = text(call(&app, "GET", list_settings::URL, &cookie, "").await).await;
        for (name, label) in [
            ("send_welcome_message", "Send welcome messages"),
            ("send_goodbye_message", "Send goodbye messages"),
        ] {
            assert!(html.contains(&format!("<label for=\"{name}\">{label}</label>")));
            let options = html
                .split(&format!("<select id=\"{name}\" name=\"{name}\">"))
                .nth(1)
                .unwrap()
                .split("</select>")
                .next()
                .unwrap();
            assert!(options.contains(&format!("value=\"{enabled}\" selected")));
        }
    }
}

#[tokio::test]
async fn owner_welcome_setting_controls_new_subscription_notices() {
    let (db, app) = fixture().await;
    welcome(db, app).await;
}

pub async fn welcome(db: Database, app: axum::Router) {
    user(&db, "notice-owner@example.com", false).await;
    let owner = member(&db, "notice-owner@example.com", MemberRole::Owner).await;
    let cookie = login_as(&app, "notice-owner@example.com").await;
    let page = text(call(&app, "GET", list_settings::URL, &cookie, "").await).await;
    let base = list_settings::form(&csrf(&page));
    let baseline: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workflow_notices")
        .fetch_one(db.pool())
        .await
        .unwrap();
    for enabled in [true, false] {
        let body = format!("{base}&send_welcome_message={enabled}");
        assert_eq!(
            call(&app, "POST", list_settings::URL, &cookie, &body)
                .await
                .status(),
            StatusCode::SEE_OTHER
        );
        assert_eq!(
            db.lists()
                .get(&owner.list_id)
                .await
                .unwrap()
                .send_welcome_message,
            enabled
        );
        let email = format!("new-{enabled}@example.com");
        db.members()
            .mass(&owner.list_id, "subscribe", std::slice::from_ref(&email))
            .await
            .unwrap();
        // A rejected duplicate membership must not publish another welcome.
        assert!(matches!(
            db.members()
                .mass(&owner.list_id, "subscribe", std::slice::from_ref(&email))
                .await,
            Err(listmngr_core::Error::Conflict(_))
        ));
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workflow_notices")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(count, baseline + 1);
    }
    let recipients: Vec<String> = sqlx::query_scalar("SELECT email FROM delivery_recipients WHERE email IN ('new-true@example.com','new-false@example.com')").fetch_all(db.pool()).await.unwrap();
    assert_eq!(recipients, ["new-true@example.com"]);
}
