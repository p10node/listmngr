use listmngr_db::{Database, NewList, workflows::SubscriptionAction};

#[path = "workflows/bans.rs"]
mod bans;
#[path = "workflows/receipts.rs"]
mod receipts;

async fn fixture() -> Database {
    fixture_at("sqlite::memory:").await
}
async fn fixture_at(url: &str) -> Database {
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
#[tokio::test]
async fn confirmation_preserves_original_delivery_mailbox_spelling() {
    let db = fixture().await;
    let list = "test.example.com".parse().unwrap();
    db.workflows()
        .request(
            &list,
            "CaseSensitive@example.com",
            SubscriptionAction::Join,
            100_000,
        )
        .await
        .unwrap();
    db.workflows()
        .confirm(&list, &token(&db).await, 100_001)
        .await
        .unwrap();
    let address = db
        .addresses()
        .get("casesensitive@example.com")
        .await
        .unwrap();
    assert_eq!(address.email, "casesensitive@example.com");
    assert_eq!(
        address.original_email, "CaseSensitive@example.com",
        "canonical identity is not the SMTP recipient spelling"
    );
}

#[tokio::test]
async fn subscription_confirmation_does_not_verify_or_relink_an_existing_account() {
    let db = fixture().await;
    let user = db
        .users()
        .create(listmngr_db::NewUser {
            display_name: "Existing".into(),
            email: "Existing@example.com".into(),
            password: "Orbit!Cobalt7-River$Quartz".into(),
            server_owner: false,
        })
        .await
        .unwrap();
    let list = "test.example.com".parse().unwrap();
    let before = db.addresses().get("existing@example.com").await.unwrap();
    assert!(before.verified_on.is_none());
    db.workflows()
        .request(
            &list,
            "EXISTING@example.com",
            SubscriptionAction::Join,
            100_000,
        )
        .await
        .unwrap();
    db.workflows()
        .confirm(&list, &token(&db).await, 100_001)
        .await
        .unwrap();
    let after = db.addresses().get("existing@example.com").await.unwrap();
    assert_eq!(after.original_email, before.original_email);
    assert_eq!(after.user_id, Some(user.id));
    assert!(
        after.verified_on.is_none(),
        "joining a list must not authorize a pre-registered account"
    );
}

async fn count(db: &Database, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
        .fetch_one(db.pool())
        .await
        .unwrap()
}
async fn token(db: &Database) -> String {
    let raw: Vec<u8> = sqlx::query_scalar("SELECT raw FROM message_blobs LIMIT 1")
        .fetch_one(db.pool())
        .await
        .unwrap();
    String::from_utf8(raw)
        .unwrap()
        .split("Token: ")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .into()
}

#[tokio::test]
async fn restart_retains_notice_and_concurrent_confirmers_consume_only_once() {
    let path =
        std::env::temp_dir().join(format!("listmngr-workflow-{}.sqlite", uuid::Uuid::now_v7()));
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let db = fixture_at(&url).await;
    let list = "test.example.com".parse().unwrap();
    db.workflows()
        .request(
            &list,
            "durable@example.com",
            SubscriptionAction::Join,
            100_000,
        )
        .await
        .unwrap();
    db.pool().close().await;
    let db = Database::connect(&url, 1).await.unwrap();
    let second = Database::connect(&url, 1).await.unwrap();
    let secret = token(&db).await;
    assert_eq!(count(&db, "queue_jobs").await, 1);
    db.workflows()
        .request(
            &list,
            "durable@example.com",
            SubscriptionAction::Join,
            100_001,
        )
        .await
        .unwrap();
    assert_eq!(count(&db, "queue_jobs").await, 1);
    let a = db.workflows();
    let b = second.workflows();
    let (a, b) = tokio::join!(
        a.confirm(&list, &secret, 100_002),
        b.confirm(&list, &secret, 100_002)
    );
    assert_ne!(a.is_ok(), b.is_ok());
    assert_eq!(count(&db, "members").await, 1);
    let audits: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='subscription.confirm'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(audits, 1);
    assert_eq!(count(&db, "workflow_notices").await, 2);
    assert_eq!(count(&db, "queue_jobs").await, 2);
    db.pool().close().await;
    second.pool().close().await;
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn confirmation_waiting_for_database_does_not_use_stale_expiry_clock() {
    let db = fixture().await;
    let list = "test.example.com".parse().unwrap();
    db.workflows()
        .request(&list, "slow@example.com", SubscriptionAction::Join, 100_000)
        .await
        .unwrap();
    let secret = token(&db).await;
    sqlx::query("UPDATE subscription_workflows SET expires_at=100010")
        .execute(db.pool())
        .await
        .unwrap();
    let connection = db.pool().acquire().await.unwrap();
    let repo = db.workflows();
    let mut confirm = Box::pin(repo.confirm(&list, &secret, 100_000));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), &mut confirm)
            .await
            .is_err()
    );
    drop(connection);
    assert!(
        confirm.await.is_err(),
        "token expired while waiting for the pool"
    );
    assert_eq!(count(&db, "members").await, 0);
}

#[tokio::test]
async fn request_performs_bounded_expiry_cleanup() {
    let db = fixture().await;
    for n in 0..150 {
        sqlx::query("INSERT INTO subscription_workflows(id,list_id,email,action,token_hash,created_at,expires_at) VALUES($1,'test.example.com','old@example.com','join',$1,0,1)").bind(format!("expired-{n}")).execute(db.pool()).await.unwrap();
    }
    db.workflows()
        .request(
            &"test.example.com".parse().unwrap(),
            "new@example.com",
            SubscriptionAction::Join,
            100_000,
        )
        .await
        .unwrap();
    assert_eq!(count(&db, "subscription_workflows").await, 51);
    assert_eq!(count(&db, "queue_jobs").await, 1);
}

#[tokio::test]
async fn invalid_header_and_envelope_mailboxes_never_enter_notice_spool() {
    let db = fixture().await;
    let list = "test.example.com".parse().unwrap();
    for email in [
        "bad>victim@example.com",
        "x,y@example.com",
        "x\"y@example.com",
        "ümlaut@example.com",
        "x@example.com\r\nBcc: evil@example.com",
    ] {
        assert!(
            db.workflows()
                .request(&list, email, SubscriptionAction::Join, 100_000)
                .await
                .is_err(),
            "accepted {email}"
        );
    }
    assert_eq!(count(&db, "message_blobs").await, 0);
}

#[tokio::test]
async fn expiration_is_exact_and_audit_failure_rolls_back_membership_and_token() {
    let db = fixture().await;
    let list = "test.example.com".parse().unwrap();
    db.workflows()
        .request(
            &list,
            "member@example.com",
            SubscriptionAction::Join,
            100_000,
        )
        .await
        .unwrap();
    let secret = token(&db).await;
    assert!(
        db.workflows()
            .confirm(&list, &secret, 86_500_000)
            .await
            .is_err()
    );
    assert_eq!(count(&db, "members").await, 0);
    sqlx::query("CREATE TRIGGER fail_confirm BEFORE INSERT ON audit_log WHEN NEW.action='subscription.confirm' BEGIN SELECT RAISE(ABORT,'test audit failure'); END").execute(db.pool()).await.unwrap();
    assert!(
        db.workflows()
            .confirm(&list, &secret, 100_001)
            .await
            .is_err()
    );
    assert_eq!(count(&db, "members").await, 0);
    assert_eq!(count(&db, "addresses").await, 0);
    let consumed: i64 = sqlx::query_scalar("SELECT consumed FROM subscription_workflows")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(consumed, 0);
    sqlx::query("DROP TRIGGER fail_confirm")
        .execute(db.pool())
        .await
        .unwrap();
    db.workflows()
        .confirm(&list, &secret, 100_002)
        .await
        .unwrap();
    assert_eq!(count(&db, "members").await, 1);
}

#[tokio::test]
async fn notice_audit_failure_rolls_back_every_request_write() {
    let db = fixture().await;
    let before = count(&db, "audit_log").await;
    sqlx::query("CREATE TRIGGER fail_request BEFORE INSERT ON audit_log WHEN NEW.action='subscription.request' BEGIN SELECT RAISE(ABORT,'test audit failure'); END").execute(db.pool()).await.unwrap();
    assert!(
        db.workflows()
            .request(
                &"test.example.com".parse().unwrap(),
                "member@example.com",
                SubscriptionAction::Leave,
                100_000
            )
            .await
            .is_err()
    );
    for table in [
        "subscription_workflows",
        "workflow_notices",
        "messages",
        "message_blobs",
        "queue_jobs",
        "delivery_recipients",
        "members",
    ] {
        assert_eq!(count(&db, table).await, 0, "{table}");
    }
    assert_eq!(count(&db, "audit_log").await, before);
}

#[tokio::test]
async fn global_and_address_notice_limits_are_durable_and_bounded() {
    let db = fixture().await;
    let list = "test.example.com".parse().unwrap();
    for n in 0..110 {
        db.workflows()
            .request(
                &list,
                &format!("m{n}@example.com"),
                SubscriptionAction::Join,
                100_000,
            )
            .await
            .unwrap();
    }
    assert_eq!(count(&db, "queue_jobs").await, 100);
    db.workflows()
        .request(&list, "m0@example.com", SubscriptionAction::Leave, 160_000)
        .await
        .unwrap();
    assert_eq!(count(&db, "queue_jobs").await, 100);
    db.workflows()
        .request(
            &list,
            "fresh@example.com",
            SubscriptionAction::Leave,
            160_000,
        )
        .await
        .unwrap();
    assert_eq!(count(&db, "queue_jobs").await, 101);
}
