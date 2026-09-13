use super::{call, csrf, fixture, login_as, member, seeded_fixture, text, user};
use axum::http::StatusCode;
use listmngr_core::{ArchivePolicy, MemberRole, ModerationAction};
use listmngr_db::Database;

pub const URL: &str = "/web/lists/public.example.com/settings";
pub fn form(token: &str) -> String {
    serde_urlencoded::to_string([
        ("csrf", token),
        ("display_name", "Owner <name> & \"quoted\""),
        ("description", "Description </textarea><script>bad</script>"),
        ("advertised", "false"),
        ("default_member_action", "hold"),
        ("default_nonmember_action", "reject"),
        ("archive_policy", "private"),
    ])
    .unwrap()
}

fn limited_form(token: &str) -> String {
    format!(
        "{}&max_message_size=128&max_num_recipients=12&send_welcome_message=true&send_goodbye_message=true&emergency=true",
        form(token)
    )
}

#[tokio::test]
async fn owner_list_settings() {
    let (db, app) = fixture().await;
    matrix(db, app, true).await;
}

#[tokio::test]
#[ignore = "requires NEW empty disposable WEBUI_SETTINGS_POSTGRES_URL"]
async fn postgres_owner_list_settings() {
    let db = Database::connect(&std::env::var("WEBUI_SETTINGS_POSTGRES_URL").unwrap(), 3)
        .await
        .unwrap();
    db.migrate().await.unwrap();
    let (db, app) = seeded_fixture(db).await;
    matrix(db, app, false).await;
}

pub async fn matrix(db: Database, app: axum::Router, sqlite: bool) {
    let owner = user(&db, "settings-owner@example.com", false).await;
    let role = member(&db, "settings-owner@example.com", MemberRole::Owner).await;
    let cookie = login_as(&app, "settings-owner@example.com").await;
    let response = call(&app, "GET", URL, &cookie, "").await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = text(response).await;
    assert!(html.contains("&lt;script&gt;"));
    assert!(!html.contains("<script>"));
    let token = csrf(&html);
    let index = text(call(&app, "GET", "/web/admin", &cookie, "").await).await;
    assert!(index.contains(URL));
    // A stale browser form must not restore fields changed since rendering.
    db.lists()
        .update(
            &role.list_id,
            &serde_json::json!({"subject_prefix":"[new]", "emergency":true}),
        )
        .await
        .unwrap();
    assert_eq!(
        call(&app, "POST", URL, &cookie, &limited_form(&token))
            .await
            .status(),
        StatusCode::SEE_OTHER
    );
    let saved = db.lists().get(&role.list_id).await.unwrap();
    check_saved(&saved);
    let readback = text(call(&app, "GET", URL, &cookie, "").await).await;
    assert!(readback.contains("Owner &lt;name&gt; &amp; &quot;quoted&quot;"));
    assert!(!readback.contains("<script>"));
    let audits: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='list.config' AND target_id=$1 AND actor_user_id=$2")
        .bind(role.list_id.as_str()).bind(owner.id.to_string()).fetch_one(db.pool()).await.unwrap();
    assert_eq!(audits, 1);
    controls(&db, &app, &cookie, &token, sqlite).await;
    authority(&db, &app, &cookie, &token, role.id).await;
}

fn check_saved(saved: &listmngr_core::MailingList) {
    assert_eq!(saved.display_name, "Owner <name> & \"quoted\"");
    assert_eq!(
        saved.description,
        "Description </textarea><script>bad</script>"
    );
    assert!(!saved.advertised);
    assert_eq!(saved.default_member_action, Some(ModerationAction::Hold));
    assert_eq!(
        saved.default_nonmember_action,
        Some(ModerationAction::Reject)
    );
    assert_eq!(saved.archive_policy, ArchivePolicy::Private);
    assert_eq!(saved.subject_prefix, "[new]");
    assert!(saved.emergency);
    assert_eq!(saved.max_message_size, 128);
    assert_eq!(saved.max_num_recipients, 12);
    assert!(saved.send_welcome_message);
    assert!(saved.send_goodbye_message);
}

async fn controls(db: &Database, app: &axum::Router, cookie: &str, token: &str, sqlite: bool) {
    let id = "public.example.com".parse().unwrap();
    let before = serde_json::to_value(db.lists().get(&id).await.unwrap()).unwrap();
    let valid = limited_form(token);
    for invalid in [
        limited_form("wrong"),
        valid.replace("advertised=false", "advertised=maybe"),
        valid.replace("default_member_action=hold", "default_member_action=bogus"),
        valid.replace(
            "default_nonmember_action=reject",
            "default_nonmember_action=bogus",
        ),
        valid.replace("archive_policy=private", "archive_policy=bogus"),
        format!("{valid}&emergency=false"),
        format!("{valid}&advertised=true"),
        valid.replace("advertised=false&", ""),
    ] {
        assert!(
            call(app, "POST", URL, cookie, &invalid)
                .await
                .status()
                .is_client_error()
        );
        assert_eq!(
            serde_json::to_value(db.lists().get(&id).await.unwrap()).unwrap(),
            before
        );
    }
    origin_controls(app, cookie, &valid).await;
    sqlx::query(if sqlite {
        "CREATE TRIGGER reject_settings_audit BEFORE INSERT ON audit_log WHEN NEW.action='list.config' BEGIN SELECT RAISE(ABORT,'owned audit failure'); END"
    } else {
        "ALTER TABLE audit_log ADD CONSTRAINT reject_settings_audit CHECK (action <> 'list.config') NOT VALID"
    }).execute(db.pool()).await.unwrap();
    let changed = valid
        .replace("advertised=false", "advertised=true")
        .replace("max_message_size=128", "max_message_size=9")
        .replace("max_num_recipients=12", "max_num_recipients=3")
        .replace("send_welcome_message=true", "send_welcome_message=false")
        .replace("send_goodbye_message=true", "send_goodbye_message=false")
        .replace("emergency=true", "emergency=false");
    assert!(
        call(app, "POST", URL, cookie, &changed)
            .await
            .status()
            .is_server_error()
    );
    assert_eq!(
        serde_json::to_value(db.lists().get(&id).await.unwrap()).unwrap(),
        before
    );
    sqlx::query(if sqlite {
        "DROP TRIGGER reject_settings_audit"
    } else {
        "ALTER TABLE audit_log DROP CONSTRAINT reject_settings_audit"
    })
    .execute(db.pool())
    .await
    .unwrap();
    let audits: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log WHERE action='list.config' AND actor_user_id IS NOT NULL",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(audits, 1);
}

async fn origin_controls(app: &axum::Router, cookie: &str, form: &str) {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt as _;
    for origin in [None, Some("null"), Some("http://foreign.example")] {
        let mut request = Request::builder()
            .method("POST")
            .uri(URL)
            .header("cookie", cookie)
            .header("host", "localhost")
            .header("content-type", "application/x-www-form-urlencoded");
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::from(form.to_owned())).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}

async fn authority(
    db: &Database,
    app: &axum::Router,
    cookie: &str,
    token: &str,
    role: listmngr_core::MemberId,
) {
    let valid = limited_form(token);
    let foreign = URL.replace("public.example.com", "private.example.com");
    for method in ["GET", "POST"] {
        assert_eq!(
            call(app, method, &foreign, cookie, &valid).await.status(),
            StatusCode::FORBIDDEN
        );
    }
    sqlx::query("UPDATE members SET role='moderator' WHERE id=$1")
        .bind(role.to_string())
        .execute(db.pool())
        .await
        .unwrap();
    for method in ["GET", "POST"] {
        assert_eq!(
            call(app, method, URL, cookie, &valid).await.status(),
            StatusCode::FORBIDDEN
        );
    }
    sqlx::query("UPDATE members SET role='owner' WHERE id=$1")
        .bind(role.to_string())
        .execute(db.pool())
        .await
        .unwrap();
    db.addresses()
        .verify("settings-owner@example.com", false)
        .await
        .unwrap();
    assert_eq!(
        call(app, "POST", URL, cookie, &valid).await.status(),
        StatusCode::FORBIDDEN
    );
    db.addresses()
        .verify("settings-owner@example.com", true)
        .await
        .unwrap();
    db.users()
        .set_password(
            db.users()
                .get_by_email("settings-owner@example.com")
                .await
                .unwrap()
                .id,
            "another very secure password",
        )
        .await
        .unwrap();
    assert!(
        call(app, "POST", URL, cookie, &valid)
            .await
            .status()
            .is_client_error()
    );
    server_authority(db, app, &foreign).await;
}

async fn server_authority(db: &Database, app: &axum::Router, foreign: &str) {
    user(db, "settings-server@example.com", true).await;
    let server = login_as(app, "settings-server@example.com").await;
    let html = text(call(app, "GET", foreign, &server, "").await).await;
    let server_form = limited_form(&csrf(&html));
    assert_eq!(
        call(app, "POST", foreign, &server, &server_form)
            .await
            .status(),
        StatusCode::SEE_OTHER
    );
    db.addresses()
        .verify("settings-server@example.com", false)
        .await
        .unwrap();
    assert_eq!(
        call(app, "POST", foreign, &server, &server_form)
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
}
