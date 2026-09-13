use listmngr_core::{ListId, MemberRole, SubscriptionMode};
use listmngr_db::{AuditContext, Database, NewList, NewMember, workflows::SubscriptionAction};
use serde_json::json;

async fn fixture() -> (Database, ListId) {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    for name in ["on", "off"] {
        db.lists()
            .create(NewList {
                list_id: format!("{name}.example.invalid").parse().unwrap(),
                // The list display name is public (Mailman prints it in notices);
                // secrecy is asserted on member data only.
                display_name: "On List".into(),
                style: "legacy-default".into(),
            })
            .await
            .unwrap();
    }
    let list = "on.example.invalid".parse().unwrap();
    db.lists()
        .update(&list, &json!({"send_welcome_message":true}))
        .await
        .unwrap();
    (db, list)
}
fn member(list: &ListId, email: &str, role: MemberRole) -> NewMember {
    NewMember {
        list_id: list.clone(),
        email: email.into(),
        role,
        subscription_mode: SubscriptionMode::AsAddress,
        display_name: "SECRET MEMBER".into(),
    }
}
async fn count(db: &Database, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
        .fetch_one(db.pool())
        .await
        .unwrap()
}
async fn snapshot(db: &Database) -> Vec<i64> {
    let mut counts = Vec::new();
    for table in [
        "members",
        "addresses",
        "preferences",
        "message_blobs",
        "messages",
        "queue_jobs",
        "delivery_recipients",
        "workflow_notices",
        "audit_log",
    ] {
        counts.push(count(db, table).await);
    }
    counts
}
async fn raw_notices(db: &Database) -> Vec<String> {
    let raws: Vec<Vec<u8>> = sqlx::query_scalar("SELECT b.raw FROM workflow_notices n JOIN queue_jobs q ON q.id=n.job_id JOIN messages m ON m.id=q.message_id JOIN message_blobs b ON b.store_key=m.store_key").fetch_all(db.pool()).await.unwrap();
    raws.into_iter()
        .map(|raw| String::from_utf8(raw).unwrap())
        .collect()
}

#[tokio::test]
async fn direct_bulk_role_default_list_and_noop_differentials() {
    let (db, list) = fixture().await;
    let off: ListId = "off.example.invalid".parse().unwrap();
    for role in [
        MemberRole::Owner,
        MemberRole::Moderator,
        MemberRole::Nonmember,
    ] {
        db.members()
            .create(member(&list, "Stored@example.invalid", role))
            .await
            .unwrap();
        db.members()
            .mass_for_role(
                &list,
                "subscribe",
                &[format!("{role}@example.invalid")],
                role,
                SubscriptionMode::AsAddress,
            )
            .await
            .unwrap();
    }
    db.members()
        .create(member(&off, "Stored@example.invalid", MemberRole::Member))
        .await
        .unwrap();
    db.members()
        .mass(&off, "subscribe", &["defaultbulk@example.invalid".into()])
        .await
        .unwrap();
    assert_eq!(count(&db, "workflow_notices").await, 0);
    db.members()
        .subscribe_with_context(
            member(&list, "stored@example.invalid", MemberRole::Member),
            false,
            &AuditContext::system(),
        )
        .await
        .unwrap();
    assert_eq!(count(&db, "workflow_notices").await, 1);
    assert!(
        db.members()
            .create(member(&list, "stored@example.invalid", MemberRole::Member))
            .await
            .is_err()
    );
    db.members()
        .mass(
            &list,
            "sync",
            &[
                "stored@example.invalid".into(),
                "Bulk@example.invalid".into(),
            ],
        )
        .await
        .unwrap();
    db.members()
        .mass(
            &list,
            "sync",
            &[
                "STORED@example.invalid".into(),
                "bulk@example.invalid".into(),
            ],
        )
        .await
        .unwrap();
    assert_eq!(count(&db, "workflow_notices").await, 2);
    let recipients: Vec<String> =
        sqlx::query_scalar("SELECT email FROM delivery_recipients ORDER BY email")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(
        recipients,
        ["Bulk@example.invalid", "Stored@example.invalid"]
    );
    for raw in raw_notices(&db).await {
        assert!(raw.contains("mailing list!"));
        assert!(raw.contains("  on@example.invalid"));
        assert!(raw.len() <= 4096);
        assert!(!raw.contains("SECRET"));
        assert!(!raw.contains("off.example.invalid"));
    }
    db.lists()
        .update(&list, &json!({"send_welcome_message":false}))
        .await
        .unwrap();
    db.members()
        .create(member(
            &list,
            "disabled@example.invalid",
            MemberRole::Member,
        ))
        .await
        .unwrap();
    assert_eq!(count(&db, "workflow_notices").await, 2);
}

#[tokio::test]
async fn direct_and_bulk_audit_and_notice_sabotage_roll_back_all_and_retry() {
    for operation in ["direct", "bulk"] {
        for table in ["audit_log", "workflow_notices", "delivery_recipients"] {
            let (db, list) = fixture().await;
            let before = snapshot(&db).await;
            sqlx::query(&format!("CREATE TRIGGER sabotage BEFORE INSERT ON {table} BEGIN SELECT RAISE(ABORT,'owned fixture failure'); END")).execute(db.pool()).await.unwrap();
            assert!(subscribe(&db, &list, operation).await.is_err());
            assert_eq!(snapshot(&db).await, before, "{operation}/{table}");
            sqlx::query("DROP TRIGGER sabotage")
                .execute(db.pool())
                .await
                .unwrap();
            subscribe(&db, &list, operation).await.unwrap();
            assert_eq!(
                count(&db, "members").await,
                if operation == "bulk" { 2 } else { 1 }
            );
            assert_eq!(
                count(&db, "workflow_notices").await,
                count(&db, "members").await
            );
        }
    }
}
async fn subscribe(db: &Database, list: &ListId, operation: &str) -> listmngr_core::Result<()> {
    if operation == "direct" {
        db.members()
            .create(member(list, "New@example.invalid", MemberRole::Member))
            .await?;
    } else {
        db.members()
            .mass(
                list,
                "subscribe",
                &["New@example.invalid".into(), "Other@example.invalid".into()],
            )
            .await?;
    }
    Ok(())
}

#[tokio::test]
async fn pending_confirm_receipt_replay_and_audit_rollback_are_distinct() {
    let (db, list) = fixture().await;
    db.workflows()
        .request(
            &list,
            "Join@example.invalid",
            SubscriptionAction::Join,
            100_000,
        )
        .await
        .unwrap();
    assert_eq!(count(&db, "members").await, 0);
    let raws = raw_notices(&db).await;
    assert_eq!(raws.len(), 1);
    assert!(!raws[0].contains("Subject: Welcome"));
    let token = raws[0]
        .split("Token: ")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    let before = snapshot(&db).await;
    for table in ["audit_log", "workflow_notices"] {
        sqlx::query(&format!("CREATE TRIGGER sabotage BEFORE INSERT ON {table} BEGIN SELECT RAISE(ABORT,'owned fixture failure'); END")).execute(db.pool()).await.unwrap();
        assert!(db.workflows().confirm(&list, token, 100_001).await.is_err());
        assert_eq!(snapshot(&db).await, before);
        let consumed: i64 = sqlx::query_scalar("SELECT consumed FROM subscription_workflows")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(consumed, 0);
        sqlx::query("DROP TRIGGER sabotage")
            .execute(db.pool())
            .await
            .unwrap();
    }
    db.workflows().confirm(&list, token, 100_001).await.unwrap();
    assert!(db.workflows().confirm(&list, token, 100_002).await.is_err());
    let raws = raw_notices(&db).await;
    assert_eq!(raws.len(), 3);
    assert_eq!(
        raws.iter()
            .filter(|r| r.contains("Subject: Welcome"))
            .count(),
        1
    );
    assert_eq!(
        raws.iter()
            .filter(|r| r.contains("Subject: List join request completed"))
            .count(),
        1
    );
    assert_eq!(count(&db, "members").await, 1);
    // A later confirmed join for an existing member still receives its receipt,
    // but must not turn into a second welcome.
    db.workflows()
        .request(
            &list,
            "Join@example.invalid",
            SubscriptionAction::Join,
            7_300_000,
        )
        .await
        .unwrap();
    let raws = raw_notices(&db).await;
    let token = raws
        .iter()
        .filter_map(|r| r.split("Token: ").nth(1))
        .map(|r| r.split_whitespace().next().unwrap())
        .find(|t| *t != token)
        .unwrap();
    db.workflows()
        .confirm(&list, token, 7_300_001)
        .await
        .unwrap();
    let raws = raw_notices(&db).await;
    assert_eq!(raws.len(), 5);
    assert_eq!(
        raws.iter()
            .filter(|r| r.contains("Subject: Welcome"))
            .count(),
        1
    );
    assert_eq!(
        raws.iter()
            .filter(|r| r.contains("Subject: List join request completed"))
            .count(),
        2
    );
}

#[tokio::test]
async fn public_bans_and_unsafe_transport_cannot_publish() {
    let (db, list) = fixture().await;
    sqlx::query("INSERT INTO bans(id,list_id,email_or_regex) VALUES('welcome-ban',$1,'new@example.invalid')").bind(list.as_str()).execute(db.pool()).await.unwrap();
    db.workflows()
        .request(
            &list,
            "New@example.invalid",
            SubscriptionAction::Join,
            100_000,
        )
        .await
        .unwrap();
    assert_eq!(count(&db, "workflow_notices").await, 0);
    assert!(
        db.members()
            .create(member(
                &list,
                "bad\r\nBcc:other@example.invalid",
                MemberRole::Member
            ))
            .await
            .is_err()
    );
    assert_eq!(count(&db, "workflow_notices").await, 0);
}

#[tokio::test]
async fn welcome_setting_preserves_administrative_ban_override() {
    for enabled in [false, true] {
        for operation in ["direct", "bulk"] {
            let (db, list) = fixture().await;
            db.lists()
                .update(&list, &json!({"send_welcome_message":enabled}))
                .await
                .unwrap();
            sqlx::query("INSERT INTO bans(id,list_id,email_or_regex) VALUES('welcome-ban',$1,'new@example.invalid')")
                .bind(list.as_str()).execute(db.pool()).await.unwrap();
            subscribe(&db, &list, operation)
                .await
                .expect("welcome configuration must not redefine administrative admission");
            assert_eq!(
                count(&db, "members").await,
                if operation == "bulk" { 2 } else { 1 }
            );
            let notices = raw_notices(&db).await;
            assert_eq!(notices.len(), usize::from(enabled && operation == "bulk"));
            for raw in notices {
                assert!(raw.contains("To: Other@example.invalid\r\n"));
                assert!(!raw.contains("To: New@example.invalid\r\n"));
            }
        }
    }
}
