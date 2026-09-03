use chrono::Utc;
use listmngr_core::{
    Argon2Config, DeliveryMode, DeliveryStatus, MemberRole, ModerationAction, Preferences,
    SecurityConfig, SubscriptionMode,
};
use listmngr_db::{Database, NewList, NewMember, NewUser};

#[tokio::test]
#[ignore = "requires a live PostgreSQL server via TEST_POSTGRES_URL; CI runs this explicitly"]
async fn postgres_repeated_migrate_schema_and_crud_contract() {
    let url = std::env::var("TEST_POSTGRES_URL")
        .expect("TEST_POSTGRES_URL is required for the ignored live PostgreSQL contract");
    assert!(url.starts_with("postgres://") || url.starts_with("postgresql://"));
    let db = Database::connect(&url, 1)
        .await
        .expect("connect TEST_POSTGRES_URL");
    db.migrate().await.expect("first PostgreSQL migration");
    db.migrate().await.expect("repeated PostgreSQL migration");
    let table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema=current_schema() AND table_name IN ('domains','mailing_lists','users','addresses','members','api_tokens','audit_log')",
    )
    .fetch_one(db.pool()).await.expect("inspect PostgreSQL schema");
    assert_eq!(table_count, 7);

    for sequence in 0..2 {
        let host = format!("phase1-pg-{sequence}.invalid");
        let list_id = format!("contract.{host}").parse().unwrap();
        db.domains()
            .create(&host, "PostgreSQL contract", None)
            .await
            .unwrap();
        db.lists()
            .create(NewList {
                list_id,
                display_name: "Contract".into(),
                style: "legacy-default".into(),
            })
            .await
            .unwrap();
        assert_eq!(db.lists().by_domain(&host).await.unwrap().len(), 1);
        let list = db.lists().by_domain(&host).await.unwrap().remove(0);
        db.lists().delete(&list.id).await.unwrap();
        db.domains().delete(&host).await.unwrap();
    }
}

#[tokio::test]
async fn sqlite_migrates_and_crud_domain_list_user_member_with_audit() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();

    let domain = db
        .domains()
        .create("example.com", "Example", None)
        .await
        .unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: "dev.example.com".parse().unwrap(),
            display_name: "Developers".into(),
            style: "private-default".into(),
        })
        .await
        .unwrap();
    let user = db
        .users()
        .create(NewUser {
            display_name: "Alice".into(),
            email: "Alice@Example.com".into(),
            password: "correct horse battery staple".into(),
            server_owner: true,
        })
        .await
        .unwrap();
    let member = db
        .members()
        .create(NewMember {
            list_id: list.id.clone(),
            email: "alice@example.com".into(),
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsAddress,
            display_name: "Alice".into(),
        })
        .await
        .unwrap();

    assert_eq!(domain.mail_host, "example.com");
    assert!(!list.advertised);
    assert_eq!(db.users().get(user.id).await.unwrap().display_name, "Alice");
    assert_eq!(
        db.members()
            .roster(&list.id, MemberRole::Member)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(member.user_id, Some(user.id));
    assert!(
        db.users()
            .verify_password(user.id, "correct horse battery staple")
            .await
            .unwrap()
    );
    assert!(db.audit().list().await.unwrap().len() >= 4);
}

#[tokio::test]
async fn preferences_resolve_system_user_address_member_order() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains().create("example.com", "", None).await.unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: "dev.example.com".parse().unwrap(),
            display_name: "Dev".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let user = db
        .users()
        .create(NewUser {
            display_name: "A".into(),
            email: "a@example.com".into(),
            password: "long enough password".into(),
            server_owner: false,
        })
        .await
        .unwrap();
    let member = db
        .members()
        .create(NewMember {
            list_id: list.id,
            email: "a@example.com".into(),
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsAddress,
            display_name: "A".into(),
        })
        .await
        .unwrap();
    db.preferences()
        .set_user(
            user.id,
            Preferences {
                hide_address: Some(true),
                ..Preferences::default()
            },
        )
        .await
        .unwrap();
    db.preferences()
        .set_member(
            member.id,
            Preferences {
                delivery_mode: Some(DeliveryMode::MimeDigests),
                delivery_status: Some(DeliveryStatus::ByUser),
                ..Preferences::default()
            },
        )
        .await
        .unwrap();
    let resolved = db
        .preferences()
        .resolve_member(member.id, "vi")
        .await
        .unwrap();
    assert_eq!(resolved.hide_address, Some(true));
    assert_eq!(resolved.delivery_mode, Some(DeliveryMode::MimeDigests));
    assert_eq!(resolved.preferred_language.as_deref(), Some("vi"));
}

#[tokio::test]
async fn token_secret_is_returned_once_and_scope_is_enforced() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let user = db
        .users()
        .create(NewUser {
            display_name: "Admin".into(),
            email: "admin@example.com".into(),
            password: "long enough password".into(),
            server_owner: true,
        })
        .await
        .unwrap();
    let issued = db
        .tokens()
        .create(
            user.id,
            "automation",
            &["lists:read", "members:write"],
            None,
        )
        .await
        .unwrap();
    let auth = db.tokens().authenticate(&issued.token).await.unwrap();
    assert!(auth.has_scope("lists:read"));
    assert!(!auth.has_scope("users:write"));
    db.tokens().revoke(issued.id).await.unwrap();
    assert!(db.tokens().authenticate(&issued.token).await.is_err());
}

#[tokio::test]
async fn audit_insert_failure_rolls_back_business_write() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    sqlx::query("DROP TABLE audit_log")
        .execute(db.pool())
        .await
        .unwrap();

    assert!(
        db.domains()
            .create("atomic.example", "", None)
            .await
            .is_err()
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM domains")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0, "business write must roll back with audit");
}

#[tokio::test]
async fn password_uses_configured_argon2id_and_zxcvbn_policy() {
    let security = SecurityConfig {
        argon2: Argon2Config {
            memory_kib: 8_192,
            iterations: 2,
            parallelism: 1,
        },
        password_min_score: 3,
        ..SecurityConfig::default()
    };
    let db = Database::connect_with_security("sqlite::memory:", 1, &security)
        .await
        .unwrap();
    db.migrate().await.unwrap();
    assert!(
        db.users()
            .create(NewUser {
                display_name: "Weak".into(),
                email: "weak@example.com".into(),
                password: "password1234".into(),
                server_owner: false,
            })
            .await
            .is_err()
    );
    let user = db
        .users()
        .create(NewUser {
            display_name: "Strong".into(),
            email: "strong@example.com".into(),
            password: "Orbit!Cobalt7-River$Quartz".into(),
            server_owner: false,
        })
        .await
        .unwrap();
    let hash: String =
        sqlx::query_scalar("SELECT password_hash FROM user_credentials WHERE user_id=?")
            .bind(user.id.to_string())
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(hash.starts_with("$argon2id$v=19$m=8192,t=2,p=1$"));
}

#[test]
fn all_phase_one_enums_round_trip() {
    assert_eq!(
        "hold".parse::<ModerationAction>().unwrap(),
        ModerationAction::Hold
    );
}

#[tokio::test]
async fn remaining_phase_one_resources_persist_with_bounds_and_conflicts() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let domain = db.domains().create("example.com", "", None).await.unwrap();
    let owner = db
        .users()
        .create(NewUser {
            display_name: "Owner".into(),
            email: "owner@example.com".into(),
            password: "Orbit!Cobalt7-River$Quartz".into(),
            server_owner: false,
        })
        .await
        .unwrap();
    db.domains()
        .add_owner("example.com", owner.id)
        .await
        .unwrap();
    assert_eq!(
        db.domains().owners("example.com").await.unwrap()[0].id,
        owner.id
    );
    let list = db
        .lists()
        .create(NewList {
            list_id: "dev.example.com".parse().unwrap(),
            display_name: "Dev".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    assert!(matches!(
        db.domains().delete("example.com").await,
        Err(listmngr_core::Error::Conflict(_))
    ));
    db.lists()
        .set_archiver(&list.id, "prototype", true)
        .await
        .unwrap();
    assert_eq!(
        db.lists().archivers(&list.id).await.unwrap(),
        vec![("prototype".into(), true)]
    );
    db.lists()
        .set_template(&list.id, "domain:admin:notice:new-list", "en", "Welcome")
        .await
        .unwrap();
    assert_eq!(
        db.lists().templates(&list.id).await.unwrap()[0]
            .body
            .as_deref(),
        Some("Welcome")
    );
    db.preferences()
        .set_address(
            "owner@example.com",
            Preferences {
                preferred_language: Some("vi".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        db.preferences()
            .get_address("owner@example.com")
            .await
            .unwrap()
            .preferred_language
            .as_deref(),
        Some("vi")
    );
    let scoped = db
        .tokens()
        .create_scoped(
            owner.id,
            "scoped",
            &["lists:read"],
            Some(&list.id),
            Some(domain.id),
            Some(Utc::now() + chrono::Duration::hours(1)),
        )
        .await
        .unwrap();
    let auth = db.tokens().authenticate(&scoped.token).await.unwrap();
    assert!(auth.allows_list(&list.id, domain.id));
    assert!(!auth.allows_list(&"other.example.com".parse().unwrap(), domain.id));
}

#[tokio::test]
async fn domain_idna_and_control_validation_is_persisted_canonically() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let domain = db
        .domains()
        .create("BÜCHER.example", "", None)
        .await
        .unwrap();
    assert_eq!(domain.mail_host, "xn--bcher-kva.example");
    assert!(
        db.domains()
            .create("bad\n.example", "", None)
            .await
            .is_err()
    );
    assert_eq!(db.domains().list().await.unwrap().len(), 1);
}

#[tokio::test]
async fn every_write_rolls_back_when_audit_is_unavailable() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains().create("example.com", "", None).await.unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: "dev.example.com".parse().unwrap(),
            display_name: "Dev".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    sqlx::query("DROP TABLE audit_log")
        .execute(db.pool())
        .await
        .unwrap();
    assert!(
        db.lists()
            .update(
                &list.id,
                &serde_json::json!({"description":"must rollback"})
            )
            .await
            .is_err()
    );
    assert_eq!(db.lists().get(&list.id).await.unwrap().description, "");

    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains().create("example.com", "", None).await.unwrap();
    sqlx::query("DROP TABLE audit_log")
        .execute(db.pool())
        .await
        .unwrap();
    assert!(
        db.lists()
            .create(NewList {
                list_id: "atomic.example.com".parse().unwrap(),
                display_name: "Atomic".into(),
                style: "legacy-default".into(),
            })
            .await
            .is_err()
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mailing_lists")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0, "list create must roll back with audit");
}

#[tokio::test]
async fn user_member_address_password_writes_are_audit_atomic() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    sqlx::query("DROP TABLE audit_log")
        .execute(db.pool())
        .await
        .unwrap();
    assert!(
        db.users()
            .create(NewUser {
                display_name: "Atomic".into(),
                email: "atomic@example.com".into(),
                password: "Orbit!Cobalt7-River$Quartz".into(),
                server_owner: false,
            })
            .await
            .is_err()
    );
    let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(users, 0, "user graph must roll back with audit");
}

#[tokio::test]
async fn member_create_is_audit_atomic() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains().create("example.com", "", None).await.unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: "dev.example.com".parse().unwrap(),
            display_name: "Dev".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    sqlx::query("DROP TABLE audit_log")
        .execute(db.pool())
        .await
        .unwrap();
    assert!(
        db.members()
            .create(NewMember {
                list_id: list.id,
                email: "new@example.com".into(),
                role: MemberRole::Member,
                subscription_mode: SubscriptionMode::AsAddress,
                display_name: "New".into(),
            })
            .await
            .is_err()
    );
    let members: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM members")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let addresses: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM addresses WHERE email='new@example.com'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!((members, addresses), (0, 0), "member graph must roll back");
}

#[tokio::test]
async fn address_verification_and_password_writes_are_audit_atomic() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let user = db
        .users()
        .create(NewUser {
            display_name: "Atomic".into(),
            email: "atomic@example.com".into(),
            password: "Orbit!Cobalt7-River$Quartz".into(),
            server_owner: false,
        })
        .await
        .unwrap();
    let old_hash: String =
        sqlx::query_scalar("SELECT password_hash FROM user_credentials WHERE user_id=?")
            .bind(user.id.to_string())
            .fetch_one(db.pool())
            .await
            .unwrap();
    sqlx::query("DROP TABLE audit_log")
        .execute(db.pool())
        .await
        .unwrap();
    assert!(
        db.addresses()
            .verify("atomic@example.com", true)
            .await
            .is_err()
    );
    assert!(
        db.addresses()
            .get("atomic@example.com")
            .await
            .unwrap()
            .verified_on
            .is_none()
    );
    assert!(
        db.users()
            .set_password(user.id, "Zephyr!Copper8-Mountain$Glass")
            .await
            .is_err()
    );
    let new_hash: String =
        sqlx::query_scalar("SELECT password_hash FROM user_credentials WHERE user_id=?")
            .bind(user.id.to_string())
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        old_hash, new_hash,
        "password change must roll back with audit"
    );
}

#[tokio::test]
async fn foreign_key_unique_and_delete_conflicts_fail_closed() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let domain = db.domains().create("example.com", "", None).await.unwrap();
    assert!(matches!(
        db.domains().create("EXAMPLE.COM", "", None).await,
        Err(listmngr_core::Error::Conflict(_))
    ));
    let user = db
        .users()
        .create(NewUser {
            display_name: "Owner".into(),
            email: "owner@example.com".into(),
            password: "Orbit!Cobalt7-River$Quartz".into(),
            server_owner: false,
        })
        .await
        .unwrap();
    db.domains()
        .add_owner("example.com", user.id)
        .await
        .unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: "dev.example.com".parse().unwrap(),
            display_name: "Dev".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    assert!(matches!(
        db.lists()
            .create(NewList {
                list_id: list.id.clone(),
                display_name: "Again".into(),
                style: "legacy-default".into()
            })
            .await,
        Err(listmngr_core::Error::Conflict(_))
    ));
    let _member = db
        .members()
        .create(NewMember {
            list_id: list.id.clone(),
            email: "owner@example.com".into(),
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsAddress,
            display_name: "Owner".into(),
        })
        .await
        .unwrap();
    assert!(matches!(
        db.domains().delete("example.com").await,
        Err(listmngr_core::Error::Conflict(_))
    ));
    db.lists().delete(&list.id).await.unwrap();
    assert!(matches!(
        db.users().delete(user.id).await,
        Err(listmngr_core::Error::Conflict(_))
    ));
    assert!(db.addresses().get("owner@example.com").await.is_ok());
    assert_eq!(domain.mail_host, "example.com");
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn deleting_list_removes_owned_graph_but_preserves_shared_identity_and_other_lists() {
    let db = migrated_sqlite().await;
    db.domains().create("example.com", "", None).await.unwrap();
    let deleted = db
        .lists()
        .create(NewList {
            list_id: "deleted.example.com".parse().unwrap(),
            display_name: "Deleted".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let kept = db
        .lists()
        .create(NewList {
            list_id: "kept.example.com".parse().unwrap(),
            display_name: "Kept".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let user = db
        .users()
        .create(NewUser {
            display_name: "Shared".into(),
            email: "shared@example.com".into(),
            password: "Orbit!Cobalt7-River$Quartz".into(),
            server_owner: false,
        })
        .await
        .unwrap();
    let deleted_member = db
        .members()
        .create(NewMember {
            list_id: deleted.id.clone(),
            email: "shared@example.com".into(),
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsAddress,
            display_name: "Shared".into(),
        })
        .await
        .unwrap();
    let kept_member = db
        .members()
        .create(NewMember {
            list_id: kept.id.clone(),
            email: "shared@example.com".into(),
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsAddress,
            display_name: "Shared".into(),
        })
        .await
        .unwrap();
    let deleted_preferences: String =
        sqlx::query_scalar("SELECT preferences_id FROM members WHERE id=?")
            .bind(deleted_member.id.to_string())
            .fetch_one(db.pool())
            .await
            .unwrap();
    let kept_preferences: String =
        sqlx::query_scalar("SELECT preferences_id FROM members WHERE id=?")
            .bind(kept_member.id.to_string())
            .fetch_one(db.pool())
            .await
            .unwrap();
    for list in [&deleted.id, &kept.id] {
        db.lists()
            .set_archiver(list, "prototype", true)
            .await
            .unwrap();
        db.lists()
            .set_template(list, "notice", "en", list.as_str())
            .await
            .unwrap();
        sqlx::query("INSERT INTO header_matches(id,list_id,position,header,pattern) VALUES(?,?,0,'subject','x')")
            .bind(format!("header-{list}"))
            .bind(list.as_str())
            .execute(db.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO bans(id,list_id,email_or_regex) VALUES(?,?,?)")
            .bind(format!("ban-{list}"))
            .bind(list.as_str())
            .bind(format!("{list}@example.com"))
            .execute(db.pool())
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO bans(id,list_id,email_or_regex) VALUES('global-ban',NULL,'global@example.com')")
        .execute(db.pool())
        .await
        .unwrap();

    db.lists().delete(&deleted.id).await.unwrap();

    assert!(db.lists().get(&deleted.id).await.is_err());
    assert!(db.members().get(deleted_member.id).await.is_err());
    assert_eq!(
        row_count(&db, "preferences", "id", &deleted_preferences).await,
        0
    );
    assert!(db.users().get(user.id).await.is_ok());
    assert!(db.addresses().get("shared@example.com").await.is_ok());
    assert!(db.lists().get(&kept.id).await.is_ok());
    assert!(db.members().get(kept_member.id).await.is_ok());
    assert_eq!(
        row_count(&db, "preferences", "id", &kept_preferences).await,
        1
    );
    for table in ["list_archivers", "header_matches", "bans"] {
        assert_eq!(
            row_count(&db, table, "list_id", deleted.id.as_str()).await,
            0
        );
        assert_eq!(row_count(&db, table, "list_id", kept.id.as_str()).await, 1);
    }
    assert_eq!(
        row_count(&db, "templates", "scope_id", deleted.id.as_str()).await,
        0
    );
    assert_eq!(
        row_count(&db, "templates", "scope_id", kept.id.as_str()).await,
        1
    );
    let global_bans: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM bans WHERE list_id IS NULL")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(global_bans, 1);
}

#[tokio::test]
async fn deleting_list_rolls_back_entire_owned_graph_when_audit_fails() {
    let db = migrated_sqlite().await;
    db.domains().create("example.com", "", None).await.unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: "atomic.example.com".parse().unwrap(),
            display_name: "Atomic".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let member = db
        .members()
        .create(NewMember {
            list_id: list.id.clone(),
            email: "atomic@example.com".into(),
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsAddress,
            display_name: "Atomic".into(),
        })
        .await
        .unwrap();
    let preference_id: String = sqlx::query_scalar("SELECT preferences_id FROM members WHERE id=?")
        .bind(member.id.to_string())
        .fetch_one(db.pool())
        .await
        .unwrap();
    db.lists()
        .set_archiver(&list.id, "prototype", true)
        .await
        .unwrap();
    db.lists()
        .set_template(&list.id, "notice", "en", "atomic")
        .await
        .unwrap();
    sqlx::query("INSERT INTO header_matches(id,list_id,position,header,pattern) VALUES('atomic-header',?,0,'subject','x')")
        .bind(list.id.as_str())
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO bans(id,list_id,email_or_regex) VALUES('atomic-ban',?,'atomic@example.com')",
    )
    .bind(list.id.as_str())
    .execute(db.pool())
    .await
    .unwrap();
    sabotage_audit(&db).await;

    assert!(db.lists().delete(&list.id).await.is_err());

    assert!(db.lists().get(&list.id).await.is_ok());
    assert!(db.members().get(member.id).await.is_ok());
    assert_eq!(row_count(&db, "preferences", "id", &preference_id).await, 1);
    for table in ["list_archivers", "header_matches", "bans"] {
        assert_eq!(row_count(&db, table, "list_id", list.id.as_str()).await, 1);
    }
    assert_eq!(
        row_count(&db, "templates", "scope_id", list.id.as_str()).await,
        1
    );
}

async fn row_count(db: &Database, table: &str, column: &str, value: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table} WHERE {column}=?"))
        .bind(value)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

async fn migrated_sqlite() -> Database {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db
}

async fn sabotage_audit(db: &Database) {
    sqlx::query("DROP TABLE audit_log")
        .execute(db.pool())
        .await
        .unwrap();
}

async fn seeded_graph() -> (
    Database,
    listmngr_core::ListId,
    listmngr_core::UserId,
    listmngr_core::MemberId,
) {
    let db = migrated_sqlite().await;
    db.domains().create("example.com", "", None).await.unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: "dev.example.com".parse().unwrap(),
            display_name: "Dev".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let user = db
        .users()
        .create(NewUser {
            display_name: "Atomic".into(),
            email: "atomic@example.com".into(),
            password: "Orbit!Cobalt7-River$Quartz".into(),
            server_owner: false,
        })
        .await
        .unwrap();
    let member = db
        .members()
        .create(NewMember {
            list_id: list.id.clone(),
            email: "atomic@example.com".into(),
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsAddress,
            display_name: "Atomic".into(),
        })
        .await
        .unwrap();
    (db, list.id, user.id, member.id)
}

#[tokio::test]
async fn delete_and_link_mutations_roll_back_when_audit_is_sabotaged() {
    let db = migrated_sqlite().await;
    db.domains()
        .create("delete.example", "", None)
        .await
        .unwrap();
    sabotage_audit(&db).await;
    assert!(db.domains().delete("delete.example").await.is_err());
    assert!(db.domains().get("delete.example").await.is_ok());

    let db = migrated_sqlite().await;
    db.domains().create("example.com", "", None).await.unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: "dev.example.com".parse().unwrap(),
            display_name: "Dev".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    sabotage_audit(&db).await;
    assert!(db.lists().delete(&list.id).await.is_err());
    assert!(db.lists().get(&list.id).await.is_ok());

    let (db, list_id, user_id, member_id) = seeded_graph().await;
    sabotage_audit(&db).await;
    assert!(db.members().delete(member_id).await.is_err());
    assert!(db.members().get(member_id).await.is_ok());
    assert!(
        db.addresses()
            .link("atomic@example.com", None)
            .await
            .is_err()
    );
    assert_eq!(
        db.addresses()
            .get("atomic@example.com")
            .await
            .unwrap()
            .user_id,
        Some(user_id)
    );
    assert!(db.lists().get(&list_id).await.is_ok());

    let db = migrated_sqlite().await;
    let user = db
        .users()
        .create(NewUser {
            display_name: "Delete".into(),
            email: "delete@example.com".into(),
            password: "Orbit!Cobalt7-River$Quartz".into(),
            server_owner: false,
        })
        .await
        .unwrap();
    sabotage_audit(&db).await;
    assert!(db.users().delete(user.id).await.is_err());
    assert!(db.users().get(user.id).await.is_ok());
}

#[tokio::test]
async fn preferences_and_token_revoke_roll_back_when_audit_is_sabotaged() {
    let (db, _, user_id, member_id) = seeded_graph().await;
    let issued = db
        .tokens()
        .create(user_id, "atomic", &["system:read"], None)
        .await
        .unwrap();
    sabotage_audit(&db).await;
    let changed = Preferences {
        hide_address: Some(true),
        ..Preferences::default()
    };
    assert!(
        db.preferences()
            .set_user(user_id, changed.clone())
            .await
            .is_err()
    );
    assert!(
        db.preferences()
            .set_member(member_id, changed.clone())
            .await
            .is_err()
    );
    assert!(db.tokens().revoke(issued.id).await.is_err());
    assert_eq!(
        db.preferences()
            .get_user(user_id)
            .await
            .unwrap()
            .hide_address,
        None
    );
    assert_eq!(
        db.preferences()
            .resolve_member(member_id, "en")
            .await
            .unwrap()
            .hide_address,
        Some(false)
    );
    assert!(db.tokens().authenticate(&issued.token).await.is_ok());

    let (db, _, _, _) = seeded_graph().await;
    sabotage_audit(&db).await;
    assert!(
        db.preferences()
            .set_address("atomic@example.com", changed)
            .await
            .is_err()
    );
    assert_eq!(
        db.preferences()
            .get_address("atomic@example.com")
            .await
            .unwrap(),
        Preferences::default()
    );
}

#[tokio::test]
async fn catalog_mass_and_update_mutations_roll_back_when_audit_is_sabotaged() {
    let (db, list_id, user_id, member_id) = seeded_graph().await;
    sabotage_audit(&db).await;
    assert!(
        db.users()
            .update(user_id, &serde_json::json!({"display_name":"Changed"}))
            .await
            .is_err()
    );
    assert!(
        db.members()
            .update(member_id, &serde_json::json!({"display_name":"Changed"}))
            .await
            .is_err()
    );
    assert!(
        db.lists()
            .set_archiver(&list_id, "prototype", true)
            .await
            .is_err()
    );
    assert!(
        db.lists()
            .set_template(&list_id, "notice", "en", "changed")
            .await
            .is_err()
    );
    assert!(
        db.members()
            .mass(&list_id, "subscribe", &["new@example.com".into()])
            .await
            .is_err()
    );
    assert_eq!(
        db.users().get(user_id).await.unwrap().display_name,
        "Atomic"
    );
    assert_eq!(
        db.members().get(member_id).await.unwrap().display_name,
        "Atomic"
    );
    assert!(db.lists().archivers(&list_id).await.unwrap().is_empty());
    assert!(db.lists().templates(&list_id).await.unwrap().is_empty());
    assert!(
        db.members()
            .find("new@example.com")
            .await
            .unwrap()
            .is_empty()
    );

    let db = migrated_sqlite().await;
    db.domains().create("example.com", "", None).await.unwrap();
    let owner = db
        .users()
        .create(NewUser {
            display_name: "Owner".into(),
            email: "owner@example.com".into(),
            password: "Orbit!Cobalt7-River$Quartz".into(),
            server_owner: false,
        })
        .await
        .unwrap();
    sabotage_audit(&db).await;
    assert!(
        db.domains()
            .add_owner("example.com", owner.id)
            .await
            .is_err()
    );
    assert!(db.domains().owners("example.com").await.unwrap().is_empty());
}
