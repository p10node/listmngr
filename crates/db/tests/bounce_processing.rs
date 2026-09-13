//! The bounce processor: what arrives at `list-bounces@` names a member
//! through VERP or a DSN, raises their score through the same rules as an
//! SMTP-time failure, and anything it cannot attribute goes where the list
//! says unrecognized bounces go.
// Scores are exact integers stored as doubles; equality is the assertion.
#![allow(clippy::float_cmp)]
use listmngr_core::{MemberRole, SubscriptionMode};
use listmngr_db::bounce_processing::Outcome;
use listmngr_db::mail_queue::{JobState, NewMessage, Queue};
use listmngr_db::{Database, NewList, NewMember};
use serde_json::json;

const LIST: &str = "dev.example.invalid";

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
    let mut settings = settings;
    settings["process_bounces"] = json!(true);
    settings["bounce_score_threshold"] = json!(2.0);
    settings["bounce_notify_owner_on_bounce_increment"] = json!(false);
    settings["bounce_notify_owner_on_disable"] = json!(false);
    db.lists().update(&list.id, &settings).await.unwrap();
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
    (db, list.id)
}

fn dsn(final_recipient: &str, action: &str, status: &str, envid: Option<&str>) -> Vec<u8> {
    let envelope = envid.map_or(String::new(), |id| {
        format!("Original-Envelope-Id: {id}\r\n")
    });
    format!(
        "From: MAILER-DAEMON@mx.example.net\r\nTo: dev-bounces@example.invalid\r\nSubject: Undelivered Mail Returned to Sender\r\nMessage-ID: <{}@mx.example.net>\r\nAuto-Submitted: auto-replied\r\nContent-Type: multipart/report; report-type=delivery-status; boundary=r\r\n\r\n--r\r\nContent-Type: text/plain\r\n\r\nThis is the mail system.\r\n--r\r\nContent-Type: message/delivery-status\r\n\r\nReporting-MTA: dns; mx.example.net\r\n{envelope}\r\nFinal-Recipient: rfc822; {final_recipient}\r\nAction: {action}\r\nStatus: {status}\r\n--r--\r\n",
        uuid::Uuid::now_v7()
    )
    .into_bytes()
}

/// Queue a bounce the way the LMTP intake does, and process it.
async fn bounce(
    db: &Database,
    raw: Vec<u8>,
    verp_recipient: Option<&str>,
    now_ms: i64,
) -> (Outcome, JobState) {
    let mut context = json!({"list_id": LIST, "envelope_sender": ""});
    if let Some(recipient) = verp_recipient {
        context["verp_recipient"] = json!(recipient);
    }
    let job = db
        .mail_queue()
        .enqueue(
            NewMessage {
                raw,
                external_id: format!("<bounce-{}@mx.example.net>", uuid::Uuid::now_v7()),
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
        .claim(Queue::Bounces, "bounce-test", now_ms, 30_000)
        .await
        .unwrap()
        .unwrap();
    let outcome = db
        .bounce_processing()
        .process(&lease, None, "postmaster@example.invalid", now_ms)
        .await
        .unwrap();
    (outcome, db.mail_queue().job(job.id).await.unwrap().state)
}

async fn score(db: &Database) -> (f64, String) {
    sqlx::query_as(
        "SELECT m.bounce_score, COALESCE(p.delivery_status,'enabled') FROM members m JOIN preferences p ON p.id=m.preferences_id JOIN addresses a ON a.id=m.address_id WHERE a.email='member@example.net'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap()
}

async fn events(db: &Database) -> Vec<(String, String, i64)> {
    sqlx::query_as("SELECT recipient, source, processed FROM bounce_events ORDER BY created_at")
        .fetch_all(db.pool())
        .await
        .unwrap()
}

const DAY_MS: i64 = 86_400_000;

#[tokio::test]
async fn a_verp_bounce_names_its_member_and_scores_them() {
    let (db, _) = fixture(json!({})).await;
    let (outcome, state) = bounce(
        &db,
        b"From: MAILER-DAEMON@mx.example.net\r\nSubject: failure\r\n\r\nunknown user\r\n".to_vec(),
        Some("member@example.net"),
        DAY_MS,
    )
    .await;
    assert_eq!(outcome.scored, vec!["member@example.net".to_owned()]);
    assert!(!outcome.unrecognized);
    assert_eq!(state, JobState::Done);
    assert_eq!(score(&db).await, (1.0, "enabled".into()));
    assert_eq!(
        events(&db).await,
        vec![("member@example.net".to_owned(), "verp".to_owned(), 1)]
    );

    // A second bounce the same day does not count twice; the next day it
    // reaches the threshold and delivery is disabled.
    bounce(
        &db,
        b"Subject: again\r\n\r\n".to_vec(),
        Some("member@example.net"),
        DAY_MS + 1,
    )
    .await;
    assert_eq!(score(&db).await.0, 1.0);
    bounce(
        &db,
        b"Subject: again\r\n\r\n".to_vec(),
        Some("member@example.net"),
        2 * DAY_MS,
    )
    .await;
    assert_eq!(score(&db).await, (0.0, "by_bounces".into()));
    let audit: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM audit_log WHERE action LIKE 'bounce.%' ORDER BY at,id",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert!(audit.contains(&"bounce.process".to_owned()), "{audit:?}");
    assert!(audit.contains(&"bounce.disable".to_owned()), "{audit:?}");
}

#[tokio::test]
async fn a_dsn_names_failed_recipients_and_ignores_delays() {
    let (db, _) = fixture(json!({})).await;
    let (outcome, _) = bounce(
        &db,
        dsn("member@example.net", "failed", "5.1.1", None),
        None,
        DAY_MS,
    )
    .await;
    assert_eq!(outcome.scored, vec!["member@example.net".to_owned()]);
    assert_eq!(score(&db).await.0, 1.0);
    assert_eq!(events(&db).await[0].1, "dsn");

    // A delay is not a bounce: nothing scored, nothing forwarded.
    let (outcome, state) = bounce(
        &db,
        dsn("member@example.net", "delayed", "4.4.1", None),
        None,
        2 * DAY_MS,
    )
    .await;
    assert!(outcome.scored.is_empty());
    assert!(
        !outcome.unrecognized,
        "a delay is recognized, just not actionable"
    );
    assert_eq!(state, JobState::Done);
    assert_eq!(score(&db).await.0, 1.0);

    // A failed recipient who is not a member is recognized but not scored.
    let (outcome, _) = bounce(
        &db,
        dsn("stranger@example.net", "failed", "5.1.1", None),
        None,
        2 * DAY_MS,
    )
    .await;
    assert!(outcome.scored.is_empty());
    assert!(!outcome.unrecognized);
    assert_eq!(events(&db).await.len(), 1, "no event for a stranger");
}

#[tokio::test]
async fn an_unrecognized_bounce_goes_where_the_list_says() {
    let junk = b"From: someone@example.net\r\nSubject: not a bounce\r\n\r\nhello\r\n".to_vec();
    for (disposition, expected_recipients) in [
        ("administrators", vec!["owner@example.invalid"]),
        ("site_owner", vec!["postmaster@example.invalid"]),
        ("discard", vec![]),
    ] {
        let (db, _) = fixture(json!({"forward_unrecognized_bounces_to": disposition})).await;
        let (outcome, state) = bounce(&db, junk.clone(), None, DAY_MS).await;
        assert!(outcome.unrecognized, "{disposition}");
        assert!(outcome.scored.is_empty());
        assert_eq!(state, JobState::Done);
        let forwarded: Vec<String> = sqlx::query_scalar(
            "SELECT r.email FROM delivery_recipients r JOIN queue_jobs q ON q.id=r.job_id WHERE q.queue='out' ORDER BY r.email",
        )
        .fetch_all(db.pool())
        .await
        .unwrap();
        assert_eq!(forwarded, expected_recipients, "{disposition}");
        assert_eq!(score(&db).await.0, 0.0, "{disposition}");
    }
}

#[tokio::test]
async fn a_list_that_does_not_process_bounces_records_nothing() {
    let (db, list) = fixture(json!({})).await;
    db.lists()
        .update(&list, &json!({"process_bounces": false}))
        .await
        .unwrap();
    let (outcome, state) = bounce(
        &db,
        dsn("member@example.net", "failed", "5.1.1", None),
        None,
        DAY_MS,
    )
    .await;
    assert_eq!(state, JobState::Done);
    assert!(outcome.scored.is_empty());
    assert!(!outcome.unrecognized);
    assert_eq!(score(&db).await.0, 0.0);
}

#[tokio::test]
async fn a_bounce_for_a_disabled_member_changes_nothing_more() {
    let (db, _) = fixture(json!({})).await;
    sqlx::query("UPDATE preferences SET delivery_status='by_moderator' WHERE id=(SELECT m.preferences_id FROM members m JOIN addresses a ON a.id=m.address_id WHERE a.email='member@example.net')")
        .execute(db.pool())
        .await
        .unwrap();
    let (outcome, state) = bounce(
        &db,
        dsn("member@example.net", "failed", "5.1.1", None),
        None,
        DAY_MS,
    )
    .await;
    assert_eq!(state, JobState::Done);
    assert!(outcome.scored.is_empty());
    assert!(!outcome.unrecognized);
    assert_eq!(score(&db).await, (0.0, "by_moderator".into()));
}

fn issuer(dir: &std::path::Path) -> listmngr_core::dsn_issuance::Issuer {
    let key = dir.join("issuer.key");
    std::fs::write(&key, [42_u8; 32]).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let config: listmngr_core::Config = serde_json::from_value(json!({"mta":{"smtp_tls":"plaintext_trusted_relay","smtp_single_recipient":true,"dsn_issuance_enabled":true,"dsn_key_file":key,"dsn_key_id":"test"}})).unwrap();
    listmngr_core::dsn_issuance::Issuer::load(&config.mta)
        .unwrap()
        .unwrap()
}

/// A stored issuance for `recipient`, exactly as the out runner records
/// one before `MAIL FROM`, with the MAC the issuer produces.
async fn issued_envid(
    db: &Database,
    issuer: &listmngr_core::dsn_issuance::Issuer,
    recipient: &str,
    issued_at: i64,
) -> String {
    let job = db
        .mail_queue()
        .enqueue(
            NewMessage {
                raw: b"Subject: post\r\n\r\nbody\r\n".to_vec(),
                external_id: format!("<post-{}@example.invalid>", uuid::Uuid::now_v7()),
                context: json!({"list_id": LIST}).to_string(),
                queue: Queue::Out,
                max_attempts: 3,
            },
            issued_at,
        )
        .await
        .unwrap();
    let nonce = uuid::Uuid::from_bytes(rand::random()).simple().to_string();
    let claims = json!({"version":1,"nonce":nonce,"recipient":recipient}).to_string();
    let envid = issuer.issue(&claims, &nonce);
    sqlx::query("INSERT INTO dsn_issuances(id,job_id,message_id,recipient,attempt_token,claims,envid,issued_at,expires_at) VALUES($1,$2,$3,$4,'attempt',$5,$6,$7,$8)")
        .bind(&nonce)
        .bind(job.id.0.to_string())
        .bind(job.message_id.0.to_string())
        .bind(recipient)
        .bind(&claims)
        .bind(&envid)
        .bind(issued_at)
        .bind(issued_at + 7 * DAY_MS)
        .execute(db.pool())
        .await
        .unwrap();
    envid
}

#[tokio::test]
async fn a_verified_envelope_id_names_the_recipient_the_report_cannot_forge() {
    let dir = tempfile::tempdir().unwrap();
    let issuer = issuer(dir.path());
    let (db, _) = fixture(json!({})).await;
    let envid = issued_envid(&db, &issuer, "member@example.net", DAY_MS).await;

    // The report claims somebody else failed; the ENVID says who it was for.
    let raw = dsn("stranger@example.net", "failed", "5.1.1", Some(&envid));
    let job = db
        .mail_queue()
        .enqueue(
            NewMessage {
                raw,
                external_id: "<bounce-envid@mx.example.net>".into(),
                context: json!({"list_id": LIST, "envelope_sender": ""}).to_string(),
                queue: Queue::Bounces,
                max_attempts: 3,
            },
            DAY_MS + 1000,
        )
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::Bounces, "bounce-test", DAY_MS + 1000, 30_000)
        .await
        .unwrap()
        .unwrap();
    let outcome = db
        .bounce_processing()
        .process(
            &lease,
            Some(&issuer),
            "postmaster@example.invalid",
            DAY_MS + 1000,
        )
        .await
        .unwrap();
    assert_eq!(outcome.scored, vec!["member@example.net".to_owned()]);
    assert_eq!(events(&db).await[0].1, "dsn_envid");
    assert_eq!(
        db.mail_queue().job(job.id).await.unwrap().state,
        JobState::Done
    );

    // A tampered or unknown ENVID falls back to the report's own claims.
    let forged = format!("{}x", &envid[..envid.len() - 1]);
    let raw = dsn("member@example.net", "failed", "5.1.1", Some(&forged));
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw,
                external_id: "<bounce-forged@mx.example.net>".into(),
                context: json!({"list_id": LIST, "envelope_sender": ""}).to_string(),
                queue: Queue::Bounces,
                max_attempts: 3,
            },
            2 * DAY_MS,
        )
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::Bounces, "bounce-test", 2 * DAY_MS, 30_000)
        .await
        .unwrap()
        .unwrap();
    let outcome = db
        .bounce_processing()
        .process(
            &lease,
            Some(&issuer),
            "postmaster@example.invalid",
            2 * DAY_MS,
        )
        .await
        .unwrap();
    assert_eq!(outcome.scored, vec!["member@example.net".to_owned()]);
    assert_eq!(events(&db).await[1].1, "dsn");
}

#[tokio::test]
async fn an_mta_that_writes_prose_is_read_by_the_detectors() {
    let (db, _) = fixture(json!({})).await;
    let raw = b"From: MAILER-DAEMON@mx.example.net\r\nTo: dev-bounces@example.invalid\r\nSubject: failure notice\r\nMessage-ID: <qmail@mx.example.net>\r\n\r\nHi. This is the qmail-send program at mx.example.net.\r\nI'm afraid I wasn't able to deliver your message to the following addresses.\r\nThis is a permanent error; I've given up. Sorry it didn't work out.\r\n\r\n<member@example.net>:\r\nSorry, no mailbox here by that name. (#5.1.1)\r\n\r\n--- Below this line is a copy of the message.\r\n".to_vec();
    let (outcome, state) = bounce(&db, raw, None, DAY_MS).await;
    assert_eq!(outcome.scored, vec!["member@example.net".to_owned()]);
    assert!(!outcome.unrecognized);
    assert_eq!(state, JobState::Done);
    assert_eq!(events(&db).await[0].1, "heuristic");

    // A delay written as prose is recognized and inert, like a DSN delay.
    let raw = b"From: MAILER-DAEMON@mx.example.net\r\nSubject: Warning: could not send message for past 4 hours\r\nMessage-ID: <w@mx.example.net>\r\n\r\n    **      THIS IS A WARNING MESSAGE ONLY      **\r\n\r\nYour message to <member@example.net> has not yet been delivered; it will be retried.\r\n".to_vec();
    let (outcome, _) = bounce(&db, raw, None, 2 * DAY_MS).await;
    assert!(outcome.scored.is_empty());
    assert!(!outcome.unrecognized);
    assert_eq!(score(&db).await.0, 1.0);
}
