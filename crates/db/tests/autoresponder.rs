//! Mailman's Automatic Responses: each list address can answer on its own,
//! at most once per writer per grace period, and optionally swallow the
//! original.
use listmngr_core::ResponseAction;
use listmngr_db::autoresponse::ResponseKind;
use listmngr_db::{Database, NewList};
use serde_json::json;

const LIST: &str = "dev.example.invalid";
const DAY_MS: i64 = 86_400_000;

async fn fixture(settings: serde_json::Value) -> (Database, listmngr_core::ListId) {
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
    db.lists().update(&list.id, &settings).await.unwrap();
    (db, list.id)
}

async fn bodies(db: &Database) -> Vec<String> {
    let raws: Vec<Vec<u8>> = sqlx::query_scalar(
        "SELECT b.raw FROM workflow_notices n JOIN queue_jobs q ON q.id=n.job_id JOIN messages m ON m.id=q.message_id JOIN message_blobs b ON b.store_key=m.store_key ORDER BY q.id",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    raws.into_iter()
        .map(|raw| String::from_utf8_lossy(&raw).into_owned())
        .collect()
}

#[tokio::test]
async fn a_list_that_answers_nothing_sends_nothing() {
    let (db, list) = fixture(json!({})).await;
    for kind in [
        ResponseKind::Owner,
        ResponseKind::Postings,
        ResponseKind::Requests,
    ] {
        assert_eq!(
            db.autoresponse()
                .respond(&list, kind, "writer@example.net", 0)
                .await
                .unwrap(),
            ResponseAction::None
        );
    }
    assert!(bodies(&db).await.is_empty());
}

#[tokio::test]
async fn the_owner_address_answers_once_per_grace_period_and_per_writer() {
    let (db, list) = fixture(json!({
        "autorespond_owner": "respond",
        "autoresponse_owner_text": "The $listname owners read mail weekly.",
        "autoresponse_grace_period": 2,
    }))
    .await;
    let respond = |email: &'static str, now: i64| {
        let db = db.clone();
        let list = list.clone();
        async move {
            db.autoresponse()
                .respond(&list, ResponseKind::Owner, email, now)
                .await
                .unwrap()
        }
    };
    assert_eq!(
        respond("writer@example.net", DAY_MS).await,
        ResponseAction::Respond
    );
    let sent = bodies(&db).await;
    assert_eq!(sent.len(), 1);
    assert!(
        sent[0].contains("The dev@example.invalid owners read mail weekly."),
        "the configured text is expanded: {}",
        sent[0]
    );
    assert!(sent[0].contains("To: writer@example.net"));
    assert!(
        sent[0].contains("Auto-Submitted: auto-replied"),
        "{}",
        sent[0]
    );

    // Inside the grace period the same writer is not answered again.
    assert_eq!(
        respond("writer@example.net", DAY_MS + DAY_MS).await,
        ResponseAction::Respond
    );
    assert_eq!(bodies(&db).await.len(), 1);
    // Another writer is answered at once.
    assert_eq!(
        respond("other@example.net", DAY_MS).await,
        ResponseAction::Respond
    );
    assert_eq!(bodies(&db).await.len(), 2);
    // Past the grace period the first writer is answered again.
    assert_eq!(
        respond("writer@example.net", DAY_MS + 3 * DAY_MS).await,
        ResponseAction::Respond
    );
    assert_eq!(bodies(&db).await.len(), 3);
}

#[tokio::test]
async fn discarding_keeps_discarding_even_when_the_grace_period_is_quiet() {
    let (db, list) = fixture(json!({
        "autorespond_requests": "respond_and_discard",
        "autoresponse_grace_period": 90,
    }))
    .await;
    for expected_notices in [1, 1] {
        assert_eq!(
            db.autoresponse()
                .respond(&list, ResponseKind::Requests, "writer@example.net", DAY_MS)
                .await
                .unwrap(),
            ResponseAction::RespondAndDiscard,
            "the original is swallowed whether or not a reply went out"
        );
        assert_eq!(bodies(&db).await.len(), expected_notices);
    }
    // With no configured text the built-in body is used.
    let sent = bodies(&db).await;
    assert!(sent[0].contains("automatic reply"), "{}", sent[0]);
    let audit: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='list.autoresponse'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(audit, 1);
}

#[tokio::test]
async fn a_zero_grace_period_answers_every_message_and_each_kind_counts_separately() {
    let (db, list) = fixture(json!({
        "autorespond_postings": "respond",
        "autorespond_owner": "respond",
        "autoresponse_grace_period": 0,
    }))
    .await;
    for _ in 0..3 {
        assert_eq!(
            db.autoresponse()
                .respond(&list, ResponseKind::Postings, "writer@example.net", DAY_MS)
                .await
                .unwrap(),
            ResponseAction::Respond
        );
    }
    assert_eq!(bodies(&db).await.len(), 3);
    // The owner address keeps its own record for the same writer.
    assert_eq!(
        db.autoresponse()
            .respond(&list, ResponseKind::Owner, "writer@example.net", DAY_MS)
            .await
            .unwrap(),
        ResponseAction::Respond
    );
    assert_eq!(bodies(&db).await.len(), 4);
    // With no grace period every record is already stale: the table is
    // pruned as it goes and never accumulates.
    let records: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM autoresponse_records")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(records <= 2, "{records}");
}

#[tokio::test]
async fn an_unusable_writer_address_is_never_answered() {
    let (db, list) = fixture(json!({"autorespond_owner": "respond_and_discard"})).await;
    for address in ["", "not-an-address", "dev@example.invalid"] {
        assert_eq!(
            db.autoresponse()
                .respond(&list, ResponseKind::Owner, address, DAY_MS)
                .await
                .unwrap(),
            ResponseAction::RespondAndDiscard,
            "{address}"
        );
    }
    assert!(
        bodies(&db).await.is_empty(),
        "no reply to a null sender, a malformed address or the list itself"
    );
}
