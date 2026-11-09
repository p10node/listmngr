//! Audit F-01: an owner's `pre_verified` mass-subscribe flag asserts delivery
//! level confirmation for the new subscription; it must never write
//! `addresses.verified_on` (the account-level mailbox proof) of an address
//! that already exists.
use listmngr_core::{MemberRole, SubscriptionMode};
use listmngr_db::web_members::MassFlags;
use listmngr_db::{Database, NewList, NewMember, NewUser};

async fn verified_on(db: &Database, email: &str) -> Option<String> {
    sqlx::query_scalar("SELECT verified_on FROM addresses WHERE email=$1")
        .bind(email)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn owner_pre_verified_never_verifies_an_existing_address() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains().create("example.com", "", None).await.unwrap();
    let public = db
        .lists()
        .create(NewList {
            list_id: "public.example.com".parse().unwrap(),
            display_name: "Public".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let user = db
        .users()
        .create(NewUser {
            display_name: "Actor".into(),
            email: "actor@example.com".into(),
            password: "a very secure fixture password".into(),
            server_owner: false,
        })
        .await
        .unwrap();
    db.addresses()
        .verify("actor@example.com", true)
        .await
        .unwrap();
    db.members()
        .create(NewMember {
            list_id: public.id.clone(),
            email: "actor@example.com".into(),
            role: MemberRole::Owner,
            subscription_mode: SubscriptionMode::AsAddress,
            display_name: "Actor".into(),
        })
        .await
        .unwrap();
    // An email-only address that nobody has proven.
    let other = db
        .lists()
        .create(NewList {
            list_id: "private.example.com".parse().unwrap(),
            display_name: "Private".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    db.members()
        .create(NewMember {
            list_id: other.id,
            email: "takeover@example.com".into(),
            role: MemberRole::Owner,
            subscription_mode: SubscriptionMode::AsAddress,
            display_name: String::new(),
        })
        .await
        .unwrap();
    assert!(verified_on(&db, "takeover@example.com").await.is_none());
    let now = chrono::Utc::now().timestamp_millis();
    let anon = db.create_web_session(None, None, now).await.unwrap();
    let listmngr_db::web_sessions::LoginOutcome::Complete(session) = db
        .browser_login("actor@example.com", "a very secure fixture password", &anon)
        .await
        .unwrap()
    else {
        panic!("no second factor is enrolled")
    };
    assert_eq!(session.user_id, Some(user.id));
    let flags = MassFlags {
        pre_verified: true,
        ..MassFlags::default()
    };
    let entries = [("takeover@example.com".to_owned(), String::new())];
    let outcomes = db
        .browser_mass_subscribe(
            &session,
            &public.id,
            &entries,
            MemberRole::Nonmember,
            flags,
            now,
        )
        .await
        .unwrap();
    assert_eq!(outcomes.len(), 1);
    assert!(
        verified_on(&db, "takeover@example.com").await.is_none(),
        "a subscription-level flag must not become mailbox proof"
    );
    // A brand-new address the owner introduces still starts out verified.
    let entries = [("fresh@example.com".to_owned(), String::new())];
    db.browser_mass_subscribe(
        &session,
        &public.id,
        &entries,
        MemberRole::Nonmember,
        flags,
        now,
    )
    .await
    .unwrap();
    assert!(verified_on(&db, "fresh@example.com").await.is_some());
}
