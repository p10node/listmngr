//! `UserRepo::create_verified_with_context`: an account whose address the
//! operator vouches for (`listmngr user create`) signs in at once. The
//! REST path, `create`, keeps leaving the address unverified as Mailman's
//! `POST /users` does.
use listmngr_core::{Error, MemberRole, Result, SubscriptionMode};
use listmngr_db::{
    AuditContext, Database, NewList, NewMember, NewUser, web_sessions::LoginOutcome,
};

const PASSWORD: &str = "Orbit!Cobalt7-River$Quartz";

async fn fixture(url: &str) -> Database {
    let db = Database::connect(url, 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains().create("example.com", "", None).await.unwrap();
    db.lists()
        .create(NewList {
            list_id: "test.example.com".parse().unwrap(),
            display_name: "Test".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    db
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

async fn login(db: &Database, email: &str) -> Result<LoginOutcome> {
    let anonymous = db.create_web_session(None, None, now_ms()).await.unwrap();
    db.browser_login(email, PASSWORD, &anonymous).await
}

async fn create_audit_diff(db: &Database, user: &str) -> serde_json::Value {
    let diff: String = sqlx::query_scalar(
        "SELECT diff FROM audit_log WHERE action='user.create' AND target_type='user' AND target_id=$1",
    )
    .bind(user)
    .fetch_one(db.pool())
    .await
    .unwrap();
    serde_json::from_str(&diff).unwrap()
}

/// A new address: created verified, in the same transaction as the
/// account, and the one audit event says so.
async fn operator_created_account_signs_in(db: &Database) {
    let owner = db
        .users()
        .create_verified_with_context(
            NewUser {
                display_name: "Admin".into(),
                email: "Admin@example.com".into(),
                password: PASSWORD.into(),
                server_owner: true,
            },
            &AuditContext::system(),
        )
        .await
        .unwrap();
    let address = db.addresses().get("admin@example.com").await.unwrap();
    assert_eq!(address.user_id, Some(owner.id));
    assert!(
        address.verified_on.is_some(),
        "the operator vouched for the address"
    );
    assert_eq!(owner.preferred_address_id, Some(address.id));
    let diff = create_audit_diff(db, &owner.id.to_string()).await;
    assert_eq!(diff["email"], "admin@example.com");
    assert_eq!(diff["verified"], true);
    assert!(
        matches!(
            login(db, "admin@example.com").await,
            Ok(LoginOutcome::Complete(_))
        ),
        "the first account signs in with its password"
    );
    let owners: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM users u WHERE u.is_server_owner=1 AND EXISTS (SELECT 1 FROM addresses a WHERE a.user_id=u.id AND a.verified_on IS NOT NULL)",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(owners, 1, "the erase guard counts this server owner");
}

/// A bare address — a subscriber nobody owns — is adopted and verified.
async fn bare_address_is_adopted_and_verified(db: &Database) {
    db.members()
        .create(NewMember {
            list_id: "test.example.com".parse().unwrap(),
            email: "Bare@example.com".into(),
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsAddress,
            display_name: "Bare".into(),
        })
        .await
        .unwrap();
    let before = db.addresses().get("bare@example.com").await.unwrap();
    assert!(before.user_id.is_none() && before.verified_on.is_none());
    let account = db
        .users()
        .create_verified_with_context(
            NewUser {
                display_name: "Bare".into(),
                email: "bare@example.com".into(),
                password: PASSWORD.into(),
                server_owner: false,
            },
            &AuditContext::system(),
        )
        .await
        .unwrap();
    let after = db.addresses().get("bare@example.com").await.unwrap();
    assert_eq!(after.id, before.id, "the row is adopted, not replaced");
    assert_eq!(after.user_id, Some(account.id));
    assert!(after.verified_on.is_some());
    assert!(matches!(
        login(db, "bare@example.com").await,
        Ok(LoginOutcome::Complete(_))
    ));
}

/// The REST path is unchanged: an account created the Mailman way owns
/// an address nobody has proven, and cannot sign in until someone does.
async fn rest_created_account_stays_unproven(db: &Database) {
    let member = db
        .users()
        .create(NewUser {
            display_name: "Member".into(),
            email: "member@example.com".into(),
            password: PASSWORD.into(),
            server_owner: false,
        })
        .await
        .unwrap();
    let unproven = db.addresses().get("member@example.com").await.unwrap();
    assert_eq!(unproven.user_id, Some(member.id));
    assert!(unproven.verified_on.is_none());
    let diff = create_audit_diff(db, &member.id.to_string()).await;
    assert_eq!(diff["email"], "member@example.com");
    assert_eq!(diff["verified"], false);
    assert!(matches!(
        login(db, "member@example.com").await,
        Err(Error::Authentication)
    ));
}

async fn scenario(url: &str) {
    let db = fixture(url).await;
    operator_created_account_signs_in(&db).await;
    bare_address_is_adopted_and_verified(&db).await;
    rest_created_account_stays_unproven(&db).await;
    db.pool().close().await;
}

#[tokio::test]
async fn sqlite_operator_created_account_signs_in() {
    scenario("sqlite::memory:").await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_user_create_verified_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("user_create_verified")
        .await
        .unwrap();
    scenario(&schema.url).await;
    schema.drop().await.unwrap();
}
