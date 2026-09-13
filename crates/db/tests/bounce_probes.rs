//! Mailman's probe step: at the bounce threshold a member is sent a probe
//! from a one-time VERP address, and only that probe's own bounce disables
//! delivery. Off, the threshold disables at once, as before.
// Scores are exact integers stored as doubles; equality is the assertion.
#![allow(clippy::float_cmp)]
use listmngr_core::{MemberRole, SubscriptionMode};
use listmngr_db::mail_queue::{JobState, NewMessage, Queue};
use listmngr_db::{Database, NewList, NewMember};
use serde_json::json;

const LIST: &str = "dev.example.invalid";
const DAY_MS: i64 = 86_400_000;

async fn fixture(probes: bool) -> Database {
    let mut db = Database::connect("sqlite::memory:", 1).await.unwrap();
    if probes {
        db = db.with_bounce_probes(true, 7 * 86_400, "{bounces}+{local}={domain}");
    }
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
                "process_bounces": true,
                "bounce_score_threshold": 1.0,
                "bounce_notify_owner_on_disable": true,
                "bounce_notify_owner_on_bounce_increment": false,
            }),
        )
        .await
        .unwrap();
    for (email, role) in [
        ("owner@example.invalid", MemberRole::Owner),
        ("member@example.net", MemberRole::Member),
    ] {
        db.members()
            .create(NewMember {
                list_id: list.id.clone(),
                email: email.into(),
                display_name: String::new(),
                role,
                subscription_mode: SubscriptionMode::AsAddress,
            })
            .await
            .unwrap();
    }
    db
}

/// A VERP bounce for the member, processed by the bounce processor.
async fn verp_bounce(db: &Database, now_ms: i64) -> listmngr_db::bounce_processing::Outcome {
    bounce(
        db,
        json!({"list_id": LIST, "envelope_sender": "", "verp_recipient": "member@example.net"}),
        now_ms,
    )
    .await
}

async fn bounce(
    db: &Database,
    context: serde_json::Value,
    now_ms: i64,
) -> listmngr_db::bounce_processing::Outcome {
    let job = db
        .mail_queue()
        .enqueue(
            NewMessage {
                raw: b"From: MAILER-DAEMON@mx.example.net\r\nSubject: failure\r\n\r\nunknown user\r\n"
                    .to_vec(),
                external_id: format!("<b-{}@mx.example.net>", uuid::Uuid::now_v7()),
                context: context.to_string(),
                queue: Queue::Bounces,
                max_attempts: 3,
            },
            now_ms,
        )
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::Bounces, "probe-test", now_ms, 30_000)
        .await
        .unwrap()
        .unwrap();
    let outcome = db
        .bounce_processing()
        .process(&lease, None, "postmaster@example.invalid", now_ms)
        .await
        .unwrap();
    assert_eq!(
        db.mail_queue().job(job.id).await.unwrap().state,
        JobState::Done
    );
    outcome
}

async fn member_state(db: &Database) -> (f64, String) {
    sqlx::query_as(
        "SELECT m.bounce_score, COALESCE(p.delivery_status,'enabled') FROM members m JOIN preferences p ON p.id=m.preferences_id JOIN addresses a ON a.id=m.address_id WHERE a.email='member@example.net'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap()
}

/// `(to, mail_from, subject)` of every queued notice.
async fn notices(db: &Database) -> Vec<(String, Option<String>, String)> {
    let rows: Vec<(Vec<u8>, Option<String>)> = sqlx::query_as(
        "SELECT b.raw, n.mail_from FROM workflow_notices n JOIN queue_jobs q ON q.id=n.job_id JOIN messages m ON m.id=q.message_id JOIN message_blobs b ON b.store_key=m.store_key ORDER BY q.id",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    rows.into_iter()
        .map(|(raw, mail_from)| {
            let header = |name: &str| listmngr_mail::header_value(&raw, name).unwrap_or_default();
            (header("to"), mail_from, header("subject"))
        })
        .collect()
}

#[tokio::test]
async fn the_threshold_sends_a_probe_from_a_one_time_bounce_address_instead_of_disabling() {
    let db = fixture(true).await;
    let outcome = verp_bounce(&db, DAY_MS).await;
    assert_eq!(outcome.scored, vec!["member@example.net".to_owned()]);
    assert_eq!(
        member_state(&db).await,
        (0.0, "enabled".into()),
        "still enabled, score reset for the probe"
    );
    let sent = notices(&db).await;
    assert_eq!(sent.len(), 1, "{sent:?}");
    let (to, mail_from, subject) = &sent[0];
    assert_eq!(to, "member@example.net");
    assert!(subject.contains("probe"), "{subject}");
    let mail_from = mail_from
        .as_deref()
        .expect("a probe carries its own sender");
    assert!(
        mail_from.starts_with("dev-bounces+probe=") && mail_from.ends_with("@example.invalid"),
        "{mail_from}"
    );
    let token = &mail_from["dev-bounces+probe=".len()..mail_from.len() - "@example.invalid".len()];
    assert_eq!(token.len(), 40);
    assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
    let probes: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM bounce_probes")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(probes, 1);
    let stored: String = sqlx::query_scalar("SELECT token_hash FROM bounce_probes")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_ne!(stored, token, "the token is stored hashed");
    let audit: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM audit_log WHERE action LIKE 'bounce.%' ORDER BY at,id",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert!(audit.contains(&"bounce.probe".to_owned()), "{audit:?}");
    assert!(!audit.contains(&"bounce.disable".to_owned()), "{audit:?}");

    // Another bounce the next day sends a second probe rather than piling up.
    verp_bounce(&db, 2 * DAY_MS).await;
    assert_eq!(member_state(&db).await, (0.0, "enabled".into()));
    assert_eq!(notices(&db).await.len(), 2);

    // The probe's own bounce is what disables delivery.
    let outcome = bounce(
        &db,
        json!({"list_id": LIST, "envelope_sender": "", "probe_token": token}),
        2 * DAY_MS + 1000,
    )
    .await;
    assert_eq!(outcome.scored, vec!["member@example.net".to_owned()]);
    assert_eq!(member_state(&db).await, (0.0, "by_bounces".into()));
    let sent = notices(&db).await;
    assert_eq!(sent.len(), 3, "the owner's disable notice: {sent:?}");
    assert_eq!(sent[2].0, "owner@example.invalid");
    let probes: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM bounce_probes")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(probes, 0, "every probe for the member is spent");

    // A replayed or unknown probe bounce changes nothing more.
    let outcome = bounce(
        &db,
        json!({"list_id": LIST, "envelope_sender": "", "probe_token": token}),
        2 * DAY_MS + 2000,
    )
    .await;
    assert!(outcome.scored.is_empty());
    assert!(!outcome.unrecognized, "a probe bounce is never forwarded");
    assert_eq!(notices(&db).await.len(), 3);
}

#[tokio::test]
async fn an_expired_probe_is_inert_and_probes_are_off_by_default() {
    let db = fixture(true).await;
    verp_bounce(&db, DAY_MS).await;
    let (_, mail_from, _) = notices(&db).await.remove(0);
    let mail_from = mail_from.unwrap();
    let token = &mail_from["dev-bounces+probe=".len()..mail_from.len() - "@example.invalid".len()];
    let outcome = bounce(
        &db,
        json!({"list_id": LIST, "envelope_sender": "", "probe_token": token}),
        DAY_MS + 8 * DAY_MS,
    )
    .await;
    assert!(outcome.scored.is_empty());
    assert_eq!(member_state(&db).await.1, "enabled");

    // Without the configuration the threshold disables at once, as before.
    let db = fixture(false).await;
    verp_bounce(&db, DAY_MS).await;
    assert_eq!(member_state(&db).await, (0.0, "by_bounces".into()));
    let probes: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM bounce_probes")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(probes, 0);
}
