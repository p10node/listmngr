//! The bounce runner end to end: a report delivered to a VERP bounce
//! address by the LMTP intake is consumed, its member scored and eventually
//! disabled, and an unrecognized report is forwarded to the owners.
// Scores are exact integers stored as doubles; equality is the assertion.
#![allow(clippy::float_cmp)]
use listmngr_core::{Config, MemberRole, SubscriptionMode};
use listmngr_db::{Database, NewList, NewMember};
use listmngr_mail::lmtp::LmtpHandler;
use listmngr_runners::InboundHandler;
use serde_json::json;
use std::time::Duration;

const LIST: &str = "dev.example.invalid";

async fn fixture() -> (Database, InboundHandler) {
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
    let handler = InboundHandler {
        db: db.clone(),
        local_hostname: "fixture.invalid".into(),
        max_message_bytes: 65_536,
        max_recipients: 4,
        command_timeout: Duration::from_secs(3),
        in_max_attempts: 3,
        verp_delimiter: "+".into(),
        structure: listmngr_mail::structure::Limits::default(),
    };
    (db, handler)
}

async fn run_bounces(db: &Database) {
    let config = Config::default();
    let role = listmngr_runners::MailRoleConfig::from_core(&config).unwrap();
    let (shutdown, rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(listmngr_runners::run_bounce_processor(
        db.clone(),
        role,
        "postmaster@example.invalid".into(),
        "bounces-test".into(),
        rx,
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let n: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM queue_jobs WHERE queue='bounces' AND state!='done'",
            )
            .fetch_one(db.pool())
            .await
            .unwrap();
            if n == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the bounce runner finished every job");
    shutdown.send(true).unwrap();
    task.await.unwrap();
}

async fn member_state(db: &Database) -> (f64, String) {
    sqlx::query_as(
        "SELECT m.bounce_score, COALESCE(p.delivery_status,'enabled') FROM members m JOIN preferences p ON p.id=m.preferences_id JOIN addresses a ON a.id=m.address_id WHERE a.email='member@example.net'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap()
}

#[tokio::test]
async fn a_verp_bounce_from_the_mta_disables_the_member_and_tells_the_owner() {
    let (db, mut handler) = fixture().await;
    let raw = b"From: MAILER-DAEMON@mx.example.net\r\nTo: dev-bounces+member=example.net@example.invalid\r\nSubject: Undelivered Mail Returned to Sender\r\nMessage-ID: <dsn-1@mx.example.net>\r\nAuto-Submitted: auto-replied\r\n\r\nUser unknown.\r\n";
    let outcome = handler
        .deliver(
            None,
            &["dev-bounces+member=example.net@example.invalid".into()],
            raw,
        )
        .await;
    assert_eq!(outcome[0].code, 250, "{}", outcome[0].detail);
    let queued: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs WHERE queue='bounces'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(queued, 1);

    run_bounces(&db).await;
    assert_eq!(member_state(&db).await, (0.0, "by_bounces".into()));
    let jobs: Vec<(String, String)> =
        sqlx::query_as("SELECT queue, state FROM queue_jobs ORDER BY id")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert!(
        jobs.contains(&("bounces".into(), "done".into())),
        "{jobs:?}"
    );
    // The owner's disable notice is the one outgoing message.
    let notices: Vec<String> = sqlx::query_scalar(
        "SELECT r.email FROM delivery_recipients r JOIN queue_jobs q ON q.id=r.job_id WHERE q.queue='out'",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(notices, ["owner@example.invalid"]);
    let events: Vec<(String, String)> =
        sqlx::query_as("SELECT recipient, source FROM bounce_events")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(events, [("member@example.net".into(), "verp".into())]);
}

#[tokio::test]
async fn an_unrecognized_report_at_the_bare_bounces_address_reaches_the_owners() {
    let (db, mut handler) = fixture().await;
    let raw = b"From: someone@example.net\r\nTo: dev-bounces@example.invalid\r\nSubject: Out of office\r\nMessage-ID: <ooo@example.net>\r\n\r\nI am away.\r\n";
    let outcome = handler
        .deliver(None, &["dev-bounces@example.invalid".into()], raw)
        .await;
    assert_eq!(outcome[0].code, 250, "{}", outcome[0].detail);
    run_bounces(&db).await;
    assert_eq!(member_state(&db).await, (0.0, "enabled".into()));
    let forwarded: Vec<String> = sqlx::query_scalar(
        "SELECT r.email FROM delivery_recipients r JOIN queue_jobs q ON q.id=r.job_id WHERE q.queue='out'",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(forwarded, ["owner@example.invalid"]);
    let audit: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM audit_log WHERE action IN ('bounce.forward','bounce.process') ORDER BY at,id",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(audit, ["bounce.forward", "bounce.process"]);
    let bounces_done: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM queue_jobs WHERE queue='bounces' AND state='done'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(bounces_done, 1);
}

#[tokio::test]
async fn with_probes_on_the_threshold_probes_and_only_the_probe_bounce_disables() {
    let (db, mut handler) = fixture().await;
    let db = db.with_bounce_probes(true, 7 * 86_400, "{bounces}+{local}={domain}");
    handler.db = db.clone();
    let raw = b"From: MAILER-DAEMON@mx.example.net\r\nTo: dev-bounces+member=example.net@example.invalid\r\nSubject: Undelivered Mail Returned to Sender\r\nMessage-ID: <dsn-2@mx.example.net>\r\nAuto-Submitted: auto-replied\r\n\r\nUser unknown.\r\n";
    assert_eq!(
        handler
            .deliver(
                None,
                &["dev-bounces+member=example.net@example.invalid".into()],
                raw
            )
            .await[0]
            .code,
        250
    );
    run_bounces(&db).await;
    assert_eq!(
        member_state(&db).await,
        (0.0, "enabled".into()),
        "probed, not disabled"
    );
    let (to, sender): (String, Option<String>) = sqlx::query_as(
        "SELECT r.email, n.mail_from FROM workflow_notices n JOIN queue_jobs q ON q.id=n.job_id JOIN delivery_recipients r ON r.job_id=q.id",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(to, "member@example.net");
    let probe_address = sender.expect("the probe's one-time sender");
    assert!(
        probe_address.starts_with("dev-bounces+probe="),
        "{probe_address}"
    );

    // The MTA bounces the probe to its own sender address.
    let raw = format!(
        "From: MAILER-DAEMON@mx.example.net\r\nTo: {probe_address}\r\nSubject: Undelivered Mail Returned to Sender\r\nMessage-ID: <probe-bounce@mx.example.net>\r\nAuto-Submitted: auto-replied\r\n\r\nUser unknown.\r\n"
    );
    let outcome = handler
        .deliver(None, &[probe_address.clone()], raw.as_bytes())
        .await;
    assert_eq!(outcome[0].code, 250, "{}", outcome[0].detail);
    let context: String = sqlx::query_scalar(
        "SELECT m.context FROM messages m JOIN queue_jobs q ON q.message_id=m.id WHERE q.queue='bounces' AND q.state!='done'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(context.contains("probe_token"), "{context}");
    assert!(!context.contains("verp_recipient"), "{context}");
    run_bounces(&db).await;
    assert_eq!(member_state(&db).await, (0.0, "by_bounces".into()));
    let probes: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM bounce_probes")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(probes, 0);
}
