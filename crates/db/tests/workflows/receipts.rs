use super::{count, fixture};
use listmngr_db::{Database, workflows::SubscriptionAction};

async fn latest_token(db: &Database) -> String {
    let raws: Vec<Vec<u8>> = sqlx::query_scalar(
        "SELECT b.raw FROM message_blobs b JOIN messages m ON m.store_key=b.store_key ORDER BY m.created_at DESC",
    ).fetch_all(db.pool()).await.unwrap();
    raws.iter()
        .find_map(|raw| {
            std::str::from_utf8(raw)
                .unwrap()
                .lines()
                .find_map(|line| line.strip_prefix("Token: ").map(str::to_owned))
        })
        .unwrap()
}

#[tokio::test]
async fn unleased_receipt_failure_rolls_back_join_and_leave_then_retry_succeeds() {
    for action in [SubscriptionAction::Join, SubscriptionAction::Leave] {
        for trigger in [
            "CREATE TRIGGER fail_receipt BEFORE INSERT ON workflow_notices BEGIN SELECT RAISE(ABORT,'receipt fault'); END",
            "CREATE TRIGGER fail_receipt BEFORE INSERT ON audit_log WHEN NEW.action='subscription.confirm' BEGIN SELECT RAISE(ABORT,'audit fault'); END",
        ] {
            let db = fixture().await;
            let list = "test.example.com".parse().unwrap();
            if matches!(action, SubscriptionAction::Leave) {
                db.workflows()
                    .request(
                        &list,
                        "Exact@example.com",
                        SubscriptionAction::Join,
                        100_000,
                    )
                    .await
                    .unwrap();
                db.workflows()
                    .confirm(&list, &latest_token(&db).await, 100_001)
                    .await
                    .unwrap();
            }
            db.workflows()
                .request(&list, "Exact@example.com", action, 7_300_000)
                .await
                .unwrap();
            let token = latest_token(&db).await;
            let tables = [
                "members",
                "preferences",
                "addresses",
                "message_blobs",
                "messages",
                "queue_jobs",
                "workflow_notices",
                "delivery_recipients",
                "audit_log",
            ];
            let mut before = Vec::new();
            for table in tables {
                before.push(count(&db, table).await);
            }
            sqlx::query(trigger).execute(db.pool()).await.unwrap();
            assert!(
                db.workflows()
                    .confirm(&list, &token, 7_300_001)
                    .await
                    .is_err()
            );
            for (table, expected) in tables.into_iter().zip(before) {
                assert_eq!(
                    count(&db, table).await,
                    expected,
                    "partial effect in {table}"
                );
            }
            let pending: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM subscription_workflows WHERE consumed=0")
                    .fetch_one(db.pool())
                    .await
                    .unwrap();
            assert_eq!(pending, 1);
            sqlx::query("DROP TRIGGER fail_receipt")
                .execute(db.pool())
                .await
                .unwrap();
            db.workflows()
                .confirm(&list, &token, 7_300_002)
                .await
                .unwrap();
            let expected = i64::from(matches!(action, SubscriptionAction::Join));
            assert_eq!(count(&db, "members").await, expected);
            let notices = count(&db, "workflow_notices").await;
            assert_eq!(notices, if expected == 1 { 2 } else { 4 });
            assert!(
                db.workflows()
                    .confirm(&list, &token, 7_300_003)
                    .await
                    .is_err()
            );
            assert_eq!(count(&db, "workflow_notices").await, notices);
        }
    }
}
