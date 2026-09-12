//! Mailman's subscription policies and the moderator queue behind them:
//! `open` acts at once, `confirm` keeps today's token flow, `moderate` and
//! `confirm_then_moderate` park the request until a moderator decides.
use listmngr_core::{MemberRole, SubscriptionMode};
use listmngr_db::workflows::{RequestDecision, RequestFilter, SubscriptionAction, TokenOwner};
use listmngr_db::{AuditContext, Database, NewList, NewMember};
use serde_json::json;

const LIST: &str = "dev.example.invalid";

async fn fixture(policy: &str, unsubscription: &str) -> (Database, listmngr_core::ListId) {
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
        .update(
            &list.id,
            &json!({
                "subscription_policy": policy,
                "unsubscription_policy": unsubscription,
                "send_welcome_message": true,
                "send_goodbye_message": true,
            }),
        )
        .await
        .unwrap();
    (db, list.id)
}

async fn members(db: &Database) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM members")
        .fetch_one(db.pool())
        .await
        .unwrap()
}

async fn notices(db: &Database) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM workflow_notices")
        .fetch_one(db.pool())
        .await
        .unwrap()
}

async fn actions(db: &Database) -> Vec<String> {
    sqlx::query_scalar("SELECT action FROM audit_log ORDER BY at,id")
        .fetch_all(db.pool())
        .await
        .unwrap()
}

async fn add_member(db: &Database, list: &listmngr_core::ListId, email: &str) {
    db.members()
        .create(NewMember {
            list_id: list.clone(),
            email: email.into(),
            display_name: String::new(),
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsAddress,
        })
        .await
        .unwrap();
}

const fn moderator() -> RequestFilter {
    RequestFilter {
        token_owner: Some(TokenOwner::Moderator),
        action: None,
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[tokio::test]
async fn an_open_policy_subscribes_and_unsubscribes_without_a_token() {
    let (db, list) = fixture("open", "open").await;
    db.workflows()
        .request(
            &list,
            "reader@example.invalid",
            SubscriptionAction::Join,
            now(),
        )
        .await
        .unwrap();
    assert_eq!(members(&db).await, 1, "open joins immediately");
    assert!(
        db.workflows()
            .pending(&list, moderator())
            .await
            .unwrap()
            .is_empty(),
        "nothing waits for a moderator"
    );
    let rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM subscription_workflows WHERE state='pending_confirmation'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(rows, 0, "no confirmation token is issued");
    assert_eq!(notices(&db).await, 1, "only the welcome notice");
    assert!(actions(&db).await.contains(&"subscription.open".to_owned()));

    db.workflows()
        .request(
            &list,
            "reader@example.invalid",
            SubscriptionAction::Leave,
            now(),
        )
        .await
        .unwrap();
    assert_eq!(members(&db).await, 0, "open leaves immediately");
    assert_eq!(notices(&db).await, 2, "the goodbye notice follows");
}

#[tokio::test]
async fn a_moderated_policy_parks_the_request_until_a_moderator_accepts() {
    let (db, list) = fixture("moderate", "confirm").await;
    db.workflows()
        .request(
            &list,
            "reader@example.invalid",
            SubscriptionAction::Join,
            now(),
        )
        .await
        .unwrap();
    assert_eq!(members(&db).await, 0, "nothing is subscribed yet");
    assert_eq!(notices(&db).await, 0, "the requester gets no token");
    let pending = db.workflows().pending(&list, moderator()).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].email, "reader@example.invalid");
    assert!(matches!(pending[0].action, SubscriptionAction::Join));
    assert!(
        actions(&db)
            .await
            .contains(&"subscription.request".to_owned())
    );

    db.workflows()
        .decide(
            &pending[0].id,
            RequestDecision::Accept,
            "",
            &AuditContext::system(),
        )
        .await
        .unwrap();
    assert_eq!(members(&db).await, 1);
    assert_eq!(
        notices(&db).await,
        1,
        "the welcome notice is sent on acceptance"
    );
    assert!(
        db.workflows()
            .pending(&list, moderator())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        actions(&db)
            .await
            .contains(&"subscription.accept".to_owned())
    );

    // A decided request cannot be decided again.
    assert!(
        db.workflows()
            .decide(
                &pending[0].id,
                RequestDecision::Accept,
                "",
                &AuditContext::system(),
            )
            .await
            .is_err()
    );
    assert_eq!(members(&db).await, 1);
}

#[tokio::test]
async fn confirm_then_moderate_needs_both_the_token_and_the_moderator() {
    let (db, list) = fixture("confirm_then_moderate", "confirm").await;
    db.workflows()
        .request(
            &list,
            "reader@example.invalid",
            SubscriptionAction::Join,
            now(),
        )
        .await
        .unwrap();
    assert_eq!(notices(&db).await, 1, "the confirmation notice is sent");
    assert!(
        db.workflows()
            .pending(&list, moderator())
            .await
            .unwrap()
            .is_empty(),
        "the moderator sees nothing before the address is confirmed"
    );
    let token = confirmation_token(&db).await;
    db.workflows().confirm(&list, &token, now()).await.unwrap();
    assert_eq!(members(&db).await, 0, "confirming only proves the address");
    let pending = db.workflows().pending(&list, moderator()).await.unwrap();
    assert_eq!(pending.len(), 1);
    // The token is spent: replaying it cannot bypass the moderator.
    assert!(db.workflows().confirm(&list, &token, now()).await.is_err());

    db.workflows()
        .decide(
            &pending[0].id,
            RequestDecision::Reject,
            "",
            &AuditContext::system(),
        )
        .await
        .unwrap();
    assert_eq!(members(&db).await, 0);
    assert!(
        db.workflows()
            .pending(&list, moderator())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        actions(&db)
            .await
            .contains(&"subscription.reject".to_owned())
    );
}

#[tokio::test]
async fn discard_removes_the_request_and_defer_keeps_it_waiting() {
    let (db, list) = fixture("moderate", "moderate").await;
    add_member(&db, &list, "reader@example.invalid").await;
    db.workflows()
        .request(
            &list,
            "reader@example.invalid",
            SubscriptionAction::Leave,
            now(),
        )
        .await
        .unwrap();
    let pending = db.workflows().pending(&list, moderator()).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert!(matches!(pending[0].action, SubscriptionAction::Leave));

    db.workflows()
        .decide(
            &pending[0].id,
            RequestDecision::Defer,
            "",
            &AuditContext::system(),
        )
        .await
        .unwrap();
    assert_eq!(
        db.workflows()
            .pending(&list, moderator())
            .await
            .unwrap()
            .len(),
        1,
        "defer leaves the request for later"
    );
    assert!(
        actions(&db)
            .await
            .contains(&"subscription.defer".to_owned())
    );

    db.workflows()
        .decide(
            &pending[0].id,
            RequestDecision::Accept,
            "",
            &AuditContext::system(),
        )
        .await
        .unwrap();
    assert_eq!(
        members(&db).await,
        0,
        "an accepted leave removes the member"
    );

    // A second request, discarded, leaves no trace but an audit event.
    add_member(&db, &list, "reader@example.invalid").await;
    db.workflows()
        .request(
            &list,
            "other@example.invalid",
            SubscriptionAction::Join,
            now(),
        )
        .await
        .unwrap();
    let pending = db.workflows().pending(&list, moderator()).await.unwrap();
    assert_eq!(pending.len(), 1);
    db.workflows()
        .decide(
            &pending[0].id,
            RequestDecision::Discard,
            "",
            &AuditContext::system(),
        )
        .await
        .unwrap();
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM subscription_workflows WHERE id=$1")
        .bind(&pending[0].id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(rows, 0);
    assert!(
        actions(&db)
            .await
            .contains(&"subscription.discard".to_owned())
    );
}

#[tokio::test]
async fn a_pending_request_survives_the_confirmation_expiry_sweep() {
    let (db, list) = fixture("moderate", "confirm").await;
    db.workflows()
        .request(
            &list,
            "reader@example.invalid",
            SubscriptionAction::Join,
            now(),
        )
        .await
        .unwrap();
    // Another list's request, far in the future, drives the sweep that
    // deletes expired confirmation rows.
    db.workflows()
        .request(
            &list,
            "later@example.invalid",
            SubscriptionAction::Join,
            now() + 90 * 86_400_000,
        )
        .await
        .unwrap();
    assert_eq!(
        db.workflows()
            .pending(&list, moderator())
            .await
            .unwrap()
            .len(),
        2,
        "moderation requests never expire with confirmation tokens"
    );
}

/// The only readable copy of a token is the queued notice body.
async fn confirmation_token(db: &Database) -> String {
    let raw: Vec<u8> = sqlx::query_scalar(
        "SELECT b.raw FROM workflow_notices n JOIN queue_jobs q ON q.id=n.job_id JOIN messages m ON m.id=q.message_id JOIN message_blobs b ON b.store_key=m.store_key",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let start = text.find("confirm ").expect("confirm token in the notice") + "confirm ".len();
    text[start..start + 43].to_owned()
}
