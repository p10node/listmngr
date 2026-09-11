use super::{
    call, csrf, fixture,
    list_settings::{URL, form},
    login_as, member, text, user,
};
use axum::http::StatusCode;
use listmngr_core::MemberRole;
use listmngr_db::Database;

#[tokio::test]
async fn owner_can_save_posting_limits() {
    let (db, app) = fixture().await;
    user(&db, "limits-owner@example.com", false).await;
    let owner = member(&db, "limits-owner@example.com", MemberRole::Owner).await;
    let cookie = login_as(&app, "limits-owner@example.com").await;
    let html = text(call(&app, "GET", URL, &cookie, "").await).await;
    let body = format!(
        "{}&max_message_size=128&max_num_recipients=12",
        form(&csrf(&html))
    );
    assert_eq!(
        call(&app, "POST", URL, &cookie, &body).await.status(),
        StatusCode::SEE_OTHER
    );
    let saved = db.lists().get(&owner.list_id).await.unwrap();
    assert_eq!(saved.max_message_size, 128);
    assert_eq!(saved.max_num_recipients, 12);
}

#[tokio::test]
async fn owner_form_exposes_current_posting_limits_and_units() {
    let (db, app) = fixture().await;
    user(&db, "limits-owner@example.com", false).await;
    let owner = member(&db, "limits-owner@example.com", MemberRole::Owner).await;
    db.lists()
        .update(
            &owner.list_id,
            &serde_json::json!({"max_message_size":256,"max_num_recipients":17}),
        )
        .await
        .unwrap();
    let cookie = login_as(&app, "limits-owner@example.com").await;
    let html = text(call(&app, "GET", URL, &cookie, "").await).await;
    assert!(html.contains("<label for=\"max_message_size\">Maximum message size (KiB)</label>"));
    assert!(html.contains("name=\"max_message_size\" value=\"256\""));
    assert!(
        html.contains("<label for=\"max_num_recipients\">To/Cc recipient hold threshold</label>")
    );
    assert!(html.contains("name=\"max_num_recipients\" value=\"17\""));
    assert!(html.contains("at or above"));
    assert!(html.contains("0 disables"));
}

#[tokio::test]
async fn posting_limits_preserve_omissions_validate_bounds_and_audit_exact_patch() {
    let (db, app) = fixture().await;
    values(&db, &app).await;
}

async fn values(db: &Database, app: &axum::Router) {
    let user = user(db, "values-owner@example.com", false).await;
    let owner = member(db, "values-owner@example.com", MemberRole::Owner).await;
    let cookie = login_as(app, "values-owner@example.com").await;
    let html = text(call(app, "GET", URL, &cookie, "").await).await;
    let legacy = form(&csrf(&html));
    db.lists().update(&owner.list_id, &serde_json::json!({"max_message_size":83,"max_num_recipients":7,"subject_prefix":"[preserved]"})).await.unwrap();
    for (suffix, size, recipients) in [
        ("", 83, 7),
        ("&max_message_size=0", 0, 7),
        ("&max_num_recipients=2147483647", 0, 2_147_483_647),
        (
            "&max_message_size=2147483647&max_num_recipients=0",
            2_147_483_647,
            0,
        ),
        ("&max_message_size=19&max_num_recipients=3", 19, 3),
    ] {
        assert_eq!(
            call(app, "POST", URL, &cookie, &format!("{legacy}{suffix}"))
                .await
                .status(),
            StatusCode::SEE_OTHER
        );
        let saved = db.lists().get(&owner.list_id).await.unwrap();
        assert_eq!(
            (saved.max_message_size, saved.max_num_recipients),
            (size, recipients)
        );
        assert_eq!(saved.subject_prefix, "[preserved]");
        let detail: String = sqlx::query_scalar("SELECT diff FROM audit_log WHERE action='list.config' AND actor_user_id=$1 ORDER BY id DESC LIMIT 1")
            .bind(user.id.to_string()).fetch_one(db.pool()).await.unwrap();
        let detail: serde_json::Value = serde_json::from_str(&detail).unwrap();
        audit_patch(&detail, suffix, size, recipients);
    }
    let before = serde_json::to_value(db.lists().get(&owner.list_id).await.unwrap()).unwrap();
    for name in ["max_message_size", "max_num_recipients"] {
        for value in [
            "",
            "-1",
            "1.5",
            "true",
            "bogus",
            "2147483648",
            "4294967296",
            "1&max_message_size=2&max_num_recipients=2",
        ] {
            assert!(
                call(
                    app,
                    "POST",
                    URL,
                    &cookie,
                    &format!("{legacy}&{name}={value}")
                )
                .await
                .status()
                .is_client_error()
            );
            assert_eq!(
                serde_json::to_value(db.lists().get(&owner.list_id).await.unwrap()).unwrap(),
                before
            );
        }
    }
    let audits: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log WHERE action='list.config' AND actor_user_id=$1",
    )
    .bind(user.id.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(audits, 5, "invalid forms must not publish audit records");
}

fn audit_patch(detail: &serde_json::Value, suffix: &str, size: u32, recipients: u32) {
    for (name, value) in [
        ("max_message_size", size),
        ("max_num_recipients", recipients),
    ] {
        if suffix.contains(name) {
            assert_eq!(detail[name], value);
        } else {
            assert!(
                detail.get(name).is_none(),
                "omission must not enter audit: {name}"
            );
        }
    }
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns isolated schema"]
async fn postgres_posting_limits_controls() {
    let url = std::env::var("TEST_POSTGRES_URL").unwrap();
    let admin = Database::connect(&url, 1).await.unwrap();
    let schema = format!("web_limits_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(admin.pool())
        .await
        .unwrap();
    let sep = if url.contains('?') { '&' } else { '?' };
    let fixture = format!("{url}{sep}options=-csearch_path%3D{schema}");
    let result = tokio::spawn(async move {
        let db = Database::connect(&fixture, 3).await.unwrap();
        db.migrate().await.unwrap();
        let (db, app) = super::seeded_fixture(db).await;
        super::list_settings::matrix(db.clone(), app.clone(), false).await;
        values(&db, &app).await;
        super::notices::welcome(db.clone(), app.clone()).await;
        super::notices::controls(&db, &app).await;
        db.pool().close().await;
    })
    .await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(admin.pool())
        .await
        .unwrap();
    admin.pool().close().await;
    result.unwrap();
}
