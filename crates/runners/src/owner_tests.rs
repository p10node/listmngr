use super::*;
use listmngr_core::{MemberRole, SubscriptionMode};
use listmngr_db::{NewList, NewMember, mail_queue::NewMessage};

async fn fixture() -> Database {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains().create("example.com", "", None).await.unwrap();
    let list = "test.example.com".parse().unwrap();
    db.lists()
        .create(NewList {
            list_id: list,
            display_name: "Test".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let list = "test.example.com".parse().unwrap();
    db.lists()
        .update(
            &list,
            &serde_json::json!({"anonymous_list": true, "subject_prefix": "[POST] ", "dmarc_mitigate_action":"munge_from", "dmarc_mitigate_unconditionally":true}),
        )
        .await
        .unwrap();
    db.members()
        .create(NewMember {
            list_id: list,
            email: "OwnerCase@example.net".into(),
            display_name: String::new(),
            role: MemberRole::Owner,
            subscription_mode: SubscriptionMode::AsUser,
        })
        .await
        .unwrap();
    db
}

#[tokio::test]
async fn owner_preparation_requires_producer_provenance_and_sanitizes_without_post_cook() {
    let db = fixture().await;
    let raw = b"From: Author@example.net\r\nMessage-ID: <private@example.net>\r\nSubject: Question\r\nBcc: private@example.net\r\nApproved: secret\r\nDKIM-Signature: stale\r\nAuthentication-Results: attacker; dmarc=pass\r\nAuto-Submitted: no\r\nSender: forged@example.net\r\n\r\nbody\xff";
    let context = serde_json::json!({"list_id":"test.example.com", "owner_route":true, "owner":true, "envelope_sender":"Author@example.net"}).to_string();
    let now = chrono::Utc::now().timestamp_millis();
    // A forged durable JSON marker is not an outgoing capability.
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: raw.to_vec(),
                external_id: "private@example.net".into(),
                context: context.clone(),
                queue: Queue::Out,
                max_attempts: 3,
            },
            now,
        )
        .await
        .unwrap();
    let forged = db
        .mail_queue()
        .claim(Queue::Out, "forged", now, 30000)
        .await
        .unwrap()
        .unwrap();
    let (cooked, sender) = prepare_delivery(&db, &forged, raw, &context).await.unwrap();
    assert_eq!(sender, "test-bounces@example.com");
    assert!(String::from_utf8_lossy(&cooked).contains("Subject: [POST] Question"));
    assert!(!String::from_utf8_lossy(&cooked).contains("Author@example.net"));
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: raw.to_vec(),
                external_id: "private@example.net".into(),
                context: context.clone(),
                queue: Queue::In,
                max_attempts: 3,
            },
            now,
        )
        .await
        .unwrap();
    let inbound = db
        .mail_queue()
        .claim(Queue::In, "producer", now, 30000)
        .await
        .unwrap()
        .unwrap();
    db.owner_mail().forward(&inbound, 3, now).await.unwrap();
    let owner = db
        .mail_queue()
        .claim(Queue::Out, "owner", now, 30000)
        .await
        .unwrap()
        .unwrap();
    let (cooked, sender) = prepare_delivery(&db, &owner, raw, &context).await.unwrap();
    assert_eq!(sender, "", "owner forwarding needs a null reverse path");
    let text = String::from_utf8_lossy(&cooked);
    assert!(text.contains("From: Author@example.net"));
    assert!(text.contains("Subject: Question"));
    for forbidden in [
        "[POST]",
        "Bcc:",
        "Approved:",
        "DKIM-Signature:",
        "List-Post:",
        "Authentication-Results:",
        "Auto-Submitted: no",
        "Sender:",
    ] {
        assert!(!text.contains(forbidden), "{forbidden}");
    }
    assert!(text.contains("Auto-Submitted: auto-forwarded"));
    assert!(cooked.ends_with(b"body\xff"));
    let keys = tempfile::tempdir().unwrap();
    let key = dkim_tests::key(keys.path());
    let mut role =
        MailRoleConfig::from_core(&dkim_tests::signing_config(&key, "example.com")).unwrap();
    let sink = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    role.smtp_relay = sink.local_addr().unwrap();
    let (sent, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            dkim_tests::capture(&sink, "", "OwnerCase@example.net"),
            deliver_one(&db, &role, owner)
        )
    })
    .await
    .unwrap();
    assert!(dkim_tests::oracle::passes(&sent, &dkim_tests::oracle::public_txt(&key)).await);
    assert!(sent.ends_with(b"body\xff\r\n"));
    assert!(listmngr_mail::header_value(&sent, "List-Post").is_none());
    dkim_tests::oracle::export_capture("owner", &sent, &key, "example.com");
}
