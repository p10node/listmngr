use listmngr_core::{MemberId, MemberRole, SubscriptionMode, UserId};
use listmngr_db::{Database, NewList, NewMember, NewUser};

async fn migrated() -> Database {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db
}

#[tokio::test]
async fn repository_ids_fail_closed_when_valid_ids_are_used_for_the_wrong_resource_type() {
    let db = migrated().await;
    db.domains().create("ids.example", "", None).await.unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: "typed.ids.example".parse().unwrap(),
            display_name: "Typed IDs".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let user = db
        .users()
        .create(NewUser {
            display_name: "Typed user".into(),
            email: "typed@ids.example".into(),
            password: "Orbit!Cobalt7-River$Quartz".into(),
            server_owner: false,
        })
        .await
        .unwrap();
    let member = db
        .members()
        .create(NewMember {
            list_id: list.id,
            email: "typed@ids.example".into(),
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsUser,
            display_name: "Typed member".into(),
        })
        .await
        .unwrap();

    let member_bits_as_user = UserId(member.id.0);
    let user_bits_as_member = MemberId(user.id.0);
    assert!(matches!(
        db.users().get(member_bits_as_user).await,
        Err(listmngr_core::Error::NotFound(_))
    ));
    assert!(matches!(
        db.members().get(user_bits_as_member).await,
        Err(listmngr_core::Error::NotFound(_))
    ));
}

#[tokio::test]
async fn passwords_over_1024_bytes_are_rejected_and_rotation_replaces_the_hash() {
    let db = migrated().await;
    let user = db
        .users()
        .create(NewUser {
            display_name: "Password rotation".into(),
            email: "rotation@example.com".into(),
            password: "Orbit!Cobalt7-River$Quartz".into(),
            server_owner: false,
        })
        .await
        .unwrap();
    let before: (String, String) = sqlx::query_as(
        "SELECT password_hash,password_updated_at FROM user_credentials WHERE user_id=?",
    )
    .bind(user.id.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();

    let oversized = "A".repeat(1025);
    assert!(matches!(
        db.users().set_password(user.id, &oversized).await,
        Err(listmngr_core::Error::Validation(message)) if message.contains("1024")
    ));
    let unchanged: (String, String) = sqlx::query_as(
        "SELECT password_hash,password_updated_at FROM user_credentials WHERE user_id=?",
    )
    .bind(user.id.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(unchanged, before);

    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let rotated = "Zephyr!Copper8-Mountain$Glass";
    db.users().set_password(user.id, rotated).await.unwrap();
    let after: (String, String) = sqlx::query_as(
        "SELECT password_hash,password_updated_at FROM user_credentials WHERE user_id=?",
    )
    .bind(user.id.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_ne!(after.0, before.0);
    assert_ne!(after.1, before.1);
    assert!(
        !db.users()
            .verify_password(user.id, "Orbit!Cobalt7-River$Quartz")
            .await
            .unwrap()
    );
    assert!(db.users().verify_password(user.id, rotated).await.unwrap());
}
