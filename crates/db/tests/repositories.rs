use chrono::Utc;
use listmngr_core::{
    Argon2Config, DeliveryMode, DeliveryStatus, MemberRole, ModerationAction, Preferences,
    SecurityConfig, SubscriptionMode,
};
use listmngr_db::{AuditContext, Database, NewList, NewMember, NewUser};

const PHASE_ONE_SCHEMA: &str = include_str!("fixtures/phase1-schema.snapshot");

fn canonical_schema(mut lines: Vec<String>) -> String {
    lines.sort();
    format!("{}\n", lines.join("\n"))
}

async fn sqlite_semantic_schema(db: &Database) -> String {
    let columns: Vec<String> = sqlx::query_scalar(
        r"SELECT 'C|' || m.name || '|' || p.name || '|' ||
        CASE WHEN upper(p.type) LIKE '%INT%' THEN 'integer'
             WHEN upper(p.type) IN ('REAL','DOUBLE','DOUBLE PRECISION','FLOAT') THEN 'real'
             ELSE 'text' END || '|' ||
        CASE WHEN p.[notnull]=1 OR p.pk>0 THEN 'required' ELSE 'optional' END || '|' ||
        CASE replace(replace(COALESCE(p.dflt_value,''),'''',''),'::text','')
             WHEN '' THEN '-' ELSE replace(replace(COALESCE(p.dflt_value,''),'''',''),'::text','') END
        FROM sqlite_master m JOIN pragma_table_info(m.name) p
        WHERE m.type='table' AND m.name NOT LIKE 'sqlite_%' AND m.name NOT LIKE '_sqlx_%'",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    let foreign_keys: Vec<String> = sqlx::query_scalar(
        r"SELECT 'F|' || m.name || '|' || f.[from] || '|' || f.[table] || '|' || f.[to] || '|' || lower(f.on_delete)
        FROM sqlite_master m JOIN pragma_foreign_key_list(m.name) f
        WHERE m.type='table' AND m.name NOT LIKE 'sqlite_%' AND m.name NOT LIKE '_sqlx_%'",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    let unique_keys: Vec<String> = sqlx::query_scalar(
        r"SELECT 'U|' || indexes.table_name || '|' || group_concat(indexes.column_name, ',')
        FROM (
          SELECT m.name AS table_name, il.name AS index_name, ii.name AS column_name, ii.seqno
          FROM sqlite_master m
          JOIN pragma_index_list(m.name) il
          JOIN pragma_index_info(il.name) ii
          WHERE m.type='table' AND m.name NOT LIKE 'sqlite_%' AND m.name NOT LIKE '_sqlx_%'
            AND il.[unique]=1
          ORDER BY m.name, il.name, ii.seqno
        ) indexes
        GROUP BY indexes.table_name, indexes.index_name
        UNION
        SELECT 'U|' || m.name || '|' || p.name
        FROM sqlite_master m JOIN pragma_table_info(m.name) p
        WHERE m.type='table' AND m.name NOT LIKE 'sqlite_%' AND m.name NOT LIKE '_sqlx_%'
          AND p.pk=1 AND upper(p.type)='INTEGER'
          AND NOT EXISTS (SELECT 1 FROM pragma_table_info(m.name) pk WHERE pk.pk>1)",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    canonical_schema(
        columns
            .into_iter()
            .chain(foreign_keys)
            .chain(unique_keys)
            .collect(),
    )
}

async fn postgres_semantic_schema(db: &Database) -> String {
    let columns: Vec<String> = sqlx::query_scalar(
        r"SELECT 'C|' || c.table_name || '|' || c.column_name || '|' ||
        CASE WHEN c.data_type IN ('smallint','integer','bigint') THEN 'integer'
             WHEN c.data_type IN ('real','double precision','numeric','decimal') THEN 'real'
             ELSE 'text' END || '|' ||
        CASE c.is_nullable WHEN 'NO' THEN 'required' ELSE 'optional' END || '|' ||
        COALESCE(NULLIF(regexp_replace(regexp_replace(c.column_default, '^''(.*)''::[a-z ]+$', '\1'), '^\((.*)\)$', '\1'), ''), '-')
        FROM information_schema.columns c
        WHERE c.table_schema=current_schema() AND c.table_name NOT LIKE '\_sqlx\_%' ESCAPE '\'",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    let foreign_keys: Vec<String> = sqlx::query_scalar(
        r"SELECT 'F|' || tc.table_name || '|' || kcu.column_name || '|' || ccu.table_name || '|' || ccu.column_name || '|' || lower(rc.delete_rule)
        FROM information_schema.table_constraints tc
        JOIN information_schema.key_column_usage kcu ON tc.constraint_name=kcu.constraint_name AND tc.constraint_schema=kcu.constraint_schema
        JOIN information_schema.constraint_column_usage ccu ON tc.constraint_name=ccu.constraint_name AND tc.constraint_schema=ccu.constraint_schema
        JOIN information_schema.referential_constraints rc ON tc.constraint_name=rc.constraint_name AND tc.constraint_schema=rc.constraint_schema
        WHERE tc.constraint_schema=current_schema() AND tc.constraint_type='FOREIGN KEY'",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    let unique_keys: Vec<String> = sqlx::query_scalar(
        r"SELECT 'U|' || tbl.relname || '|' || string_agg(att.attname,',' ORDER BY ord.n)
        FROM pg_index idx JOIN pg_class tbl ON tbl.oid=idx.indrelid
        JOIN pg_namespace ns ON ns.oid=tbl.relnamespace
        JOIN LATERAL unnest(idx.indkey) WITH ORDINALITY ord(attnum,n) ON true
        JOIN pg_attribute att ON att.attrelid=tbl.oid AND att.attnum=ord.attnum
        WHERE ns.nspname=current_schema() AND idx.indisunique AND tbl.relname NOT LIKE '\_sqlx\_%' ESCAPE '\'
        GROUP BY tbl.relname,idx.indexrelid",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    canonical_schema(
        columns
            .into_iter()
            .chain(foreign_keys)
            .chain(unique_keys)
            .collect(),
    )
}

fn expected_phase_one_schema() -> String {
    // Preserve the frozen Phase 1 corpus; additive migrations have their own corpus.
    canonical_schema(
        PHASE_ONE_SCHEMA
            .lines()
            .chain(include_str!("fixtures/phase2-queue-schema.snapshot").lines())
            .chain(include_str!("fixtures/phase2-mail-policy-schema.snapshot").lines())
            .chain(include_str!("fixtures/phase2-delivery-attempt-schema.snapshot").lines())
            .chain(include_str!("fixtures/phase4-composed-schema.snapshot").lines())
            .chain(include_str!("fixtures/owner-schema.snapshot").lines())
            .chain(include_str!("fixtures/message-size-schema.snapshot").lines())
            .chain(include_str!("fixtures/dmarc-munge-schema.snapshot").lines())
            .chain(include_str!("fixtures/smtp-bounces-schema.snapshot").lines())
            .chain(include_str!("fixtures/smtp-failure-metadata-schema.snapshot").lines())
            .chain(include_str!("fixtures/welcome-schema.snapshot").lines())
            .chain(include_str!("fixtures/goodbye-schema.snapshot").lines())
            .chain(include_str!("fixtures/bounce-score-schema.snapshot").lines())
            .chain(include_str!("fixtures/bounce-disable-schema.snapshot").lines())
            .chain(include_str!("fixtures/bounce-disable-notice-schema.snapshot").lines())
            .chain(include_str!("fixtures/bounce-increment-notice-schema.snapshot").lines())
            .chain(include_str!("fixtures/bounce-maintenance-schema.snapshot").lines())
            .chain(include_str!("fixtures/dsn-issuance-schema.snapshot").lines())
            .chain(include_str!("fixtures/dsn-plan-schema.snapshot").lines())
            .chain(include_str!("fixtures/recipient-limit-schema.snapshot").lines())
            .chain(include_str!("fixtures/moderation-rules-schema.snapshot").lines())
            .chain(include_str!("fixtures/posting-pipeline-schema.snapshot").lines())
            .chain(include_str!("fixtures/hold-notices-schema.snapshot").lines())
            .chain(include_str!("fixtures/alter-messages-schema.snapshot").lines())
            .chain(include_str!("fixtures/topics-schema.snapshot").lines())
            .chain(include_str!("fixtures/site-secrets-schema.snapshot").lines())
            .chain(include_str!("fixtures/subscription-state-schema.snapshot").lines())
            .chain(include_str!("fixtures/subscription-invitation-schema.snapshot").lines())
            .chain(include_str!("fixtures/autoresponder-schema.snapshot").lines())
            .chain(include_str!("fixtures/bounce-probes-schema.snapshot").lines())
            .chain(include_str!("fixtures/digest-settings-schema.snapshot").lines())
            .chain(include_str!("fixtures/admin-notify-mchanges-schema.snapshot").lines())
            .chain(include_str!("fixtures/moderation-forward-schema.snapshot").lines())
            .chain(include_str!("fixtures/session-inventory-schema.snapshot").lines())
            .chain(include_str!("fixtures/account-tokens-schema.snapshot").lines())
            .chain(include_str!("fixtures/totp-schema.snapshot").lines())
            .chain(include_str!("fixtures/passkeys-schema.snapshot").lines())
            .chain(include_str!("fixtures/oidc-schema.snapshot").lines())
            .chain(include_str!("fixtures/archive-render-schema.snapshot").lines())
            .chain(include_str!("fixtures/archive-views-schema.snapshot").lines())
            .chain(include_str!("fixtures/archive-interactions-schema.snapshot").lines())
            .chain(include_str!("fixtures/archive-admin-schema.snapshot").lines())
            .chain(include_str!("fixtures/usenet-schema.snapshot").lines())
            .chain(include_str!("fixtures/webhooks-schema.snapshot").lines())
            .chain(include_str!("fixtures/posting-rate-schema.snapshot").lines())
            .map(str::to_owned)
            .collect(),
    )
}

async fn postgres_contract_user(
    db: &Database,
    email: &str,
    server_owner: bool,
) -> listmngr_core::User {
    let user = db
        .users()
        .create(NewUser {
            display_name: "PostgreSQL owner".into(),
            email: email.into(),
            password: "Orbit!Cobalt7-River$Quartz".into(),
            server_owner,
        })
        .await
        .unwrap();
    assert!(
        db.users()
            .verify_password(user.id, "Orbit!Cobalt7-River$Quartz")
            .await
            .unwrap()
    );
    assert_eq!(
        db.addresses().get(email).await.unwrap().user_id,
        Some(user.id)
    );
    assert!(
        db.addresses()
            .verify(email, true)
            .await
            .unwrap()
            .verified_on
            .is_some()
    );
    assert!(
        db.addresses()
            .verify(email, false)
            .await
            .unwrap()
            .verified_on
            .is_none()
    );
    user
}

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
    assert_eq!(
        postgres_semantic_schema(&db).await,
        expected_phase_one_schema()
    );

    let run_id = uuid::Uuid::now_v7();
    for sequence in 0..2 {
        let host = format!("phase1-pg-{run_id}-{sequence}.invalid");
        let list_id: listmngr_core::ListId = format!("contract.{host}").parse().unwrap();
        db.domains()
            .create(&host, "PostgreSQL contract", None)
            .await
            .unwrap();
        db.lists()
            .create(NewList {
                list_id: list_id.clone(),
                display_name: "Contract".into(),
                style: "private-default".into(),
            })
            .await
            .unwrap();
        assert_eq!(db.lists().by_domain(&host).await.unwrap().len(), 1);
        assert!(!db.lists().get(&list_id).await.unwrap().advertised);
        db.lists()
            .set_archiver(&list_id, "prototype", true)
            .await
            .unwrap();
        db.lists()
            .set_template(&list_id, "list:user:notice:welcome", "en", "contract")
            .await
            .unwrap();

        let email = format!("owner@{host}");
        let user = postgres_contract_user(&db, &email, sequence == 0).await;

        let member = db
            .members()
            .create(NewMember {
                list_id: list_id.clone(),
                email: email.clone(),
                role: MemberRole::Owner,
                subscription_mode: SubscriptionMode::AsUser,
                display_name: "PostgreSQL owner".into(),
            })
            .await
            .unwrap();
        db.preferences()
            .set_member(
                member.id,
                Preferences {
                    delivery_mode: Some(DeliveryMode::MimeDigests),
                    ..Preferences::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(
            db.preferences()
                .resolve_member(member.id, "en")
                .await
                .unwrap()
                .delivery_mode,
            Some(DeliveryMode::MimeDigests)
        );
        assert_eq!(db.members().find(&email).await.unwrap().len(), 1);
        assert_eq!(
            db.members()
                .roster(&list_id, MemberRole::Owner)
                .await
                .unwrap()
                .len(),
            1
        );

        let token = db
            .tokens()
            .create(user.id, "PostgreSQL contract", &["admin"], None)
            .await
            .unwrap();
        assert!(
            db.tokens()
                .authenticate(&token.token)
                .await
                .unwrap()
                .has_scope("admin")
        );
        db.tokens().revoke(token.id).await.unwrap();
        assert!(db.tokens().authenticate(&token.token).await.is_err());

        db.members().delete(member.id).await.unwrap();
        db.lists().delete(&list_id).await.unwrap();
        db.users().delete(user.id).await.unwrap();
        db.domains().delete(&host).await.unwrap();
    }
    assert!(!db.audit().list().await.unwrap().is_empty());
}

#[tokio::test]
async fn sqlite_phase_one_schema_matches_complete_semantic_snapshot() {
    let db = migrated_sqlite().await;
    assert_eq!(
        sqlite_semantic_schema(&db).await,
        expected_phase_one_schema()
    );
}

#[tokio::test]
async fn sqlite_phase_one_schema_constraints_are_semantic() {
    let db = migrated_sqlite().await;
    db.domains().create("example.com", "", None).await.unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: "schema.example.com".parse().unwrap(),
            display_name: "Schema".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let user = db
        .users()
        .create(NewUser {
            display_name: "Schema".into(),
            email: "schema@example.com".into(),
            password: "Orbit!Cobalt7-River$Quartz".into(),
            server_owner: false,
        })
        .await
        .unwrap();

    for statement in [
        "INSERT INTO header_matches(id,list_id,position,header,pattern) VALUES('bad-header','missing.example.com',0,'subject','x')",
        "INSERT INTO bans(id,list_id,email_or_regex) VALUES('bad-ban','missing.example.com','x')",
        "UPDATE users SET preferred_address_id='00000000-0000-0000-0000-000000000000'",
    ] {
        assert!(
            sqlx::query(statement).execute(db.pool()).await.is_err(),
            "{statement}"
        );
    }
    sqlx::query("INSERT INTO header_matches(id,list_id,position,header,pattern) VALUES('header-1',?,0,'subject','x')")
        .bind(list.id.as_str()).execute(db.pool()).await.unwrap();
    assert!(sqlx::query("INSERT INTO header_matches(id,list_id,position,header,pattern) VALUES('header-2',?,0,'from','x')").bind(list.id.as_str()).execute(db.pool()).await.is_err());
    sqlx::query(
        "INSERT INTO bans(id,list_id,email_or_regex) VALUES('ban-1',?,'blocked@example.com')",
    )
    .bind(list.id.as_str())
    .execute(db.pool())
    .await
    .unwrap();
    assert!(
        sqlx::query(
            "INSERT INTO bans(id,list_id,email_or_regex) VALUES('ban-2',?,'blocked@example.com')"
        )
        .bind(list.id.as_str())
        .execute(db.pool())
        .await
        .is_err()
    );

    let address_id: String =
        sqlx::query_scalar("SELECT preferred_address_id FROM users WHERE id=?")
            .bind(user.id.to_string())
            .fetch_one(db.pool())
            .await
            .unwrap();
    sqlx::query("UPDATE addresses SET user_id=NULL WHERE id=?")
        .bind(&address_id)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM addresses WHERE id=?")
        .bind(address_id)
        .execute(db.pool())
        .await
        .unwrap();
    let preferred: Option<String> =
        sqlx::query_scalar("SELECT preferred_address_id FROM users WHERE id=?")
            .bind(user.id.to_string())
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(
        preferred.is_none(),
        "preferred address delete must SET NULL"
    );

    sqlx::query("DELETE FROM mailing_lists WHERE list_id=?")
        .bind(list.id.as_str())
        .execute(db.pool())
        .await
        .unwrap();
    for table in ["header_matches", "bans"] {
        assert_eq!(row_count(&db, table, "list_id", list.id.as_str()).await, 0);
    }
    assert!(
        sqlx::query(
            "INSERT INTO bans(id,list_id,email_or_regex) VALUES('global',NULL,'global@example.com')"
        )
        .execute(db.pool())
        .await
        .is_ok()
    );
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
async fn member_sync_is_role_scoped_atomic_and_preserves_existing_members() {
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
    let owner = db
        .members()
        .create(NewMember {
            list_id: list.id.clone(),
            email: "owner@example.com".into(),
            role: MemberRole::Owner,
            subscription_mode: SubscriptionMode::AsUser,
            display_name: "Owner".into(),
        })
        .await
        .unwrap();
    let retained = db
        .members()
        .create(NewMember {
            list_id: list.id.clone(),
            email: "keep@example.com".into(),
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsUser,
            display_name: "Keep Me".into(),
        })
        .await
        .unwrap();
    db.members()
        .create(NewMember {
            list_id: list.id.clone(),
            email: "remove@example.com".into(),
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsAddress,
            display_name: "Remove Me".into(),
        })
        .await
        .unwrap();
    let before_audit = db.audit().list().await.unwrap().len();

    let result = db
        .members()
        .mass_for_role(
            &list.id,
            "sync",
            &["keep@example.com".into(), "new@example.com".into()],
            MemberRole::Member,
            SubscriptionMode::AsAddress,
        )
        .await
        .unwrap();

    assert_eq!((result.added, result.removed, result.retained), (1, 1, 1));
    assert_eq!(db.audit().list().await.unwrap().len(), before_audit + 1);
    assert_eq!(
        db.members().get(owner.id).await.unwrap().role,
        MemberRole::Owner
    );
    let after = db.members().get(retained.id).await.unwrap();
    assert_eq!(after.subscription_mode, SubscriptionMode::AsUser);
    assert_eq!(after.display_name, "Keep Me");
    let roster = db
        .members()
        .roster(&list.id, MemberRole::Member)
        .await
        .unwrap();
    assert_eq!(roster.len(), 2);
    let new = db
        .members()
        .find("new@example.com")
        .await
        .unwrap()
        .remove(0);
    assert_eq!(new.subscription_mode, SubscriptionMode::AsAddress);
}

#[tokio::test]
async fn member_mass_validates_all_rows_and_duplicates_before_any_write_or_audit() {
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
    db.members()
        .create(NewMember {
            list_id: list.id.clone(),
            email: "existing@example.com".into(),
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsAddress,
            display_name: String::new(),
        })
        .await
        .unwrap();
    let audit = db.audit().list().await.unwrap().len();
    for rows in [
        vec!["replacement@example.com".into(), "late-invalid".into()],
        vec![
            "duplicate@example.com".into(),
            "DUPLICATE@example.com".into(),
        ],
    ] {
        assert!(
            db.members()
                .mass_for_role(
                    &list.id,
                    "sync",
                    &rows,
                    MemberRole::Member,
                    SubscriptionMode::AsAddress,
                )
                .await
                .is_err()
        );
        assert_eq!(
            db.members()
                .roster(&list.id, MemberRole::Member)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(db.audit().list().await.unwrap().len(), audit);
    }
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
async fn deleting_members_and_users_removes_their_owned_preferences() {
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
            display_name: "Owned preferences".into(),
            email: "owned@example.com".into(),
            password: "Orbit!Cobalt7-River$Quartz".into(),
            server_owner: false,
        })
        .await
        .unwrap();
    let user_preferences: String =
        sqlx::query_scalar("SELECT preferences_id FROM users WHERE id=$1")
            .bind(user.id.to_string())
            .fetch_one(db.pool())
            .await
            .unwrap();
    let member = db
        .members()
        .create(NewMember {
            list_id: list.id,
            email: "owned@example.com".into(),
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsAddress,
            display_name: "Owned preferences".into(),
        })
        .await
        .unwrap();
    let token = db
        .tokens()
        .create(user.id, "owned", &["admin"], None)
        .await
        .unwrap();

    db.members().delete(member.id).await.unwrap();
    assert_eq!(
        row_count(&db, "preferences", "id", &member.preferences_id.to_string()).await,
        0,
        "member-owned preferences must not leak after deletion"
    );

    db.users().delete(user.id).await.unwrap();
    assert_eq!(
        row_count(&db, "api_tokens", "id", &token.id.to_string()).await,
        0,
        "user-owned API tokens must not block or survive user deletion"
    );
    assert_eq!(
        row_count(&db, "preferences", "id", &user_preferences).await,
        0,
        "user-owned preferences must not leak after deletion"
    );
}

struct ListDeleteFixture {
    db: Database,
    deleted: listmngr_core::MailingList,
    kept: listmngr_core::MailingList,
    user: listmngr_core::User,
    deleted_member: listmngr_core::Member,
    kept_member: listmngr_core::Member,
    deleted_preferences: String,
    kept_preferences: String,
}

async fn add_list_owned_rows(db: &Database, lists: &[&listmngr_core::ListId]) {
    for list in lists {
        db.lists()
            .set_archiver(list, "prototype", true)
            .await
            .unwrap();
        db.lists()
            .set_template(list, "list:user:notice:welcome", "en", list.as_str())
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
}

async fn list_delete_fixture() -> ListDeleteFixture {
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
    add_list_owned_rows(&db, &[&deleted.id, &kept.id]).await;
    sqlx::query("INSERT INTO bans(id,list_id,email_or_regex) VALUES('global-ban',NULL,'global@example.com')")
        .execute(db.pool())
        .await
        .unwrap();

    ListDeleteFixture {
        db,
        deleted,
        kept,
        user,
        deleted_member,
        kept_member,
        deleted_preferences,
        kept_preferences,
    }
}

#[tokio::test]
async fn deleting_list_removes_owned_graph_but_preserves_shared_identity_and_other_lists() {
    let fixture = list_delete_fixture().await;
    let ListDeleteFixture {
        db,
        deleted,
        kept,
        user,
        deleted_member,
        kept_member,
        deleted_preferences,
        kept_preferences,
    } = fixture;

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
        .set_template(&list.id, "list:user:notice:welcome", "en", "atomic")
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
        // Mailman's system default, since no layer set it.
        Some(true)
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
            .set_template(&list_id, "list:user:notice:welcome", "en", "changed")
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

#[tokio::test]
async fn audit_context_records_actor_token_and_socket_ip_without_secrets() {
    let db = migrated_sqlite().await;
    let actor = db
        .users()
        .create(NewUser {
            display_name: "Actor".into(),
            email: "actor@example.com".into(),
            password: "Orbit!Cobalt7-River$Quartz".into(),
            server_owner: true,
        })
        .await
        .unwrap();
    let issued = db
        .tokens()
        .create(actor.id, "actor", &["admin"], None)
        .await
        .unwrap();
    let context = AuditContext::new(
        Some(actor.id),
        Some(issued.id),
        Some("203.0.113.17".parse().unwrap()),
    );
    let before = db.audit().list().await.unwrap().len();

    db.domains()
        .create_with_context("audited.example", "secret=do-not-log", None, &context)
        .await
        .unwrap();
    let list = db
        .lists()
        .create_with_context(
            NewList {
                list_id: "events.audited.example".parse().unwrap(),
                display_name: "Events".into(),
                style: "legacy-default".into(),
            },
            &context,
        )
        .await
        .unwrap();
    db.lists()
        .set_archiver_with_context(&list.id, "prototype", true, &context)
        .await
        .unwrap();
    db.lists()
        .set_template_with_context(
            &list.id,
            "list:user:notice:welcome",
            "en",
            "secret template",
            &context,
        )
        .await
        .unwrap();
    let contextual_token = db
        .tokens()
        .create_with_context(actor.id, "contextual", &["system:read"], None, &context)
        .await
        .unwrap();
    db.tokens()
        .revoke_with_context(contextual_token.id, &context)
        .await
        .unwrap();

    let entries = db.audit().list().await.unwrap();
    assert_eq!(entries.len(), before + 6, "one logical write has one event");
    for entry in &entries[before..] {
        assert_eq!(entry.actor_user_id, Some(actor.id));
        assert_eq!(entry.actor_token_id, Some(issued.id));
        assert_eq!(entry.ip, Some("203.0.113.17".parse().unwrap()));
    }
    let actions = entries[before..]
        .iter()
        .map(|entry| entry.action.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        actions,
        [
            "domain.create",
            "list.create",
            "list.archiver.set",
            "template.set",
            "token.create",
            "token.revoke",
        ]
    );
    let rendered = serde_json::to_string(&entries[before..])
        .unwrap()
        .to_ascii_lowercase();
    for secret in [
        "do-not-log",
        "secret template",
        "password",
        "token_hash",
        "$argon2",
        &contextual_token.token.to_ascii_lowercase(),
    ] {
        assert!(
            !rendered.contains(secret),
            "audit leaked {secret}: {rendered}"
        );
    }
}

#[tokio::test]
async fn subscription_verification_and_audit_are_one_atomic_write() {
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
    let before = db.audit().list().await.unwrap().len();

    let member = db
        .members()
        .subscribe_with_context(
            NewMember {
                list_id: list.id.clone(),
                email: "subscriber@example.com".into(),
                role: MemberRole::Member,
                subscription_mode: SubscriptionMode::AsAddress,
                display_name: "Subscriber".into(),
            },
            true,
            &AuditContext::system(),
        )
        .await
        .unwrap();

    assert!(
        db.addresses()
            .get("subscriber@example.com")
            .await
            .unwrap()
            .verified_on
            .is_some()
    );
    assert_eq!(db.audit().list().await.unwrap().len(), before + 1);
    assert!(db.members().get(member.id).await.is_ok());

    let db = migrated_sqlite().await;
    db.domains().create("example.com", "", None).await.unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: "rollback.example.com".parse().unwrap(),
            display_name: "Rollback".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    sabotage_audit(&db).await;
    assert!(
        db.members()
            .subscribe_with_context(
                NewMember {
                    list_id: list.id,
                    email: "rollback@example.com".into(),
                    role: MemberRole::Member,
                    subscription_mode: SubscriptionMode::AsAddress,
                    display_name: "Rollback".into(),
                },
                true,
                &AuditContext::system(),
            )
            .await
            .is_err()
    );
    for table in ["members", "addresses", "preferences"] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(count, 0, "{table} must roll back");
    }
}
