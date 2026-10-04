use listmngr_core::{Config, ListId};
use listmngr_db::{Database, NewList};
use listmngr_mail::lmtp::LmtpHandler;
use listmngr_runners::InboundHandler;
use std::time::Duration;

#[tokio::test]
async fn join_and_leave_aliases_only_mutate_membership_after_confirmation() {
    for (suffix, leaving) in [
        ("subscribe", false),
        ("leave", true),
        ("unsubscribe", true),
        ("request", false),
    ] {
        let (db, mut handler) = fixture().await;
        if leaving {
            db.members()
                .create(listmngr_db::NewMember {
                    list_id: "test.example.com".parse().unwrap(),
                    email: "Case@example.com".into(),
                    display_name: String::new(),
                    role: listmngr_core::MemberRole::Member,
                    subscription_mode: listmngr_core::SubscriptionMode::AsUser,
                })
                .await
                .unwrap();
        }
        let recipient = format!("test-{suffix}@example.com");
        assert!(
            handler.validate_recipient(&recipient).await.is_ok(),
            "supported {suffix}"
        );
        let outcomes = handler
            .deliver(
                Some("Case@example.com"),
                &[recipient],
                b"From: Case@example.com\r\nMessage-ID: <alias@example.com>\r\nSubject: subscribe\r\n\r\n",
            )
            .await;
        assert_eq!(outcomes[0].code, 250);
        consume_command(&db, "alias-test").await;
        let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM members")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(before, i64::from(leaving));
        let notice: Vec<u8> = sqlx::query_scalar("SELECT b.raw FROM message_blobs b JOIN messages m ON m.store_key=b.store_key JOIN queue_jobs q ON q.message_id=m.id WHERE q.queue='out'").fetch_one(db.pool()).await.unwrap();
        let notice = String::from_utf8(notice).unwrap();
        assert!(notice.contains("Reply-To: test-confirm@example.com"));
        assert!(notice.contains("Subject: confirm "));
        let token = notice
            .lines()
            .find_map(|line| line.strip_prefix("Token: "))
            .unwrap();
        let reply = format!(
            "From: Case@example.com\r\nMessage-ID: <reply@example.com>\r\nSubject: Re: confirm {token}\r\n\r\n"
        );
        let recipient = if suffix == "request" {
            "test-request@example.com"
        } else {
            "test-confirm@example.com"
        };
        assert!(
            handler.validate_recipient(recipient).await.is_ok(),
            "email confirmation must be admitted"
        );
        assert_eq!(
            handler
                .deliver(
                    Some("Case@example.com"),
                    &[recipient.into()],
                    reply.as_bytes()
                )
                .await[0]
                .code,
            250
        );
        consume_command(&db, "confirm-test").await;
        let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM members")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(after, i64::from(!leaving));
    }
}

async fn consume_command(db: &Database, worker: &str) {
    let now = chrono::Utc::now().timestamp_millis();
    let lease = db
        .mail_queue()
        .claim(listmngr_db::mail_queue::Queue::In, worker, now, 30_000)
        .await
        .unwrap()
        .unwrap();
    db.workflows()
        .request_from_lease(&lease, now)
        .await
        .unwrap();
}

async fn fixture() -> (Database, InboundHandler) {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains().create("example.com", "", None).await.unwrap();
    let list: ListId = "test.example.com".parse().unwrap();
    db.lists()
        .create(NewList {
            list_id: list.clone(),
            display_name: "Test".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let handler = InboundHandler {
        db: db.clone(),
        local_hostname: "localhost".into(),
        max_message_bytes: 65_536,
        max_recipients: 10,
        command_timeout: Duration::from_secs(5),
        in_max_attempts: 5,
        verp_delimiter: "+".into(),
        structure: listmngr_mail::structure::Limits::default(),
    };
    (db, handler)
}

#[tokio::test]
async fn join_command_durably_requests_confirmation_without_immediate_membership() {
    let (db, mut handler) = fixture().await;
    assert!(
        handler
            .validate_recipient("test-join@example.com")
            .await
            .is_ok(),
        "implemented join command must be admitted"
    );
    let outcomes = handler.deliver(Some("MixedCase@example.com"), &["test-join@example.com".into()], b"From: MixedCase@example.com\r\nMessage-ID: <command@example.com>\r\n\r\nsubscribe\r\n").await;
    assert_eq!(outcomes[0].code, 250);
    let config = Config::default();
    let role = listmngr_runners::MailRoleConfig::from_core(&config).unwrap();
    let (shutdown, receiver) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(listmngr_runners::run_in_processor(
        db.clone(),
        config,
        role,
        "command-test".into(),
        receiver,
    ));
    let completed = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM subscription_workflows")
                .fetch_one(db.pool())
                .await
                .unwrap();
            if count == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    shutdown.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
    completed.expect("durable command must produce workflow before deadline");
    let members: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM members")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let workflows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM subscription_workflows")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let notices: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs WHERE queue='out'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(members, 0);
    assert_eq!(workflows, 1);
    assert_eq!(notices, 1);
}

#[tokio::test]
async fn automatic_or_null_sender_commands_never_enter_the_queue() {
    for (sender, extra) in [
        (None, ""),
        (Some(""), ""),
        (
            Some("reply@example.com"),
            "Auto-Submitted: auto-generated\r\n",
        ),
        (
            Some("reply@example.com"),
            "Auto-Submitted: no\r\naUtO-sUbMiTtEd:\r\n auto-replied\r\n",
        ),
    ] {
        let (db, mut handler) = fixture().await;
        let raw = format!(
            "From: reply@example.com\r\nMessage-ID: <automatic@example.com>\r\n{extra}\r\njoin\r\n"
        );
        let result = handler
            .deliver(sender, &["test-join@example.com".into()], raw.as_bytes())
            .await;
        assert_eq!(result[0].code, 550, "automatic command must fail closed");
        let queued: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(queued, 0);
    }
}

#[tokio::test]
async fn help_is_durable_bounded_and_never_a_subscription() {
    let (db, mut handler) = fixture().await;
    for n in 0..2 {
        let raw = format!(
            "From: Case@example.com\r\nMessage-ID: <help{n}@example.com>\r\nSubject: help\r\n\r\n"
        );
        assert_eq!(
            handler
                .deliver(
                    Some("Case@example.com"),
                    &["test-request@example.com".into()],
                    raw.as_bytes()
                )
                .await[0]
                .code,
            250
        );
        let now = chrono::Utc::now().timestamp_millis();
        let lease = db
            .mail_queue()
            .claim(listmngr_db::mail_queue::Queue::In, "help", now, 30_000)
            .await
            .unwrap()
            .unwrap();
        db.workflows()
            .request_from_lease(&lease, now)
            .await
            .unwrap();
    }
    let notices: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs WHERE queue='out'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(notices, 1);
    let workflows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM subscription_workflows")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(workflows, 0);
    let raw: Vec<u8> = sqlx::query_scalar("SELECT b.raw FROM message_blobs b JOIN messages m ON m.store_key=b.store_key JOIN queue_jobs q ON q.message_id=m.id WHERE q.queue='out'").fetch_one(db.pool()).await.unwrap();
    let text = String::from_utf8(raw).unwrap();
    assert!(text.contains("confirm TOKEN"));
    assert!(text.len() < 2048);
}

/// One command message from the same mailbox through the LMTP handler.
async fn deliver(handler: &mut InboundHandler, id: &str, subject: &str) -> u16 {
    let raw = format!(
        "From: Case@example.com\r\nMessage-ID: <{id}@example.com>\r\nSubject: {subject}\r\n\r\n"
    );
    handler
        .deliver(
            Some("Case@example.com"),
            &["test-request@example.com".to_owned()],
            raw.as_bytes(),
        )
        .await[0]
        .code
}

#[tokio::test]
async fn echo_answers_with_its_own_text_and_end_answers_nothing() {
    let (db, mut handler) = fixture().await;
    assert_eq!(
        deliver(&mut handler, "echo1", "echo sandbox check").await,
        250
    );
    consume_command(&db, "echo").await;
    let text: String = sqlx::query_scalar("SELECT b.raw FROM message_blobs b JOIN messages m ON m.store_key=b.store_key JOIN queue_jobs q ON q.message_id=m.id WHERE q.queue='out'")
        .fetch_one(db.pool())
        .await
        .map(|raw: Vec<u8>| String::from_utf8(raw).unwrap())
        .unwrap();
    assert!(text.contains("Subject: List command echo"), "{text}");
    assert!(text.contains("sandbox check"), "{text}");
    assert!(text.contains("Reply-To: test-request@example.com"));
    let workflows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM subscription_workflows")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(workflows, 0, "an echo never subscribes anybody");
    let audit: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='subscription.echo'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(audit, 1);

    // The echo shares the bot's hourly budget with help.
    assert_eq!(deliver(&mut handler, "help1", "help").await, 250);
    consume_command(&db, "echo").await;
    let notices: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs WHERE queue='out'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(notices, 1, "one bot reply per mailbox and hour");

    // `end` is accepted and does nothing at all.
    let (db, mut handler) = fixture().await;
    assert_eq!(deliver(&mut handler, "end1", "end").await, 250);
    consume_command(&db, "end").await;
    let after: i64 = sqlx::query_scalar(
        "SELECT (SELECT COUNT(*) FROM queue_jobs WHERE queue='out')+(SELECT COUNT(*) FROM subscription_workflows)",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(after, 0);
    let done: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs WHERE state='done'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(done, 1, "the command job is finished, not retried");
}
