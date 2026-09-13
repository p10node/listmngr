//! A bounce probe leaves the out runner with its one-time VERP sender as
//! the envelope, not the null reverse path other notices use.
use super::*;
use crate::outbound::tests::capture_envelope;
use listmngr_db::mail_queue::{JobState, NewMessage, Queue};

#[tokio::test]
async fn a_probe_is_sent_from_its_own_bounce_address() {
    let db = Database::connect("sqlite::memory:", 1)
        .await
        .unwrap()
        .with_bounce_probes(true, 7 * 86_400, "{bounces}+{local}={domain}");
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    let list: listmngr_core::ListId = "test.example.invalid".parse().unwrap();
    db.lists()
        .create(listmngr_db::NewList {
            list_id: list.clone(),
            display_name: "Test".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    db.lists()
        .update(
            &list,
            &serde_json::json!({"process_bounces": true, "bounce_score_threshold": 1.0, "bounce_notify_owner_on_bounce_increment": false}),
        )
        .await
        .unwrap();
    db.members()
        .create(listmngr_db::NewMember {
            list_id: list.clone(),
            email: "member@example.net".into(),
            display_name: String::new(),
            role: listmngr_core::MemberRole::Member,
            subscription_mode: listmngr_core::SubscriptionMode::AsAddress,
        })
        .await
        .unwrap();
    // A VERP bounce reaches the threshold and queues the probe.
    let now = chrono::Utc::now().timestamp_millis();
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: b"Subject: failure\r\n\r\nunknown user\r\n".to_vec(),
                external_id: "<b@mx.example.net>".into(),
                context: serde_json::json!({"list_id": "test.example.invalid", "envelope_sender": "", "verp_recipient": "member@example.net"}).to_string(),
                queue: Queue::Bounces,
                max_attempts: 3,
            },
            now,
        )
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::Bounces, "probe", now, 30_000)
        .await
        .unwrap()
        .unwrap();
    db.bounce_processing()
        .process(&lease, None, "postmaster@example.invalid", now)
        .await
        .unwrap();
    let probe = db
        .mail_queue()
        .claim(Queue::Out, "out", now, 30_000)
        .await
        .unwrap()
        .expect("the probe is queued");
    let sender = db
        .workflows()
        .notice_sender(probe.job.id)
        .await
        .unwrap()
        .expect("the probe carries its sender");
    assert!(sender.starts_with("test-bounces+probe="), "{sender}");

    let sink = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut role = MailRoleConfig::from_core(&tests::plaintext_config()).unwrap();
    role.smtp_relay = sink.local_addr().unwrap();
    role.command_timeout = Duration::from_secs(2);
    let recipients = vec!["member@example.net".to_owned()];
    let (data, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            capture_envelope(&sink, &sender, &recipients),
            deliver_one(&db, &role, probe.clone())
        )
    })
    .await
    .unwrap();
    let text = String::from_utf8_lossy(&data);
    assert!(text.contains("This is a probe message"), "{text}");
    assert!(text.contains(&format!("From: {sender}")), "{text}");
    assert_eq!(
        db.mail_queue().job(probe.job.id).await.unwrap().state,
        JobState::Done
    );
}
