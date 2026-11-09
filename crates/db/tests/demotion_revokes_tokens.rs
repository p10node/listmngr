//! Demoting a server owner must end the authority their tokens carry.
use listmngr_db::{AuditContext, Database, NewList, NewUser};

const PASSWORD: &str = "Orbit!Cobalt7-River$Quartz";

async fn owner(db: &Database, email: &str) -> listmngr_core::User {
    db.users()
        .create_verified_with_context(
            NewUser {
                display_name: email.into(),
                email: email.into(),
                password: PASSWORD.into(),
                server_owner: true,
            },
            &AuditContext::system(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn demoting_a_server_owner_revokes_their_server_level_tokens() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let domain = db.domains().create("example.com", "", None).await.unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: "test.example.com".parse().unwrap(),
            display_name: "Test".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let demoted = owner(&db, "a@example.com").await;
    let actor = owner(&db, "b@example.com").await;
    let admin = db
        .tokens()
        .create(demoted.id, "op", &["admin"], None)
        .await
        .unwrap();
    let bound = db
        .tokens()
        .create_scoped(
            demoted.id,
            "bound",
            &["lists:read"],
            Some(&list.id),
            Some(domain.id),
            None,
        )
        .await
        .unwrap();
    let other = db
        .tokens()
        .create(actor.id, "other", &["admin"], None)
        .await
        .unwrap();
    assert!(db.tokens().authenticate(&admin.token).await.is_ok());

    let session = db
        .create_web_session(Some(actor.id), None, chrono::Utc::now().timestamp_millis())
        .await
        .unwrap();
    db.browser_user_update(&session, demoted.id, "a", false)
        .await
        .unwrap();

    assert!(db.tokens().authenticate(&admin.token).await.is_err());
    assert!(db.tokens().authenticate(&bound.token).await.is_err());
    // Another account's tokens are untouched, and each revocation is audited.
    assert!(db.tokens().authenticate(&other.token).await.is_ok());
    let audited: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='token.revoke'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(audited, 2);

    // Renaming without a demotion revokes nothing.
    let kept = db
        .tokens()
        .create(actor.id, "kept", &["admin"], None)
        .await
        .unwrap();
    db.browser_user_update(&session, actor.id, "b2", true)
        .await
        .unwrap();
    assert!(db.tokens().authenticate(&kept.token).await.is_ok());
}
