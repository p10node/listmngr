use super::{call, csrf, fixture, list_settings, login_as, member, text, user};
use axum::http::StatusCode;
use listmngr_core::{ListId, MemberRole};
use listmngr_db::Database;

#[tokio::test]
async fn sqlite_subject_prefix_controls() {
    let (db, app) = fixture().await;
    controls(&db, &app, true).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns isolated schema and router"]
async fn postgres_subject_prefix_controls() {
    let url = std::env::var("TEST_POSTGRES_URL").unwrap();
    let admin = Database::connect(&url, 1).await.unwrap();
    let schema = format!("web_prefix_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(admin.pool())
        .await
        .unwrap();
    let sep = if url.contains('?') { '&' } else { '?' };
    let url = format!("{url}{sep}options=-csearch_path%3D{schema}");
    let result = tokio::spawn(async move {
        let db = Database::connect(&url, 3).await.unwrap();
        db.migrate().await.unwrap();
        let (db, app) = super::seeded_fixture(db).await;
        controls(&db, &app, false).await;
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

fn form(base: &str, value: &str) -> String {
    format!(
        "{base}&{}",
        serde_urlencoded::to_string([("subject_prefix", value)]).unwrap()
    )
}

async fn snapshot(db: &Database, id: &ListId) -> (serde_json::Value, Vec<String>) {
    let list = serde_json::to_value(db.lists().get(id).await.unwrap()).unwrap();
    let audit =
        sqlx::query_scalar("SELECT diff FROM audit_log WHERE action='list.config' ORDER BY at,id")
            .fetch_all(db.pool())
            .await
            .unwrap();
    (list, audit)
}

async fn controls(db: &Database, app: &axum::Router, sqlite: bool) {
    let owner = user(db, "prefix-controls@example.com", false).await;
    let role = member(db, "prefix-controls@example.com", MemberRole::Owner).await;
    let cookie = login_as(app, "prefix-controls@example.com").await;
    let html = text(call(app, "GET", list_settings::URL, &cookie, "").await).await;
    let base = list_settings::form(&csrf(&html));
    let value = "  [Tiếng Việt + ✉]  ";
    for (provided, expected) in [
        (Some(value), value),
        (None, value),
        (Some(""), ""),
        (None, ""),
        (Some(" \t "), " \t "),
    ] {
        let body = provided.map_or_else(|| base.clone(), |v| form(&base, v));
        assert_eq!(
            call(app, "POST", list_settings::URL, &cookie, &body)
                .await
                .status(),
            StatusCode::SEE_OTHER
        );
        assert_eq!(
            db.lists().get(&role.list_id).await.unwrap().subject_prefix,
            expected
        );
    }
    let audits: Vec<String> = sqlx::query_scalar("SELECT diff FROM audit_log WHERE action='list.config' AND actor_user_id=$1 AND target_id=$2 ORDER BY at,id")
        .bind(owner.id.to_string()).bind(role.list_id.as_str()).fetch_all(db.pool()).await.unwrap();
    assert_eq!(audits.len(), 5);
    for (raw, expected) in audits
        .iter()
        .zip([Some(value), None, Some(""), None, Some(" \t ")])
    {
        let diff: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert_eq!(
            diff.get("subject_prefix")
                .and_then(serde_json::Value::as_str),
            expected
        );
        if expected.is_none() {
            assert!(diff.get("subject_prefix").is_none());
        }
    }
    denials(db, app, &cookie, &base, &role.list_id).await;
    rollback(db, app, &cookie, &base, &role.list_id, sqlite).await;
    authority(db, app, &cookie, &base, &role).await;
}

async fn denials(db: &Database, app: &axum::Router, cookie: &str, base: &str, id: &ListId) {
    let before = snapshot(db, id).await;
    for bad in [
        form(base, "bad\rprefix"),
        form(base, "bad\nprefix"),
        form(base, "\r\nBcc: victim@example.com"),
        format!("{}&subject_prefix=second", form(base, "first")),
        form(&base.replace("csrf=", "csrf=wrong"), "changed"),
    ] {
        assert!(
            call(app, "POST", list_settings::URL, cookie, &bad)
                .await
                .status()
                .is_client_error()
        );
        assert_eq!(snapshot(db, id).await, before);
    }
}

async fn rollback(
    db: &Database,
    app: &axum::Router,
    cookie: &str,
    base: &str,
    id: &ListId,
    sqlite: bool,
) {
    let before = snapshot(db, id).await;
    sqlx::query(if sqlite {
        "CREATE TRIGGER reject_prefix_audit BEFORE INSERT ON audit_log WHEN NEW.action='list.config' BEGIN SELECT RAISE(ABORT,'owned audit failure'); END"
    } else {
        "ALTER TABLE audit_log ADD CONSTRAINT reject_prefix_audit CHECK (action <> 'list.config') NOT VALID"
    }).execute(db.pool()).await.unwrap();
    let changed = form(
        &base.replace("advertised=false", "advertised=true"),
        "[changed after failure]",
    );
    assert!(
        call(app, "POST", list_settings::URL, cookie, &changed)
            .await
            .status()
            .is_server_error()
    );
    assert_eq!(snapshot(db, id).await, before);
    sqlx::query(if sqlite {
        "DROP TRIGGER reject_prefix_audit"
    } else {
        "ALTER TABLE audit_log DROP CONSTRAINT reject_prefix_audit"
    })
    .execute(db.pool())
    .await
    .unwrap();
    assert_eq!(
        call(app, "POST", list_settings::URL, cookie, &changed)
            .await
            .status(),
        StatusCode::SEE_OTHER
    );
    let saved = db.lists().get(id).await.unwrap();
    assert_eq!(saved.subject_prefix, "[changed after failure]");
    assert!(saved.advertised);
}

async fn authority(
    db: &Database,
    app: &axum::Router,
    cookie: &str,
    base: &str,
    role: &listmngr_core::Member,
) {
    let valid = form(base, "[forbidden change]");
    let before = snapshot(db, &role.list_id).await;
    let foreign_id = "private.example.com".parse().unwrap();
    let foreign_before = snapshot(db, &foreign_id).await;
    let foreign = list_settings::URL.replace("public.example.com", "private.example.com");
    for method in ["GET", "POST"] {
        assert_eq!(
            call(app, method, &foreign, cookie, &valid).await.status(),
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(snapshot(db, &foreign_id).await, foreign_before);
    sqlx::query("UPDATE members SET role='moderator' WHERE id=$1")
        .bind(role.id.to_string())
        .execute(db.pool())
        .await
        .unwrap();
    for method in ["GET", "POST"] {
        assert_eq!(
            call(app, method, list_settings::URL, cookie, &valid)
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(snapshot(db, &role.list_id).await, before);
    sqlx::query("UPDATE members SET role='owner' WHERE id=$1")
        .bind(role.id.to_string())
        .execute(db.pool())
        .await
        .unwrap();
    db.addresses()
        .verify("prefix-controls@example.com", false)
        .await
        .unwrap();
    assert_eq!(
        call(app, "POST", list_settings::URL, cookie, &valid)
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(snapshot(db, &role.list_id).await, before);
}
