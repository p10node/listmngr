use super::*;
use listmngr_db::AuditContext;

#[tokio::test]
async fn ban_mutations_honor_workflow_reservation_failure() {
    let db = fixture().await;
    let list = "test.example.com".parse().unwrap();
    db.bans()
        .create(&list, "blocked@example.net", &AuditContext::system())
        .await
        .unwrap();
    sqlx::query("CREATE TRIGGER fail_policy_reservation BEFORE UPDATE ON subscription_rate BEGIN SELECT RAISE(ABORT, 'fixture reservation failure'); END").execute(db.pool()).await.unwrap();
    assert!(
        db.bans()
            .create(&list, "other@example.net", &AuditContext::system())
            .await
            .is_err()
    );
    assert!(
        db.bans()
            .delete(&list, "blocked@example.net", &AuditContext::system())
            .await
            .is_err()
    );
    assert_eq!(
        db.bans().list(&list, 10, 0).await.unwrap(),
        ["blocked@example.net"]
    );
    sqlx::query("DROP TRIGGER fail_policy_reservation")
        .execute(db.pool())
        .await
        .unwrap();
    db.bans()
        .delete(&list, "blocked@example.net", &AuditContext::system())
        .await
        .unwrap();
    assert_eq!(count(&db, "bans").await, 0);
}

#[tokio::test]
async fn join_requests_observe_bans_without_tokens_notices_or_cooldown() {
    for (pattern, email, blocked) in [
        ("blocked@example.net", "Blocked@Example.NET.", true),
        ("^Blocked@", "Blocked@example.net", true),
        ("^Blocked@", "blocked@example.net", false),
        ("other@example.net", "blocked@example.net", false),
    ] {
        let db = fixture().await;
        let list = "test.example.com".parse().unwrap();
        db.bans()
            .create(&list, pattern, &AuditContext::system())
            .await
            .unwrap();
        db.workflows()
            .request(&list, email, SubscriptionAction::Join, 100_000)
            .await
            .unwrap();
        let expected = i64::from(!blocked);
        for table in [
            "subscription_workflows",
            "message_blobs",
            "messages",
            "queue_jobs",
            "workflow_notices",
        ] {
            assert_eq!(
                count(&db, table).await,
                expected,
                "{pattern} / {email}: {table}"
            );
        }
        let rate: i64 = sqlx::query_scalar("SELECT requests FROM subscription_rate WHERE id=1")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(rate, expected);
        if blocked {
            db.bans()
                .delete(&list, pattern, &AuditContext::system())
                .await
                .unwrap();
            db.workflows()
                .request(&list, email, SubscriptionAction::Join, 100_001)
                .await
                .unwrap();
            assert_eq!(count(&db, "subscription_workflows").await, 1);
        }
        db.workflows()
            .confirm(&list, &token(&db).await, 100_002)
            .await
            .unwrap();
        assert_eq!(count(&db, "members").await, 1);
    }
}

#[tokio::test]
async fn ban_added_after_challenge_blocks_confirmation_without_consuming_token() {
    let db = fixture().await;
    let list = "test.example.com".parse().unwrap();
    db.workflows()
        .request(
            &list,
            "Blocked@example.net",
            SubscriptionAction::Join,
            100_000,
        )
        .await
        .unwrap();
    let secret = token(&db).await;
    db.bans()
        .create(&list, "blocked@example.net", &AuditContext::system())
        .await
        .unwrap();
    assert!(
        db.workflows()
            .confirm(&list, &secret, 100_001)
            .await
            .is_err(),
        "a pre-ban token must not admit a banned subscriber"
    );
    assert_eq!(count(&db, "members").await, 0);
    let consumed: i64 = sqlx::query_scalar("SELECT consumed FROM subscription_workflows")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(consumed, 0);
    let confirmed: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='subscription.confirm'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(confirmed, 0);
    db.bans()
        .delete(&list, "blocked@example.net", &AuditContext::system())
        .await
        .unwrap();
    db.workflows()
        .confirm(&list, &secret, 100_002)
        .await
        .unwrap();
    assert_eq!(count(&db, "members").await, 1);
}

#[tokio::test]
async fn list_isolation_and_existing_global_bans_apply_to_join() {
    for global in [false, true] {
        let db = fixture().await;
        let list = "test.example.com".parse().unwrap();
        let other = "other.example.com".parse().unwrap();
        db.lists()
            .create(NewList {
                list_id: other,
                display_name: "Other".into(),
                style: "legacy-default".into(),
            })
            .await
            .unwrap();
        // Global management is not exposed; this is isolated legacy-row input.
        sqlx::query("INSERT INTO bans(id,list_id,email_or_regex) VALUES($1,$2,$3)")
            .bind(uuid::Uuid::now_v7().to_string())
            .bind(if global {
                None
            } else {
                Some("other.example.com")
            })
            .bind("^Blocked@")
            .execute(db.pool())
            .await
            .unwrap();
        db.workflows()
            .request(
                &list,
                "Blocked@example.net",
                SubscriptionAction::Join,
                100_000,
            )
            .await
            .unwrap();
        assert_eq!(
            count(&db, "subscription_workflows").await,
            i64::from(!global)
        );
        if !global {
            let secret = token(&db).await;
            sqlx::query("UPDATE bans SET list_id=NULL")
                .execute(db.pool())
                .await
                .unwrap();
            assert!(
                db.workflows()
                    .confirm(&list, &secret, 100_001)
                    .await
                    .is_err()
            );
            assert_eq!(count(&db, "members").await, 0);
        }
    }
}

#[tokio::test]
async fn banned_members_can_request_and_confirm_leave() {
    let db = fixture().await;
    let list = "test.example.com".parse().unwrap();
    db.workflows()
        .request(
            &list,
            "Blocked@example.net",
            SubscriptionAction::Join,
            100_000,
        )
        .await
        .unwrap();
    db.workflows()
        .confirm(&list, &token(&db).await, 100_001)
        .await
        .unwrap();
    db.bans()
        .create(&list, "blocked@example.net", &AuditContext::system())
        .await
        .unwrap();
    // Beyond the existing per-mailbox cooldown, independent of ban admission.
    db.workflows()
        .request(
            &list,
            "Blocked@example.net",
            SubscriptionAction::Leave,
            4_000_000,
        )
        .await
        .unwrap();
    let raws: Vec<Vec<u8>> = sqlx::query_scalar("SELECT raw FROM message_blobs")
        .fetch_all(db.pool())
        .await
        .unwrap();
    let text = raws
        .into_iter()
        .map(|raw| String::from_utf8(raw).unwrap())
        .find(|text| text.contains("unsubscription request"))
        .unwrap();
    let secret = text
        .lines()
        .find_map(|line| line.strip_prefix("Token: "))
        .unwrap();
    db.workflows()
        .confirm(&list, secret, 4_000_001)
        .await
        .unwrap();
    assert_eq!(count(&db, "members").await, 0);
    assert_eq!(count(&db, "bans").await, 1);
}
