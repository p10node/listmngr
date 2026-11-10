//! Anonymous join requests on a `moderate` list park a row for a moderator.
//! That queue is bounded: a repeated request for the same address is not a
//! second row, a list holds a fixed number of undecided requests, and a
//! request nobody decided is dropped after thirty days.
use listmngr_db::workflows::{RequestFilter, SubscriptionAction};
use listmngr_db::{Database, NewList};
use serde_json::json;

const LIST: &str = "dev.example.invalid";
const HOUR: i64 = 3_600_000;
const DAY: i64 = 24 * HOUR;

async fn fixture() -> (Database, listmngr_core::ListId) {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: LIST.parse().unwrap(),
            display_name: "Dev".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    db.lists()
        .update(&list.id, &json!({"subscription_policy": "moderate"}))
        .await
        .unwrap();
    (db, list.id)
}

async fn rows(db: &Database) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM subscription_workflows")
        .fetch_one(db.pool())
        .await
        .unwrap()
}

async fn join(db: &Database, list: &listmngr_core::ListId, email: &str, at: i64) {
    db.workflows()
        .request(list, email, SubscriptionAction::Join, at)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_repeated_request_for_one_address_is_one_pending_row() {
    let (db, list) = fixture().await;
    for at in [0, 2 * HOUR, 30 * HOUR, 60 * HOUR, 120 * HOUR, 240 * HOUR] {
        join(&db, &list, "victim@example.invalid", at).await;
    }
    assert_eq!(rows(&db).await, 1);
}

#[tokio::test]
async fn an_undecided_request_is_dropped_after_thirty_days() {
    let (db, list) = fixture().await;
    join(&db, &list, "old@example.invalid", 0).await;
    join(&db, &list, "recent@example.invalid", 25 * DAY).await;
    join(&db, &list, "new@example.invalid", 31 * DAY).await;
    let pending = db
        .workflows()
        .pending(&list, RequestFilter::default())
        .await
        .unwrap();
    let mut emails: Vec<_> = pending.iter().map(|p| p.email.as_str()).collect();
    emails.sort_unstable();
    assert_eq!(emails, ["new@example.invalid", "recent@example.invalid"]);
}

#[tokio::test]
async fn a_list_holds_a_bounded_number_of_undecided_requests() {
    let (db, list) = fixture().await;
    // The site-wide request rate admits a hundred a minute: step past it.
    for i in 0..1_010_i64 {
        join(&db, &list, &format!("u{i}@example.invalid"), i * 61_000).await;
    }
    assert_eq!(rows(&db).await, 1_000);
}
