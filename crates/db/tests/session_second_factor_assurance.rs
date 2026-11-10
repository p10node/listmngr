//! Audit F-18: `security.require_2fa_for` is satisfied by how the session was
//! established, not by what the account has enrolled. A password-only sign-in
//! of an owner who merely has a passkey enrolled is not a two-factor session.
use listmngr_db::web_sessions::LoginOutcome;
use listmngr_db::{Database, NewUser};

const PASSWORD: &str = "a very secure fixture password";

#[tokio::test]
async fn password_login_does_not_satisfy_the_policy_for_a_passkey_owner() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let owner = db
        .users()
        .create(NewUser {
            display_name: "Root".into(),
            email: "root@example.com".into(),
            password: PASSWORD.into(),
            server_owner: true,
        })
        .await
        .unwrap();
    db.addresses()
        .verify("root@example.com", true)
        .await
        .unwrap();
    let policy = vec!["server_owner".to_owned()];
    let login = |db: Database| async move {
        let anon = db
            .create_web_session(None, None, chrono::Utc::now().timestamp_millis())
            .await
            .unwrap();
        match db
            .browser_login("root@example.com", PASSWORD, &anon)
            .await
            .unwrap()
        {
            LoginOutcome::Complete(session) => session,
            LoginOutcome::SecondFactor(_) => panic!("no TOTP is enrolled"),
        }
    };
    // Nothing enrolled: the policy gates the owner.
    let session = login(db.clone()).await;
    assert!(
        db.browser_second_factor_missing(&session, &policy)
            .await
            .unwrap()
    );
    // A passkey row exists, but this session never presented it.
    sqlx::query("INSERT INTO user_passkeys(id,user_id,name,credential_id,static_state,dynamic_state,created_at) VALUES('p1',$1,'Key','cred','s','d',1)")
        .bind(owner.id.to_string())
        .execute(db.pool())
        .await
        .unwrap();
    let session = login(db.clone()).await;
    assert!(
        db.browser_second_factor_missing(&session, &policy)
            .await
            .unwrap(),
        "a password-only session is not two-factor just because a passkey is enrolled"
    );
}
