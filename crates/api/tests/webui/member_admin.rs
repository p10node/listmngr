use super::{call, csrf, fixture, login_as, member, seeded_fixture, text, user};
use axum::http::StatusCode;
use listmngr_core::{MemberRole, ModerationAction};
use listmngr_db::Database;

#[tokio::test]
async fn owner_can_manage_member_posting_policy() {
    let (db, app) = fixture().await;
    matrix(db, app, true).await;
}

#[tokio::test]
#[ignore = "requires NEW empty disposable WEBUI_ADMIN_POSTGRES_URL"]
async fn postgres_owner_member_policy() {
    let db = Database::connect(&std::env::var("WEBUI_ADMIN_POSTGRES_URL").unwrap(), 3)
        .await
        .unwrap();
    db.migrate().await.unwrap();
    let (db, app) = seeded_fixture(db).await;
    matrix(db, app, false).await;
}

async fn matrix(db: Database, app: axum::Router, sqlite: bool) {
    let owner = user(&db, "owner@example.com", false).await;
    let role = member(&db, "owner@example.com", MemberRole::Owner).await;
    let target = member(&db, "poster@example.com", MemberRole::Member).await;
    db.lists()
        .update(&role.list_id, &serde_json::json!({"advertised":false}))
        .await
        .unwrap();
    let cookie = login_as(&app, "owner@example.com").await;
    check_index(&app, &cookie).await;
    let url = "/web/lists/public.example.com/members";
    let response = call(&app, "GET", url, &cookie, "").await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = text(response).await;
    assert!(html.contains("poster@example.com"));
    assert!(!html.contains("owner@example.com"));
    let token = csrf(&html);
    let edit = format!("{url}/{}/policy", target.id);
    let form = serde_urlencoded::to_string([("csrf", token.as_str()), ("action", "hold")]).unwrap();
    assert_eq!(
        call(&app, "POST", &edit, &cookie, "csrf=wrong&action=hold")
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    rollback(&db, &app, &cookie, &edit, &form, target.id, sqlite).await;
    let actions = [
        ("hold", Some(ModerationAction::Hold)),
        ("discard", Some(ModerationAction::Discard)),
        ("accept", Some(ModerationAction::Accept)),
        ("reject", Some(ModerationAction::Reject)),
        ("defer", Some(ModerationAction::Defer)),
        ("default", None),
    ];
    for (value, expected) in actions {
        let form =
            serde_urlencoded::to_string([("csrf", token.as_str()), ("action", value)]).unwrap();
        assert_eq!(
            call(&app, "POST", &edit, &cookie, &form).await.status(),
            StatusCode::SEE_OTHER
        );
        assert_eq!(
            db.members().get(target.id).await.unwrap().moderation_action,
            expected
        );
    }
    let audits: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='member.update' AND target_id=$1 AND actor_user_id=$2").bind(target.id.to_string()).bind(owner.id.to_string()).fetch_one(db.pool()).await.unwrap();
    assert_eq!(audits, i64::try_from(actions.len()).unwrap());
    denials(&db, &app, &cookie, &token, &edit, role.id).await;
}

async fn check_index(app: &axum::Router, cookie: &str) {
    let index = call(app, "GET", "/web/admin", cookie, "").await;
    assert_eq!(index.status(), StatusCode::OK);
    let index = text(index).await;
    assert!(index.contains("/web/lists/public.example.com/members"));
    assert!(!index.contains("private.example.com"));
}

async fn rollback(
    db: &Database,
    app: &axum::Router,
    cookie: &str,
    edit: &str,
    form: &str,
    target: listmngr_core::MemberId,
    sqlite: bool,
) {
    sqlx::query(if sqlite { "CREATE TRIGGER reject_admin_audit BEFORE INSERT ON audit_log WHEN NEW.action='member.update' BEGIN SELECT RAISE(ABORT,'owned audit failure'); END" } else { "ALTER TABLE audit_log ADD CONSTRAINT reject_admin_audit CHECK (action <> 'member.update') NOT VALID" }).execute(db.pool()).await.unwrap();
    let response = call(app, "POST", edit, cookie, form).await;
    assert!(!response.status().is_success() && !response.status().is_redirection());
    assert_eq!(
        db.members().get(target).await.unwrap().moderation_action,
        None
    );
    sqlx::query(if sqlite {
        "DROP TRIGGER reject_admin_audit"
    } else {
        "ALTER TABLE audit_log DROP CONSTRAINT reject_admin_audit"
    })
    .execute(db.pool())
    .await
    .unwrap();
}

async fn denials(
    db: &Database,
    app: &axum::Router,
    cookie: &str,
    token: &str,
    edit: &str,
    role: listmngr_core::MemberId,
) {
    let form = serde_urlencoded::to_string([("csrf", token), ("action", "hold")]).unwrap();
    let wrong_list = edit.replace("public.example.com", "private.example.com");
    assert_eq!(
        call(app, "POST", &wrong_list, cookie, &form).await.status(),
        StatusCode::FORBIDDEN
    );
    let wrong_role = format!("/web/lists/public.example.com/members/{role}/policy");
    assert_eq!(
        call(app, "POST", &wrong_role, cookie, &form).await.status(),
        StatusCode::NOT_FOUND
    );
    let invalid = serde_urlencoded::to_string([("csrf", token), ("action", "invalid")]).unwrap();
    assert_eq!(
        call(app, "POST", edit, cookie, &invalid).await.status(),
        StatusCode::BAD_REQUEST
    );
    sqlx::query("UPDATE members SET role='moderator' WHERE id=$1")
        .bind(role.to_string())
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(
        call(
            app,
            "GET",
            "/web/lists/public.example.com/members",
            cookie,
            ""
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(app, "POST", edit, cookie, &form).await.status(),
        StatusCode::FORBIDDEN
    );
}
