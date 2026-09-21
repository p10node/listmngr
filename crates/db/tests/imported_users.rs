//! An account carried over from another system: its addresses, the one
//! it prefers, and no password anybody knows.
use listmngr_core::{Error, MemberRole, SubscriptionMode};
use listmngr_db::{AuditContext, Database, ImportedAddress, ImportedUser, NewList, NewMember};
use sqlx::Row;

fn account(email: &str) -> ImportedUser {
    ImportedUser {
        display_name: "Dave".into(),
        is_server_owner: false,
        locale: "en".into(),
        addresses: vec![ImportedAddress {
            email: email.into(),
            display_name: "Dave".into(),
            verified: true,
        }],
        preferred: Some(email.into()),
    }
}

async fn fixture(db: &Database) {
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    db.lists()
        .create(NewList {
            list_id: "dev.example.invalid".parse().unwrap(),
            display_name: "Dev".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
}

async fn scenario(db: &Database) {
    fixture(db).await;
    // A bare subscriber: an address with no account behind it.
    db.members()
        .subscribe_with_context(
            NewMember {
                list_id: "dev.example.invalid".parse().unwrap(),
                email: "dave@example.invalid".into(),
                role: MemberRole::Member,
                subscription_mode: SubscriptionMode::AsAddress,
                display_name: String::new(),
            },
            false,
            &AuditContext::system(),
        )
        .await
        .unwrap();
    let mut imported = account("dave@example.invalid");
    imported.addresses.push(ImportedAddress {
        email: "dave@work.invalid".into(),
        display_name: String::new(),
        verified: false,
    });
    let user = db
        .users()
        .create_imported_with_context(imported.clone(), &AuditContext::system())
        .await
        .unwrap();
    assert_eq!(user.display_name, "Dave");
    assert!(!user.is_server_owner);
    // Both addresses belong to the account; the bare one was adopted, and
    // the address Mailman had verified is verified here.
    let dave = db.addresses().get("dave@example.invalid").await.unwrap();
    assert_eq!(dave.user_id, Some(user.id));
    assert!(dave.verified_on.is_some());
    let work = db.addresses().get("dave@work.invalid").await.unwrap();
    assert_eq!(work.user_id, Some(user.id));
    assert!(work.verified_on.is_none(), "unverified stays unverified");
    assert_eq!(user.preferred_address_id, Some(dave.id));
    // The credential row exists but cannot be used: the password from the
    // other system is not carried over, so the account needs a reset.
    let row = sqlx::query("SELECT password_hash,usable FROM user_credentials WHERE user_id=$1")
        .bind(user.id.to_string())
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(row.try_get::<i64, _>("usable").unwrap(), 0);
    assert!(
        !row.try_get::<String, _>("password_hash")
            .unwrap()
            .is_empty()
    );
    // The write is audited once, with no secret in it.
    let diff: String = sqlx::query_scalar(
        "SELECT diff FROM audit_log WHERE action='user.import' AND target_id=$1",
    )
    .bind(user.id.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(diff.contains("dave@example.invalid"), "{diff}");
    assert!(diff.contains("usable_password"), "{diff}");
    assert!(
        !diff.contains("$argon2"),
        "no hash material in the trail: {diff}"
    );
    // The member that was bare now carries the account.
    let member = db
        .members()
        .roster(&"dev.example.invalid".parse().unwrap(), MemberRole::Member)
        .await
        .unwrap()
        .remove(0);
    assert_eq!(
        db.members().get(member.id).await.unwrap().address_id,
        dave.id
    );
    refusals(db).await;
}

/// What the repository refuses, leaving nothing behind.
async fn refusals(db: &Database) {
    // An address that belongs to somebody else is a conflict, and nothing
    // of the refused account is written.
    let mut other = account("dave@example.invalid");
    other.display_name = "Someone else".into();
    let refused = db
        .users()
        .create_imported_with_context(other, &AuditContext::system())
        .await;
    assert!(matches!(refused, Err(Error::Conflict(_))), "{refused:?}");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
    // An account without an address is refused too.
    let mut empty = account("nobody@example.invalid");
    empty.addresses.clear();
    empty.preferred = None;
    let refused = db
        .users()
        .create_imported_with_context(empty, &AuditContext::system())
        .await;
    assert!(matches!(refused, Err(Error::Validation(_))), "{refused:?}");
}

#[tokio::test]
async fn an_imported_account_adopts_its_addresses_and_has_no_usable_password() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    scenario(&db).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_imported_user_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("imported_users")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 2).await.unwrap();
    scenario(&db).await;
    db.pool().close().await;
    schema.drop().await.unwrap();
}
