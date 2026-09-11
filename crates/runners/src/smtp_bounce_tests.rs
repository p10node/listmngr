use super::*;

async fn stage_tracer(stage: &str) {
    let raw = b"From: author@example.invalid\r\n\r\nbody";
    let (db, lease, role, sink) = tests::fixture_at(
        "sqlite::memory:",
        "{\"list_id\":\"test.example.invalid\"}",
        raw,
        vec![
            "Mixed@Example.invalid".into(),
            "ZLater@Example.invalid".into(),
        ],
    )
    .await;
    db.lists()
        .update(
            &"test.example.invalid".parse().unwrap(),
            &serde_json::json!({"process_bounces":true}),
        )
        .await
        .unwrap();
    db.members()
        .create(listmngr_db::NewMember {
            list_id: "test.example.invalid".parse().unwrap(),
            email: "Mixed@Example.invalid".into(),
            display_name: String::new(),
            role: listmngr_core::MemberRole::Member,
            subscription_mode: listmngr_core::SubscriptionMode::AsAddress,
        })
        .await
        .unwrap();
    let peer = async {
        let (stream, _) = sink.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut read = BufReader::new(read);
        write.write_all(b"220 fixture\r\n").await.unwrap();
        for (command_stage, prefix, success) in [
            ("ehlo", "EHLO", "250 ok\r\n"),
            ("mail_from", "MAIL FROM:", "250 ok\r\n"),
            (
                "rcpt",
                "RCPT TO:<Mixed@Example.invalid>",
                "550 same private policy diagnostic\r\n",
            ),
            ("rcpt", "RCPT TO:<ZLater@Example.invalid>", "250 ok\r\n"),
            ("data_start", "DATA", "354 go\r\n"),
            ("data_final", ".", "250 ok\r\n"),
        ] {
            let mut line = String::new();
            loop {
                line.clear();
                assert!(read.read_line(&mut line).await.unwrap() > 0);
                if prefix != "." || line == ".\r\n" {
                    break;
                }
            }
            assert!(line.starts_with(prefix), "{line}");
            let response = if command_stage == stage {
                "554 same private policy diagnostic\r\n"
            } else {
                success
            };
            write.write_all(response.as_bytes()).await.unwrap();
            if command_stage == stage {
                break;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(peer, deliver_one(&db, &role, lease))
    })
    .await
    .unwrap();
    let events = db
        .bounces()
        .list(&"test.example.invalid".parse().unwrap(), 100, 0)
        .await
        .unwrap();
    assert_eq!(events.len(), 2);
    let score: f64 = sqlx::query_scalar("SELECT bounce_score FROM members WHERE role='member'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!((score - if stage.starts_with("data_") { 1.0 } else { 0.0 }).abs() < f64::EPSILON);
    for event in events {
        let retained = stage.starts_with("data_") && event.recipient == "Mixed@Example.invalid";
        let value = serde_json::to_value(event).unwrap();
        assert_eq!(value["smtp_stage"], if retained { "rcpt" } else { stage });
        assert_eq!(value["smtp_code"], if retained { 550 } else { 554 });
    }
}

#[tokio::test]
async fn smtp_stage_ehlo() {
    stage_tracer("ehlo").await;
}
#[tokio::test]
async fn smtp_stage_mail_from() {
    stage_tracer("mail_from").await;
}
#[tokio::test]
async fn smtp_stage_data_start_retains_rcpt() {
    stage_tracer("data_start").await;
}
#[tokio::test]
async fn smtp_stage_data_final_retains_rcpt() {
    stage_tracer("data_final").await;
}

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::test]
async fn smtp_permanent_failure_records_original_recipient_once() {
    let raw = b"From: author@example.invalid\r\nMessage-ID: <bounce@example.invalid>\r\n\r\nbody";
    let (db, lease, role, sink) = tests::fixture_at(
        "sqlite::memory:",
        "{\"list_id\":\"test.example.invalid\"}",
        raw,
        vec!["Mixed@Example.invalid".into()],
    )
    .await;
    let job = lease.job.clone();
    db.lists()
        .update(
            &"test.example.invalid".parse().unwrap(),
            &serde_json::json!({"bounce_score_threshold":0.5,"process_bounces":true}),
        )
        .await
        .unwrap();
    for email in ["healthy@example.invalid", "Mixed@Example.invalid"] {
        db.members()
            .create(listmngr_db::NewMember {
                list_id: "test.example.invalid".parse().unwrap(),
                email: email.into(),
                display_name: String::new(),
                role: listmngr_core::MemberRole::Member,
                subscription_mode: listmngr_core::SubscriptionMode::AsAddress,
            })
            .await
            .unwrap();
    }
    let peer = async {
        let (stream, _) = sink.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut read = BufReader::new(read);
        write.write_all(b"220 fixture\r\n").await.unwrap();
        for (prefix, response) in [
            ("EHLO", "250 fixture\r\n"),
            ("MAIL FROM:", "250 ok\r\n"),
            (
                "RCPT TO:<Mixed@Example.invalid>",
                "550 private diagnostic token body\r\n",
            ),
        ] {
            let mut line = String::new();
            assert!(read.read_line(&mut line).await.unwrap() > 0);
            assert!(line.starts_with(prefix), "{line}");
            write.write_all(response.as_bytes()).await.unwrap();
        }
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(peer, deliver_one(&db, &role, lease.clone()))
    })
    .await
    .unwrap();
    let event: (String, String, String, String) =
        sqlx::query_as("SELECT recipient,job_id,message_id,source FROM bounce_events")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        event,
        (
            "Mixed@Example.invalid".into(),
            job.id.0.to_string(),
            job.message_id.0.to_string(),
            "smtp_permanent_failure".into()
        )
    );
    let events = db
        .bounces()
        .list(&"test.example.invalid".parse().unwrap(), 100, 0)
        .await
        .unwrap();
    let metadata = serde_json::to_value(&events[0]).unwrap();
    assert_eq!(metadata["smtp_stage"], "rcpt");
    assert_eq!(metadata["smtp_code"], 550);
    let recipients = crate::policy_facts::resolve_recipients(
        &db,
        &"test.example.invalid".parse().unwrap(),
        "author@example.invalid",
    )
    .await
    .unwrap();
    assert_eq!(recipients, vec!["healthy@example.invalid"]);
    let list_id = "test.example.invalid".parse().unwrap();
    let member = db
        .members()
        .roster(&list_id, listmngr_core::MemberRole::Member)
        .await
        .unwrap();
    assert!(member.iter().all(|m| m.bounce_score.abs() < f64::EPSILON));
    deliver_one(&db, &role, lease).await;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM bounce_events")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(
        db.mail_queue().job(job.id).await.unwrap().state,
        listmngr_db::mail_queue::JobState::Done
    );
}
