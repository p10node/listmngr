use super::{call, csrf, fixture, list_settings, login_as, member, text, user};
use axum::http::StatusCode;
use listmngr_core::MemberRole;
use listmngr_db::Database;

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns isolated schema and router"]
async fn postgres_emergency_controls() {
    let url = std::env::var("TEST_POSTGRES_URL").unwrap();
    let admin = Database::connect(&url, 1).await.unwrap();
    let schema = format!("web_emergency_{}", uuid::Uuid::now_v7().simple());
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
        controls(&db, &app).await;
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

#[tokio::test]
async fn owner_can_toggle_emergency_without_resetting_legacy_omissions() {
    let (db, app) = fixture().await;
    controls(&db, &app).await;
}

pub async fn controls(db: &Database, app: &axum::Router) {
    let owner = user(db, "emergency-owner@example.com", false).await;
    let role = member(db, "emergency-owner@example.com", MemberRole::Owner).await;
    let cookie = login_as(app, "emergency-owner@example.com").await;
    let page = text(call(app, "GET", list_settings::URL, &cookie, "").await).await;
    let base = list_settings::form(&csrf(&page));
    for (suffix, expected) in [
        ("&emergency=true", true),
        ("", true),
        ("&emergency=false", false),
        ("", false),
    ] {
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
        assert_eq!(
            db.lists().get(&role.list_id).await.unwrap().emergency,
            expected
        );
        let html = text(call(app, "GET", list_settings::URL, &cookie, "").await).await;
        assert!(html.contains("<label for=\"emergency\">Emergency moderation</label>"));
        let options = html
            .split("<select id=\"emergency\" name=\"emergency\">")
            .nth(1)
            .unwrap()
            .split("</select>")
            .next()
            .unwrap();
        assert!(options.contains(&format!("value=\"{expected}\" selected")));
        assert!(html.contains("not a delivery shutdown"));
    }
    let audits: Vec<String> = sqlx::query_scalar(
        "SELECT diff FROM audit_log WHERE action='list.config' AND actor_user_id=$1 ORDER BY at,id",
    )
    .bind(owner.id.to_string())
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(audits.len(), 4);
    for (raw, expected) in audits.iter().zip([Some(true), None, Some(false), None]) {
        let diff: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert_eq!(
            diff.get("emergency").and_then(serde_json::Value::as_bool),
            expected
        );
    }
    denials(db, app, &cookie, &base, &role.list_id).await;
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log WHERE action='list.config' AND actor_user_id=$1",
    )
    .bind(owner.id.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count, 4);
}

async fn denials(
    db: &Database,
    app: &axum::Router,
    cookie: &str,
    base: &str,
    id: &listmngr_core::ListId,
) {
    let before = serde_json::to_value(db.lists().get(id).await.unwrap()).unwrap();
    for value in [
        "",
        "1",
        "0",
        "yes",
        "on",
        "True",
        "FALSE",
        "null",
        "%20true",
        "false%20",
        "true&emergency=false",
        "true&emergency=true",
    ] {
        assert!(
            call(
                app,
                "POST",
                list_settings::URL,
                cookie,
                &format!("{base}&emergency={value}")
            )
            .await
            .status()
            .is_client_error()
        );
        assert_eq!(
            serde_json::to_value(db.lists().get(id).await.unwrap()).unwrap(),
            before
        );
    }
}
