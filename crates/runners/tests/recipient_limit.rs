use listmngr_core::{Config, MemberRole, SubscriptionMode};
use listmngr_db::{
    Database, NewList, NewMember,
    mail_queue::{JobState, NewMessage, Queue},
};
use serde_json::json;
use std::time::Duration;

async fn fixture(limit: u32) -> (Database, listmngr_core::ListId) {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: "size.example.invalid".parse().unwrap(),
            display_name: "Size".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    // The author posts as an ordinary member: owners are explicitly accepted
    // and, as in Mailman, bypass the deferred checks this test exercises. The
    // destination rule is out of scope here.
    db.lists()
        .update(
            &list.id,
            &json!({"max_num_recipients":limit, "require_explicit_destination": false}),
        )
        .await
        .unwrap();
    for (email, role) in [
        ("author@example.invalid", MemberRole::Member),
        ("reader@example.invalid", MemberRole::Member),
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

async fn post(limit: u32, headers: &str) -> (i64, i64) {
    let (db, list_id) = fixture(limit).await;
    let raw =
        format!("From: author@example.invalid\r\nSubject: Recipients\r\n{headers}\r\nbody\r\n")
            .into_bytes();
    let job = db
        .mail_queue()
        .enqueue(
            NewMessage {
                raw: raw.clone(),
                external_id: "size@example.invalid".into(),
                context: json!({"list_id":list_id,"envelope_sender":"author@example.invalid"})
                    .to_string(),
                queue: Queue::In,
                max_attempts: 3,
            },
            chrono::Utc::now().timestamp_millis(),
        )
        .await
        .unwrap();
    let config = Config::default();
    let role = listmngr_runners::MailRoleConfig::from_core(&config).unwrap();
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let mut worker = tokio::spawn(listmngr_runners::run_in_processor(
        db.clone(),
        config,
        role,
        "size-fixture".into(),
        receiver,
    ));
    let completed = tokio::time::timeout(Duration::from_secs(5), async {
        while db.mail_queue().job(job.id).await.unwrap().state != JobState::Done {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    stop.send(true).unwrap();
    if let Ok(result) = tokio::time::timeout(Duration::from_secs(2), &mut worker).await {
        result.unwrap();
    } else {
        worker.abort();
        let _ = worker.await;
        panic!("in runner did not stop");
    }
    completed.unwrap();
    assert_eq!(
        db.mail_queue().message(job.message_id).await.unwrap().raw,
        raw
    );
    let held: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM held_messages")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let outgoing: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs WHERE queue='out' AND id NOT IN (SELECT job_id FROM workflow_notices)")
        .fetch_one(db.pool())
        .await
        .unwrap();
    if held > 0 {
        let reason: String = sqlx::query_scalar("SELECT reason FROM held_messages")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert!(reason.contains("max_num_recipients"));
        let children: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs WHERE queue!='in' AND id NOT IN (SELECT job_id FROM workflow_notices)")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(
            children, 0,
            "held oversized mail must not archive, digest or deliver"
        );
    }
    (held, outgoing)
}

#[tokio::test]
async fn recipient_limit_holds_above_visible_count() {
    assert_eq!(
        post(1, "To: a@example.invalid, b@example.invalid\r\n").await,
        (1, 0)
    );
}

#[tokio::test]
async fn recipient_limit_malformed_visible_headers_fail_closed() {
    for headers in [
        "To: a@example.invalid (unterminated, b@example.invalid\r\n",
        "To: a@example.invalid, not a mailbox\r\n",
        "To: <a@example.invalid, b@example.invalid\r\n",
        "To: \"unterminated a@example.invalid, b@example.invalid\r\n",
        "To: group: a@example.invalid, b@example.invalid\r\n",
        "Broken header\r\nTo: a@example.invalid, b@example.invalid\r\n",
    ] {
        assert_eq!(post(10, headers).await, (1, 0), "{headers}");
    }
}

#[tokio::test]
async fn recipient_limit_counts_groups_repeated_headers_and_not_delivery_roster() {
    let headers = "To: \"Doe, Jane\" <a@example.invalid> (comment, ignored)\r\nTo: team: b@example.invalid,\r\n c@example.invalid;\r\nCc: d@example.invalid\r\nCc: d@example.invalid\r\nBcc: secret@example.invalid\r\nReply-To: other@example.invalid\r\n";
    assert_eq!(post(4, headers).await, (1, 0));
    // Mailman's compatibility rule holds at equality, not only above the limit.
    assert_eq!(post(5, headers).await, (1, 0));
    assert_eq!(post(6, headers).await, (0, 1));
    assert_eq!(post(0, headers).await, (0, 1));
    assert_eq!(post(2_147_483_647, headers).await, (0, 1));
    assert_eq!(post(1, "To: undisclosed:;\r\nCc: \r\n").await, (0, 1));
    assert_eq!(post(1, "").await, (0, 1));
}
