//! Audit F-02: a repeat signup for an address nobody has proven must not
//! replace the credential of the account the first signer created, or the
//! first signer's own mailbox proof would activate a stranger's password.
use base64::Engine as _;
use listmngr_db::web_signup::Signup;
use listmngr_db::{Database, web_sessions::LoginOutcome};
use sha2::{Digest as _, Sha256};

const FIRST: &str = "the first signer password 1";
const STRANGER: &str = "the stranger chose this one 2";
const TOKEN: [u8; 32] = [7; 32];

fn signup(password: &str, name: &str) -> Signup {
    Signup {
        email: "victim@example.net".into(),
        display_name: name.into(),
        password: password.into(),
        language: "en".into(),
    }
}

async fn login(db: &Database, password: &str) -> bool {
    let anon = db
        .create_web_session(None, None, chrono::Utc::now().timestamp_millis())
        .await
        .unwrap();
    matches!(
        db.browser_login("victim@example.net", password, &anon)
            .await,
        Ok(LoginOutcome::Complete(_))
    )
}

#[tokio::test]
async fn repeat_signup_does_not_replace_an_unproven_credential() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    let anon = db.create_web_session(None, None, now).await.unwrap();
    db.browser_signup(&anon, &signup(FIRST, "Victim"), now)
        .await
        .unwrap();
    // Pin the one mailed token to a value this test knows.
    let digest = format!("{:x}", Sha256::digest(TOKEN));
    sqlx::query("UPDATE account_tokens SET token_hash=$1")
        .bind(digest)
        .execute(db.pool())
        .await
        .unwrap();
    let stranger = db.create_web_session(None, None, now).await.unwrap();
    db.browser_signup(&stranger, &signup(STRANGER, "Mallory"), now + 1)
        .await
        .unwrap();
    let name: String = sqlx::query_scalar("SELECT display_name FROM users")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(name, "Victim", "a stranger's signup edits nothing");
    // The victim proves their own mailbox with the token they were mailed.
    let verifier = db.create_web_session(None, None, now).await.unwrap();
    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(TOKEN);
    db.browser_verify_address(&verifier, &token, now + 2)
        .await
        .unwrap();
    assert!(login(&db, FIRST).await, "the first signer's password works");
    assert!(
        !login(&db, STRANGER).await,
        "the stranger's password must not sign in"
    );
}
