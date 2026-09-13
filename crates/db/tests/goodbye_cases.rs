use listmngr_core::{ListId, MemberRole, SubscriptionMode};
use listmngr_db::{Database, NewList, NewMember, workflows::SubscriptionAction};
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
        .update(&list, &json!({"send_goodbye_message":true}))
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
async fn goodbye_roles_default_noops_original_mailbox_and_sync() {
    let (db, list) = fixture().await;
    let off = "off.example.invalid".parse().unwrap();
    for role in [
        MemberRole::Owner,
        MemberRole::Moderator,
        MemberRole::Nonmember,
        MemberRole::Member,
    ] {
        let target = if role == MemberRole::Member {
            &off
        } else {
            &list
        };
        let m = db
            .members()
            .create(member(target, "Stored@example.invalid", role))
            .await
            .unwrap();
        db.members().delete(m.id).await.unwrap();
        assert!(db.members().delete(m.id).await.is_err());
        db.members()
            .mass_for_role(
                target,
                "subscribe",
                &["Bulk@example.invalid".into()],
                role,
                SubscriptionMode::AsAddress,
            )
            .await
            .unwrap();
        db.members()
            .mass_for_role(
                target,
                "unsubscribe",
                &["bulk@example.invalid".into()],
                role,
                SubscriptionMode::AsAddress,
            )
            .await
            .unwrap();
    }
    assert_eq!(count(&db, "workflow_notices").await, 0);
    db.members()
        .mass(
            &list,
            "subscribe",
            &[
                "stored@example.invalid".into(),
                "bulk@example.invalid".into(),
                "Survivor@example.invalid".into(),
            ],
        )
        .await
        .unwrap();
    db.members()
        .mass(&list, "unsubscribe", &["STORED@example.invalid".into()])
        .await
        .unwrap();
    db.members()
        .mass(&list, "sync", &["survivor@example.invalid".into()])
        .await
        .unwrap();
    db.members()
        .mass(&list, "sync", &["survivor@example.invalid".into()])
        .await
        .unwrap();
    assert!(
        db.members()
            .mass(&list, "unsubscribe", &["stored@example.invalid".into()])
            .await
            .is_err()
    );
    assert!(
        db.members()
            .mass(&list, "unsubscribe", &["absent@example.invalid".into()])
            .await
            .is_err()
    );
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
        assert!(raw.contains("You have been unsubscribed from the"));
        assert!(raw.contains("(on@example.invalid)"));
        assert!(raw.len() <= 4096);
        assert!(!raw.contains("SECRET"));
        assert!(!raw.contains("Survivor"));
    }
}

#[tokio::test]
async fn goodbye_direct_mass_rollback_and_retry() {
    for operation in ["direct", "unsubscribe", "sync"] {
        for table in ["audit_log", "workflow_notices", "delivery_recipients"] {
            let (db, list) = fixture().await;
            let m = db
                .members()
                .create(member(&list, "Stored@example.invalid", MemberRole::Member))
                .await
                .unwrap();
            let before = snapshot(&db).await;
            sqlx::query(&format!("CREATE TRIGGER sabotage BEFORE INSERT ON {table} BEGIN SELECT RAISE(ABORT,'owned fixture failure'); END")).execute(db.pool()).await.unwrap();
            let result = if operation == "direct" {
                db.members().delete(m.id).await
            } else {
                let emails = ["stored@example.invalid".into()];
                db.members()
                    .mass(
                        &list,
                        operation,
                        if operation == "sync" { &[] } else { &emails },
                    )
                    .await
                    .map(|_| ())
            };
            assert!(result.is_err(), "{operation}/{table}");
            assert_eq!(snapshot(&db).await, before, "{operation}/{table}");
            assert!(db.members().get(m.id).await.is_ok());
            sqlx::query("DROP TRIGGER sabotage")
                .execute(db.pool())
                .await
                .unwrap();
            db.members().delete(m.id).await.unwrap();
            assert!(db.members().delete(m.id).await.is_err());
            assert_eq!(count(&db, "workflow_notices").await, 1);
        }
    }
}

#[tokio::test]
async fn goodbye_confirmation_keeps_receipt_separate_and_rolls_back_token() {
    for present in [false, true] {
        let (db, list) = fixture().await;
        // Preserve authoritative spelling independently of challenge request.
        let m = db
            .members()
            .create(member(&list, "Stored@example.invalid", MemberRole::Member))
            .await
            .unwrap();
        db.workflows()
            .request(
                &list,
                "stored@example.invalid",
                SubscriptionAction::Leave,
                100_000,
            )
            .await
            .unwrap();
        let raw = raw_notices(&db).await.remove(0);
        let token = raw
            .split("Token: ")
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap();
        if !present {
            db.lists()
                .update(&list, &json!({"send_goodbye_message":false}))
                .await
                .unwrap();
            db.members().delete(m.id).await.unwrap();
            db.lists()
                .update(&list, &json!({"send_goodbye_message":true}))
                .await
                .unwrap();
        }
        let before = snapshot(&db).await;
        for table in ["audit_log", "workflow_notices"] {
            sqlx::query(&format!("CREATE TRIGGER sabotage BEFORE INSERT ON {table} BEGIN SELECT RAISE(ABORT,'owned fixture failure'); END")).execute(db.pool()).await.unwrap();
            assert!(db.workflows().confirm(&list, token, 100_001).await.is_err());
            assert_eq!(snapshot(&db).await, before);
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT consumed FROM subscription_workflows")
                    .fetch_one(db.pool())
                    .await
                    .unwrap(),
                0
            );
            sqlx::query("DROP TRIGGER sabotage")
                .execute(db.pool())
                .await
                .unwrap();
        }
        db.workflows().confirm(&list, token, 100_001).await.unwrap();
        assert!(db.workflows().confirm(&list, token, 100_002).await.is_err());
        let raws = raw_notices(&db).await;
        assert_eq!(
            raws.iter()
                .filter(|r| r.contains("Subject: List leave request completed"))
                .count(),
            1
        );
        let goodbyes: Vec<_> = raws
            .iter()
            .filter(|r| r.contains("Subject: You have been unsubscribed from"))
            .collect();
        assert_eq!(goodbyes.len(), usize::from(present));
        if present {
            assert!(goodbyes[0].contains("To: Stored@example.invalid\r\n"));
        }
        assert!(db.members().get(m.id).await.is_err());
    }
}

#[tokio::test]
async fn goodbye_migration_constraints_and_legacy_json_default() {
    let (db, list) = fixture().await;
    db.migrate().await.unwrap();
    for value in ["NULL", "-1", "2"] {
        assert!(
            sqlx::query(&format!(
                "UPDATE mailing_lists SET send_goodbye_message={value}"
            ))
            .execute(db.pool())
            .await
            .is_err()
        );
    }
    let mut old = serde_json::to_value(db.lists().get(&list).await.unwrap()).unwrap();
    old.as_object_mut().unwrap().remove("send_goodbye_message");
    let decoded: listmngr_core::MailingList =
        serde_json::from_value(old).expect("legacy JSON must default off");
    assert!(!decoded.send_goodbye_message);
}

#[tokio::test]
async fn goodbye_list_teardown_notifies_members_only_and_audit_failure_rolls_back() {
    let (db, list) = fixture().await;
    for role in [
        MemberRole::Member,
        MemberRole::Owner,
        MemberRole::Moderator,
        MemberRole::Nonmember,
    ] {
        db.members()
            .create(member(&list, &format!("{role}@example.invalid"), role))
            .await
            .unwrap();
    }
    let before = snapshot(&db).await;
    sqlx::query("CREATE TRIGGER sabotage BEFORE INSERT ON audit_log BEGIN SELECT RAISE(ABORT,'owned fixture failure'); END").execute(db.pool()).await.unwrap();
    assert!(db.lists().delete(&list).await.is_err());
    assert_eq!(snapshot(&db).await, before);
    sqlx::query("DROP TRIGGER sabotage")
        .execute(db.pool())
        .await
        .unwrap();
    db.lists().delete(&list).await.unwrap();
    assert!(db.lists().delete(&list).await.is_err());
    assert_eq!(count(&db, "members").await, 0);
    assert_eq!(count(&db, "workflow_notices").await, 1);
    let raw = raw_notices(&db).await.remove(0);
    assert!(raw.contains("To: member@example.invalid\r\n"));
    assert!(raw.contains("You have been unsubscribed from the"));
    assert!(raw.contains("(on@example.invalid)"));
}
