//! A token bound to a list (or domain) must not block deleting it:
//! `api_tokens.list_id`/`domain_id` restrict, so the delete removes the rows.
use listmngr_db::{Database, NewList, NewUser};

const PASSWORD: &str = "Orbit!Cobalt7-River$Quartz";

#[tokio::test]
async fn bound_tokens_do_not_block_list_and_domain_deletion() {
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
    let user = db
        .users()
        .create(NewUser {
            display_name: "Owner".into(),
            email: "owner@example.com".into(),
            password: PASSWORD.into(),
            server_owner: false,
        })
        .await
        .unwrap();
    let live = db
        .tokens()
        .create_scoped(
            user.id,
            "live",
            &["lists:read"],
            Some(&list.id),
            Some(domain.id),
            None,
        )
        .await
        .unwrap();
    let revoked = db
        .tokens()
        .create_scoped(
            user.id,
            "revoked",
            &["lists:read"],
            Some(&list.id),
            Some(domain.id),
            None,
        )
        .await
        .unwrap();
    db.tokens().revoke(revoked.id).await.unwrap();
    let domain_only = db
        .tokens()
        .create_scoped(user.id, "domain", &["lists:read"], None, Some(domain.id), None)
        .await
        .unwrap();
    let unbound = db
        .tokens()
        .create(user.id, "unbound", &["system:read"], None)
        .await
        .unwrap();

    db.lists().delete(&list.id).await.unwrap();
    assert!(db.tokens().authenticate(&live.token).await.is_err());
    db.domains().delete("example.com").await.unwrap();
    assert!(db.tokens().authenticate(&domain_only.token).await.is_err());
    // Tokens that were never bound survive.
    assert!(db.tokens().authenticate(&unbound.token).await.is_ok());
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM api_tokens")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(left, 1);
}
