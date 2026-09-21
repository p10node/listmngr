//! What was waiting on the old system when it was migrated: a message
//! held for a moderator, and a subscription a moderator had not decided.
use listmngr_db::moderation::Disposition;
use listmngr_db::workflows::{SubscriptionAction, TokenOwner};
use listmngr_db::{AuditContext, Database, ImportedHold, ImportedRequest, NewList};

const RAW: &[u8] = b"From: stranger@example.invalid\r\nTo: dev@example.invalid\r\nSubject: Held over there\r\nMessage-ID: <held@example.invalid>\r\n\r\nbody\r\n";

async fn fixture(db: &Database) {
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    db.lists()
        .create(NewList {
            list_id: "dev.example.invalid".parse().unwrap(),
            display_name: "Dev".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
}

async fn scenario(db: &Database) {
    fixture(db).await;
    let list = "dev.example.invalid".parse().unwrap();
    let held = db
        .moderation()
        .hold_imported_with_context(
            ImportedHold {
                list_id: &list,
                raw: RAW,
                sender: "stranger@example.invalid",
                subject: "Held over there",
                reason: "The message is not from a list member",
                hold_date: 1_600_000_000_000,
            },
            &AuditContext::system(),
        )
        .await
        .unwrap();
    assert_eq!(held.sender, "stranger@example.invalid");
    assert_eq!(held.hold_date, 1_600_000_000_000, "the old hold date");
    assert_eq!(held.disposition, None);
    // It is waiting for a moderator here, with its bytes kept whole.
    let pending = db.moderation().list_pending(&list).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, held.id);
    let stored = db.mail_queue().message(held.message_id).await.unwrap();
    assert_eq!(stored.raw, RAW);
    // Importing the same message again is refused: one hold per message.
    let again = db
        .moderation()
        .hold_imported_with_context(
            ImportedHold {
                list_id: &list,
                raw: RAW,
                sender: "stranger@example.invalid",
                subject: "Held over there",
                reason: "The message is not from a list member",
                hold_date: 1_600_000_000_000,
            },
            &AuditContext::system(),
        )
        .await;
    assert!(again.is_err(), "{again:?}");
    assert_eq!(db.moderation().count_pending(&list).await.unwrap(), 1);
    // The import left an audit event and no moderator notice.
    let actions: Vec<String> =
        sqlx::query_scalar("SELECT DISTINCT action FROM audit_log ORDER BY action")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert!(
        actions.iter().any(|action| action == "moderation.import"),
        "{actions:?}"
    );
    assert!(
        !actions.iter().any(|action| action == "moderation.hold"),
        "an import is not a fresh hold: {actions:?}"
    );
    let notices: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(notices, 0, "nobody is mailed by an import");
    // A moderator can still discard it, as if it had been held here.
    db.moderation()
        .discard(held.id, None, "", 1_600_000_100_000)
        .await
        .unwrap();
    assert_eq!(
        db.moderation().get(held.id).await.unwrap().disposition,
        Some(Disposition::Discarded)
    );
    requests(db).await;
}

/// A subscription waiting for a moderator's decision.
async fn requests(db: &Database) {
    let list = "dev.example.invalid".parse().unwrap();
    let id = db
        .workflows()
        .import_request_with_context(
            ImportedRequest {
                list_id: &list,
                email: "Wanted@Example.invalid",
                display_name: "Wanted",
                action: SubscriptionAction::Join,
                requested_at: 1_600_000_000_000,
            },
            &AuditContext::system(),
        )
        .await
        .unwrap();
    let pending = db
        .workflows()
        .pending(&list, listmngr_db::workflows::RequestFilter::default())
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, id);
    assert_eq!(pending[0].email, "Wanted@Example.invalid");
    assert_eq!(pending[0].display_name, "Wanted");
    assert!(matches!(pending[0].action, SubscriptionAction::Join));
    assert_eq!(
        pending[0].token_owner,
        TokenOwner::Moderator,
        "an imported request is the moderator's to decide"
    );
    assert_eq!(pending[0].requested_at, 1_600_000_000_000);
    // No confirmation token is out: nothing was mailed for it.
    let notices: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(notices, 0);
    // The moderator accepts it, and the address is a member.
    db.workflows()
        .decide(
            &id,
            listmngr_db::workflows::RequestDecision::Accept,
            "",
            &AuditContext::system(),
        )
        .await
        .unwrap();
    let roster = db
        .members()
        .roster(&list, listmngr_core::MemberRole::Member)
        .await
        .unwrap();
    assert_eq!(roster.len(), 1);
    // A banned address is refused, as a fresh request would be.
    db.bans()
        .create(&list, "spammer@example.invalid", &AuditContext::system())
        .await
        .unwrap();
    let refused = db
        .workflows()
        .import_request_with_context(
            ImportedRequest {
                list_id: &list,
                email: "spammer@example.invalid",
                display_name: "",
                action: SubscriptionAction::Join,
                requested_at: 1_600_000_000_000,
            },
            &AuditContext::system(),
        )
        .await;
    assert!(refused.is_err(), "{refused:?}");
}

#[tokio::test]
async fn a_hold_and_a_request_carried_over_wait_for_the_moderator_here() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    scenario(&db).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_imported_moderation_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("imported_moderation")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 2).await.unwrap();
    scenario(&db).await;
    db.pool().close().await;
    schema.drop().await.unwrap();
}
