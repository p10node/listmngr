//! Audit F-16: seating a domain owner by email resolves the account that has
//! proven that mailbox, never one that merely holds an unverified link.
use listmngr_core::Error;
use listmngr_db::{Database, NewUser};

async fn user(db: &Database, email: &str, owner: bool) -> listmngr_core::User {
    db.users()
        .create(NewUser {
            display_name: "U".into(),
            email: email.into(),
            password: "a very secure fixture password".into(),
            server_owner: owner,
        })
        .await
        .unwrap()
}

async fn owners(db: &Database) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM domain_owners")
        .fetch_one(db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn domain_owner_grant_requires_a_verified_link() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains().create("example.com", "", None).await.unwrap();
    user(&db, "root@example.com", true).await;
    db.addresses()
        .verify("root@example.com", true)
        .await
        .unwrap();
    let mallory = user(&db, "mallory@example.net", false).await;
    // Mallory holds an unverified link to an address she has not proven.
    sqlx::query("INSERT INTO addresses(id,email,original_email,display_name,user_id,registered_on) VALUES('00000000-0000-7000-8000-000000000001','victim@example.org','victim@example.org','',$1,'2026-01-01T00:00:00Z')")
        .bind(mallory.id.to_string())
        .execute(db.pool())
        .await
        .unwrap();
    let anon = db
        .create_web_session(None, None, chrono::Utc::now().timestamp_millis())
        .await
        .unwrap();
    let listmngr_db::web_sessions::LoginOutcome::Complete(session) = db
        .browser_login("root@example.com", "a very secure fixture password", &anon)
        .await
        .unwrap()
    else {
        panic!("no second factor is enrolled")
    };
    let refused = db
        .browser_domain_owner_add(&session, "example.com", "victim@example.org")
        .await;
    assert!(matches!(refused, Err(Error::NotFound(_))), "{refused:?}");
    assert_eq!(owners(&db).await, 0);
    // Once the mailbox is proven, the grant lands on that account.
    db.addresses()
        .verify("victim@example.org", true)
        .await
        .unwrap();
    let seated = db
        .browser_domain_owner_add(&session, "example.com", "Victim@Example.org")
        .await
        .unwrap();
    assert_eq!(seated, mallory.id);
    assert_eq!(owners(&db).await, 1);
}
