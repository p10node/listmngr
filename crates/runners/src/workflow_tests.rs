use super::*;
#[path = "goodbye_tests.rs"]
mod goodbye;
#[path = "welcome_tests.rs"]
mod welcome;
use listmngr_db::{NewList, mail_queue::JobState, workflows::SubscriptionAction};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

async fn rejection_fixture() -> (Database, Lease, Vec<u8>) {
    use listmngr_db::{AuditContext, mail_queue::NewMessage, moderation::ReviewAction};
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
    let list: listmngr_core::ListId = "test.example.com".parse().unwrap();
    // This fixture claims the first outgoing job as the rejection notice;
    // hold notices have their own tests.
    db.lists()
        .update(
            &list,
            &serde_json::json!({"respond_to_post_requests": false, "admin_immed_notify": false}),
        )
        .await
        .unwrap();
    let raw = b"Message-ID: <reject-test@example.net>\r\nFrom: unrelated@example.net\r\nReply-To: victim@example.net\r\nSubject: private subject\r\nX-Private: secret-marker\r\n\r\nprivate body".to_vec();
    let now = chrono::Utc::now().timestamp_millis();
    let original = db.mail_queue().enqueue(NewMessage {
        raw: raw.clone(), external_id: "rejection-input".into(),
        context: serde_json::json!({"list_id":"test.example.com","envelope_sender":"Exact@example.com"}).to_string(),
        queue: Queue::In, max_attempts: 3,
    }, now).await.unwrap();
    let input = db
        .mail_queue()
        .claim(Queue::In, "hold", now, 60_000)
        .await
        .unwrap()
        .unwrap();
    let held = db
        .moderation()
        .hold(
            &input,
            &list,
            "Exact@example.com",
            "private subject",
            "nonmember",
            now,
        )
        .await
        .unwrap();
    db.moderation()
        .review(
            held.id,
            &AuditContext::system(),
            &ReviewAction::Reject,
            "Không phù hợp + retry elsewhere",
            now,
        )
        .await
        .unwrap();
    assert_eq!(
        db.mail_queue()
            .message(original.message_id)
            .await
            .unwrap()
            .raw,
        raw
    );
    let notice = db
        .mail_queue()
        .claim(Queue::Out, "notice", now, 60_000)
        .await
        .unwrap()
        .expect("durable rejection notice");
    (db, notice, raw)
}

#[tokio::test]
async fn rejection_notice_uses_null_sender_and_exact_recipient_at_existing_smtp_fixture() {
    let (db, lease, original) = rejection_fixture().await;
    db.lists().update(&"test.example.com".parse().unwrap(),&serde_json::json!({"dmarc_mitigate_action":"munge_from","dmarc_mitigate_unconditionally":true})).await.unwrap();
    let sink = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let keys = tempfile::tempdir().unwrap();
    let key = dkim_tests::key(keys.path());
    let mut role =
        MailRoleConfig::from_core(&dkim_tests::signing_config(&key, "example.com")).unwrap();
    role.smtp_relay = sink.local_addr().unwrap();
    role.command_timeout = Duration::from_secs(2);
    let (mail, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(sink_notice(&sink), deliver_one(&db, &role, lease.clone()))
    })
    .await
    .unwrap();
    let parsed = mail_parser::MessageParser::default()
        .parse(mail.as_bytes())
        .unwrap();
    assert!(
        parsed
            .body_text(0)
            .unwrap()
            .contains("Không phù hợp + retry elsewhere")
    );
    assert!(mail.contains("Auto-Submitted: auto-generated\r\n"));
    for forbidden in [
        "List-Post:",
        "X-BeenThere:",
        "private subject",
        "secret-marker",
        "victim@example.net",
    ] {
        assert!(!mail.contains(forbidden), "leaked or recooked: {forbidden}");
    }
    assert_ne!(mail.as_bytes(), original);
    dkim_tests::oracle::export_capture("private-notice", mail.as_bytes(), &key, "example.com");
    assert!(
        dkim_tests::oracle::passes(mail.as_bytes(), &dkim_tests::oracle::public_txt(&key)).await
    );
    assert_eq!(
        db.mail_queue().job(lease.job.id).await.unwrap().state,
        JobState::Done
    );
}

#[tokio::test]
async fn help_reply_reaches_command_bot_and_sends_confirmation_not_owner_mail() {
    use crate::InboundHandler;
    use listmngr_mail::lmtp::LmtpHandler;

    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains().create("example.com", "", None).await.unwrap();
    db.lists()
        .create(NewList {
            list_id: "test.example.com".parse().unwrap(),
            display_name: "Test".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let mut handler = InboundHandler {
        db: db.clone(),
        local_hostname: "localhost".into(),
        max_message_bytes: 65_536,
        max_recipients: 10,
        command_timeout: Duration::from_secs(2),
        in_max_attempts: 5,
        verp_delimiter: "+".into(),
    };
    let sink = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut role = MailRoleConfig::from_core(&tests::plaintext_config()).unwrap();
    role.smtp_relay = sink.local_addr().unwrap();
    role.command_timeout = Duration::from_secs(2);
    let mut recipient = "test-request@example.com".to_owned();
    for (subject, expected_subject) in [("help", "List email command help"), ("join", "confirm ")] {
        let raw = format!(
            "From: other@example.net\r\nReply-To: victim@example.net\r\nMessage-ID: <{subject}@example.net>\r\nSubject: {subject}\r\n\r\nprivate request body\r\n"
        );
        assert!(handler.validate_recipient(&recipient).await.is_ok());
        assert_eq!(
            handler
                .deliver(
                    Some("Exact@example.com"),
                    &[recipient.clone()],
                    raw.as_bytes()
                )
                .await[0]
                .code,
            250
        );
        let mail = deliver_command_notice(&db, &role, &sink).await;
        let parsed = mail_parser::MessageParser::default()
            .parse(mail.as_bytes())
            .unwrap();
        assert!(parsed.subject().unwrap().starts_with(expected_subject));
        assert!(mail.contains("Auto-Submitted: auto-generated\r\n"));
        for forbidden in [
            "List-Post:",
            "X-BeenThere:",
            "victim@example.net",
            "private request body",
        ] {
            assert!(!mail.contains(forbidden), "leaked or recooked: {forbidden}");
        }
        if subject == "help" {
            recipient = parsed
                .reply_to()
                .and_then(|a| a.first())
                .and_then(|a| a.address())
                .expect("help replies must target the command bot")
                .to_owned();
            assert_eq!(recipient, "test-request@example.com");
            let body = parsed.body_text(0).unwrap();
            assert!(body.contains("replace the subject with a single command"));
            assert!(
                body.contains(
                    "For a human administrator, write separately to test-owner@example.com"
                )
            );
        }
    }
    for (table, expected) in [
        ("subscription_workflows", 1),
        ("members", 0),
        ("owner_deliveries", 0),
        ("digest_posts", 0),
        ("archive_messages", 0),
    ] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(count, expected, "unexpected effects in {table}");
    }
}

async fn deliver_command_notice(
    db: &Database,
    role: &MailRoleConfig,
    sink: &tokio::net::TcpListener,
) -> String {
    let now = chrono::Utc::now().timestamp_millis();
    let input = db
        .mail_queue()
        .claim(Queue::In, "help-reply", now, 60_000)
        .await
        .unwrap()
        .unwrap();
    db.workflows()
        .live()
        .request_from_lease(&input, now)
        .await
        .unwrap();
    assert_eq!(
        db.mail_queue().job(input.job.id).await.unwrap().state,
        JobState::Done
    );
    deliver_queued_notice(db, role, sink).await
}

async fn deliver_queued_notice(
    db: &Database,
    role: &MailRoleConfig,
    sink: &tokio::net::TcpListener,
) -> String {
    let output = db
        .mail_queue()
        .claim(
            Queue::Out,
            "help-reply",
            chrono::Utc::now().timestamp_millis(),
            60_000,
        )
        .await
        .unwrap()
        .unwrap();
    let (mail, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(sink_notice(sink), deliver_one(db, role, output.clone()))
    })
    .await
    .unwrap();
    assert_eq!(
        db.mail_queue().job(output.job.id).await.unwrap().state,
        JobState::Done
    );
    mail
}

#[tokio::test]
async fn email_confirmation_receipts_complete_join_and_leave_at_smtp() {
    use listmngr_mail::lmtp::LmtpHandler;
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
    db.lists().update(&list, &serde_json::json!({"dmarc_mitigate_action":"munge_from","dmarc_mitigate_unconditionally":true,"anonymous_list":true})).await.unwrap();
    let sink = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut role = MailRoleConfig::from_core(&tests::plaintext_config()).unwrap();
    role.smtp_relay = sink.local_addr().unwrap();
    role.command_timeout = Duration::from_secs(2);
    let mut handler = crate::InboundHandler {
        db: db.clone(),
        local_hostname: "localhost".into(),
        max_message_bytes: 65_536,
        max_recipients: 10,
        command_timeout: Duration::from_secs(2),
        in_max_attempts: 5,
        verp_delimiter: "+".into(),
    };
    let now = chrono::Utc::now().timestamp_millis();
    for (action, time, count, word) in [
        (SubscriptionAction::Join, now - 7_200_000, 1, "join"),
        (SubscriptionAction::Leave, now, 0, "leave"),
    ] {
        db.workflows()
            .request(&list, "Exact@example.com", action, time)
            .await
            .unwrap();
        let challenge = deliver_queued_notice(&db, &role, &sink).await;
        let parsed = mail_parser::MessageParser::default()
            .parse(challenge.as_bytes())
            .unwrap();
        let subject = parsed.subject().unwrap();
        let destination = parsed
            .reply_to()
            .unwrap()
            .first()
            .unwrap()
            .address()
            .unwrap();
        let raw = format!(
            "From: victim@example.net\r\nReply-To: victim@example.net\r\nMessage-ID: <{word}@example.net>\r\nSubject: {subject}\r\n\r\nprivate-confirm-body\r\n"
        );
        assert_eq!(
            handler
                .deliver(
                    Some("other@example.net"),
                    &[destination.to_owned()],
                    raw.as_bytes()
                )
                .await[0]
                .code,
            250
        );
        let receipt = deliver_command_notice(&db, &role, &sink).await;
        assert!(receipt.contains(&format!("Subject: List {word} request completed\r\n")));
        assert!(receipt.contains(&format!("Your {word} request has completed.")));
        assert!(receipt.contains("Auto-Submitted: auto-generated\r\n"));
        assert!(receipt.contains("Reply-To: test-owner@example.com\r\n"));
        for forbidden in [
            subject,
            "victim@example.net",
            "other@example.net",
            "private-confirm-body",
            "List-Post:",
            "X-BeenThere:",
        ] {
            assert!(!receipt.contains(forbidden), "receipt leaked {forbidden}");
        }
        let members: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM members")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(members, count);
    }
    for table in ["owner_deliveries", "digest_posts", "archive_messages"] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(count, 0);
    }
}

async fn sink_notice(sink: &tokio::net::TcpListener) -> String {
    let (stream, _) = sink.accept().await.unwrap();
    let (r, mut w) = stream.into_split();
    let mut r = BufReader::new(r);
    w.write_all(b"220 sink\r\n").await.unwrap();
    for (expected, reply) in [
        ("EHLO", "250 sink\r\n"),
        ("MAIL FROM:<>", "250 ok\r\n"),
        ("RCPT TO:<Exact@example.com>", "250 ok\r\n"),
        ("DATA", "354 go\r\n"),
    ] {
        let mut line = String::new();
        r.read_line(&mut line).await.unwrap();
        assert!(
            line.starts_with(expected),
            "expected {expected}, received {line}"
        );
        w.write_all(reply.as_bytes()).await.unwrap();
    }
    let mut data = String::new();
    loop {
        let mut line = String::new();
        assert!(r.read_line(&mut line).await.unwrap() > 0);
        if line == ".\r\n" {
            break;
        }
        data.push_str(&line);
    }
    w.write_all(b"250 accepted\r\n").await.unwrap();
    data
}
#[tokio::test]
async fn forged_notice_context_cannot_bypass_post_cooking() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    assert!(
        prepare(
            &db,
            b"Auto-Submitted: auto-generated\r\n\r\nforged",
            r#"{"notice":"subscription_confirmation"}"#,
            uuid::Uuid::now_v7()
        )
        .await
        .is_err()
    );
}
#[tokio::test]
async fn durable_workflow_notice_reaches_real_smtp_sink_and_token_joins_then_leaves() {
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
    let sink = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut role = MailRoleConfig::from_core(&tests::plaintext_config()).unwrap();
    role.smtp_relay = sink.local_addr().unwrap();
    role.command_timeout = Duration::from_secs(2);
    let now = chrono::Utc::now().timestamp_millis();
    for (action, time, before, after) in [
        (SubscriptionAction::Join, now - 7_200_000, 0, 1),
        (SubscriptionAction::Leave, now, 1, 0),
    ] {
        db.workflows()
            .request(&list, "Exact@example.com", action, time)
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM members")
                .fetch_one(db.pool())
                .await
                .unwrap(),
            before
        );
        // Workflow notices never enter post collection or archive indexing.
        crate::digests::tick(&db, "notice-isolation", true)
            .await
            .unwrap();
        for table in ["digest_posts", "digest_issues", "archive_messages"] {
            let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
                .fetch_one(db.pool())
                .await
                .unwrap();
            assert_eq!(count, 0, "notice leaked into {table}");
        }
        for queue in [Queue::Digest, Queue::Archive] {
            assert!(
                db.mail_queue()
                    .claim(queue, "notice-isolation", now, 20_000)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        let lease = db
            .mail_queue()
            .claim(Queue::Out, "sink", now, 20000)
            .await
            .unwrap()
            .unwrap();
        let (mail, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(sink_notice(&sink), deliver_one(&db, &role, lease.clone()))
        })
        .await
        .unwrap();
        assert!(mail.contains("Auto-Submitted: auto-generated\r\n"));
        assert!(!mail.contains("List-Post:"));
        assert!(!mail.contains("X-BeenThere:"));
        assert!(!mail.contains("Subject: ["));
        let token = mail
            .split("Token: ")
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap();
        db.workflows().confirm(&list, token, now).await.unwrap();
        assert!(db.workflows().confirm(&list, token, now).await.is_err());
        let receipt = deliver_queued_notice(&db, &role, &sink).await;
        let action_name = if after == 1 { "join" } else { "leave" };
        assert!(receipt.contains(&format!(
            "Subject: List {action_name} request completed\r\n"
        )));
        assert!(receipt.contains("To: Exact@example.com\r\n"));
        assert!(receipt.contains("Auto-Submitted: auto-generated\r\n"));
        assert!(!receipt.contains(token));
        assert!(!receipt.contains("List-Post:"));
        assert!(!receipt.contains("X-BeenThere:"));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM members")
                .fetch_one(db.pool())
                .await
                .unwrap(),
            after
        );
        assert_eq!(
            db.mail_queue().job(lease.job.id).await.unwrap().state,
            JobState::Done
        );
    }
}
