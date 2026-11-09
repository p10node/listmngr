//! Audit F-10: adding an address nobody owns links nothing until the mailbox
//! owner uses the mailed token. Before the proof the address must not widen
//! any reader (own export), block the mailbox owner's signup, or become the
//! requester's by someone else's later verification of a different account.
use base64::Engine as _;
use listmngr_core::{MemberRole, SubscriptionMode};
use listmngr_db::web_signup::Signup;
use listmngr_db::{Database, NewList, NewMember, NewUser, web_sessions::LoginOutcome};
use sha2::{Digest as _, Sha256};

const PASSWORD: &str = "a very secure fixture password";
const VICTIM: &str = "victim@example.org";

struct World {
    db: Database,
    mallory: listmngr_db::web_sessions::WebSession,
    mallory_id: listmngr_core::UserId,
}

async fn world() -> World {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains().create("example.com", "", None).await.unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: "private.example.com".parse().unwrap(),
            display_name: "Private".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    // An email-only subscriber nobody has claimed.
    db.members()
        .create(NewMember {
            list_id: list.id,
            email: VICTIM.into(),
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsAddress,
            display_name: "Victim".into(),
        })
        .await
        .unwrap();
    let mallory = db
        .users()
        .create(NewUser {
            display_name: "Mallory".into(),
            email: "mallory@example.net".into(),
            password: PASSWORD.into(),
            server_owner: false,
        })
        .await
        .unwrap();
    db.addresses()
        .verify("mallory@example.net", true)
        .await
        .unwrap();
    let anon = db.create_web_session(None, None, now()).await.unwrap();
    let LoginOutcome::Complete(session) = db
        .browser_login("mallory@example.net", PASSWORD, &anon)
        .await
        .unwrap()
    else {
        panic!("no second factor is enrolled")
    };
    World {
        db,
        mallory: session,
        mallory_id: mallory.id,
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

async fn owner_of(db: &Database, email: &str) -> (Option<String>, Option<String>) {
    sqlx::query_as("SELECT user_id,verified_on FROM addresses WHERE email=$1")
        .bind(email)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

/// Replace the digest of every unconsumed token with the one of `secret`.
async fn pin_token(db: &Database, secret: [u8; 32]) -> String {
    sqlx::query("UPDATE account_tokens SET token_hash=$1 WHERE consumed_at IS NULL")
        .bind(format!("{:x}", Sha256::digest(secret)))
        .execute(db.pool())
        .await
        .unwrap();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret)
}

#[tokio::test]
async fn claim_links_nothing_before_the_mailbox_proof() {
    let w = world().await;
    w.db.browser_add_address(&w.mallory, VICTIM, "en", now())
        .await
        .unwrap();
    assert_eq!(owner_of(&w.db, VICTIM).await, (None, None));
    let export = w.db.browser_export_own(&w.mallory).await.unwrap();
    assert!(
        !export.to_string().contains("private.example.com"),
        "the own export must not name another person's memberships: {export}"
    );
    // A brand-new address is not linked either.
    w.db.browser_add_address(&w.mallory, "fresh@example.org", "en", now())
        .await
        .unwrap();
    assert_eq!(owner_of(&w.db, "fresh@example.org").await, (None, None));
}

#[tokio::test]
async fn mailbox_owner_signup_is_not_blocked_and_voids_the_claim() {
    let w = world().await;
    w.db.browser_add_address(&w.mallory, VICTIM, "en", now())
        .await
        .unwrap();
    let claim = pin_token(&w.db, [3; 32]).await;
    let anon = w.db.create_web_session(None, None, now()).await.unwrap();
    w.db.browser_signup(
        &anon,
        &Signup {
            email: VICTIM.into(),
            display_name: "Victim".into(),
            password: PASSWORD.into(),
            language: "en".into(),
        },
        now() + 1,
    )
    .await
    .unwrap();
    let (owner, verified) = owner_of(&w.db, VICTIM).await;
    assert!(owner.is_some() && verified.is_none());
    assert_ne!(owner, Some(w.mallory_id.to_string()));
    // The signup mailed its own token, and the requester's is void.
    let live: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM account_tokens WHERE consumed_at IS NULL AND claimant_user_id IS NULL",
    )
    .fetch_one(w.db.pool())
    .await
    .unwrap();
    assert_eq!(live, 1);
    let browser = w.db.create_web_session(None, None, now()).await.unwrap();
    assert!(
        w.db.browser_verify_address(&browser, &claim, now() + 2)
            .await
            .is_err()
    );
    let (owner, verified) = owner_of(&w.db, VICTIM).await;
    assert!(verified.is_none());
    assert_ne!(owner, Some(w.mallory_id.to_string()));
}

#[tokio::test]
async fn token_use_links_an_unowned_address_to_the_claimant() {
    let w = world().await;
    w.db.browser_add_address(&w.mallory, VICTIM, "en", now())
        .await
        .unwrap();
    let token = pin_token(&w.db, [4; 32]).await;
    let browser = w.db.create_web_session(None, None, now()).await.unwrap();
    w.db.browser_verify_address(&browser, &token, now() + 1)
        .await
        .unwrap();
    let (owner, verified) = owner_of(&w.db, VICTIM).await;
    assert_eq!(owner, Some(w.mallory_id.to_string()));
    assert!(verified.is_some());
}

#[tokio::test]
async fn token_never_moves_an_address_another_account_took_meanwhile() {
    let w = world().await;
    w.db.browser_add_address(&w.mallory, VICTIM, "en", now())
        .await
        .unwrap();
    let token = pin_token(&w.db, [5; 32]).await;
    let other =
        w.db.users()
            .create(NewUser {
                display_name: "Other".into(),
                email: "other@example.net".into(),
                password: PASSWORD.into(),
                server_owner: false,
            })
            .await
            .unwrap();
    sqlx::query("UPDATE addresses SET user_id=$1 WHERE email=$2")
        .bind(other.id.to_string())
        .bind(VICTIM)
        .execute(w.db.pool())
        .await
        .unwrap();
    let reader = w.db.create_web_session(None, None, now()).await.unwrap();
    assert!(
        w.db.browser_verify_address(&reader, &token, now() + 1)
            .await
            .is_err()
    );
    let (owner, verified) = owner_of(&w.db, VICTIM).await;
    assert_eq!(owner, Some(other.id.to_string()));
    assert!(verified.is_none());
}
