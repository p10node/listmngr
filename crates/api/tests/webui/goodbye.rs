use super::*;

#[tokio::test]
async fn goodbye_browser_leave_is_private_audited_and_only_after_confirmed_post() {
    for enabled in [false, true] {
        let (db, app) = fixture().await;
        let user = user(&db, "leaver@example.com", false).await;
        let own = member(&db, "leaver@example.com", listmngr_core::MemberRole::Member).await;
        let owner = member(&db, "leaver@example.com", listmngr_core::MemberRole::Owner).await;
        let cookie = login_as(&app, "leaver@example.com").await;
        let settings = text(call(&app, "GET", list_settings::URL, &cookie, "").await).await;
        let settings_form = format!(
            "{}&send_goodbye_message={enabled}",
            list_settings::form(&csrf(&settings))
        );
        assert_eq!(
            call(&app, "POST", list_settings::URL, &cookie, &settings_form)
                .await
                .status(),
            StatusCode::SEE_OTHER
        );
        let url = format!("/web/members/{}/leave", own.id);
        let page = text(call(&app, "GET", &url, &cookie, "").await).await;
        let form = serde_urlencoded::to_string([("csrf", csrf(&page))]).unwrap();
        assert_eq!(
            call(&app, "POST", &url, &cookie, "csrf=wrong")
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
        let denied = format!("/web/members/{}/leave", owner.id);
        assert_eq!(
            call(&app, "POST", &denied, &cookie, &form).await.status(),
            StatusCode::FORBIDDEN
        );
        verify_leave_rollback(&db, &app, &cookie, &form, &own, true).await;
        assert_eq!(
            notice_count(&db).await,
            0,
            "preview, forbidden and rollback must be silent"
        );
        assert_eq!(
            call(&app, "POST", &url, &cookie, &form).await.status(),
            StatusCode::SEE_OTHER
        );
        assert_eq!(
            call(&app, "POST", &url, &cookie, &form).await.status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(notice_count(&db).await, i64::from(enabled));
        assert!(db.members().get(owner.id).await.is_ok());
        assert!(db.members().get(own.id).await.is_err());
        let audits: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='member.delete' AND target_id=$1 AND actor_user_id=$2")
            .bind(own.id.to_string()).bind(user.id.to_string()).fetch_one(db.pool()).await.unwrap();
        assert_eq!(audits, 1);
        if enabled {
            let recipients: Vec<String> =
                sqlx::query_scalar("SELECT email FROM delivery_recipients")
                    .fetch_all(db.pool())
                    .await
                    .unwrap();
            assert_eq!(recipients, ["leaver@example.com"]);
            let raw: Vec<u8> = sqlx::query_scalar("SELECT b.raw FROM workflow_notices n JOIN queue_jobs q ON q.id=n.job_id JOIN messages m ON m.id=q.message_id JOIN message_blobs b ON b.store_key=m.store_key").fetch_one(db.pool()).await.unwrap();
            assert!(
                String::from_utf8(raw)
                    .unwrap()
                    .contains("Subject: You have been unsubscribed from the")
            );
        }
    }
}

async fn notice_count(db: &Database) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM workflow_notices")
        .fetch_one(db.pool())
        .await
        .unwrap()
}
