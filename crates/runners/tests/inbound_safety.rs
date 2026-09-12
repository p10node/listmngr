use listmngr_db::Database;
use listmngr_mail::lmtp::{LmtpHandler, serve_session};

#[tokio::test]
async fn second_recipient_storage_failure_rolls_back_entire_batch() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    sqlx::query("CREATE TRIGGER fail_second BEFORE INSERT ON messages WHEN NEW.context LIKE '%second.example.invalid%' BEGIN SELECT RAISE(ABORT, 'fixture failure'); END")
        .execute(db.pool()).await.unwrap();
    let mut intake = handler(db.clone());
    let outcomes = intake
        .deliver(
            None,
            &[
                "first@example.invalid".into(),
                "second@example.invalid".into(),
            ],
            b"Message-ID: <batch@example.invalid>\r\n\r\nbody\r\n",
        )
        .await;
    assert_eq!(
        outcomes
            .iter()
            .map(|outcome| outcome.code)
            .collect::<Vec<_>>(),
        vec![451, 451]
    );
    let counts: (i64, i64, i64, i64) = sqlx::query_as("SELECT (SELECT COUNT(*) FROM message_blobs), (SELECT COUNT(*) FROM messages), (SELECT COUNT(*) FROM queue_jobs), (SELECT COUNT(*) FROM audit_log WHERE action='queue.enqueue')").fetch_one(db.pool()).await.unwrap();
    assert_eq!(counts, (0, 0, 0, 0));
}
use listmngr_runners::InboundHandler;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::test]
async fn valid_missing_and_unsupported_recipients_are_distinct_from_dependency_failure() {
    use listmngr_mail::lmtp::RecipientRejection;
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "fixture", None)
        .await
        .unwrap();
    db.lists()
        .create(listmngr_db::NewList {
            list_id: "list.example.invalid".parse().unwrap(),
            display_name: "fixture".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let mut intake = handler(db.clone());
    assert_eq!(
        intake.validate_recipient("LIST@EXAMPLE.INVALID").await,
        Ok(())
    );
    for address in [
        "missing@example.invalid",
        "list-bounces+token@example.invalid",
        "malformed",
    ] {
        assert!(matches!(
            intake.validate_recipient(address).await,
            Err(RecipientRejection::Permanent(_))
        ));
    }
    db.pool().close().await;
    assert!(matches!(
        intake.validate_recipient("list@example.invalid").await,
        Err(RecipientRejection::Temporary(_))
    ));
}

#[tokio::test]
async fn mixed_data_outcomes_keep_recipient_order_and_exact_bytes() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let mut intake = handler(db.clone());
    let raw = b"Message-ID: <ordered@example.invalid>\r\n\r\n\x00\xff\r\n";
    let outcomes = intake
        .deliver(
            None,
            &[
                "first@example.invalid".into(),
                "malformed".into(),
                "second@example.invalid".into(),
            ],
            raw,
        )
        .await;
    assert_eq!(
        outcomes
            .iter()
            .map(|outcome| outcome.code)
            .collect::<Vec<_>>(),
        vec![250, 550, 250]
    );
    let stored: Vec<Vec<u8>> = sqlx::query_scalar("SELECT raw FROM message_blobs")
        .fetch_all(db.pool())
        .await
        .unwrap();
    assert_eq!(stored, vec![raw.to_vec()]);
    let contexts: Vec<String> = sqlx::query_scalar("SELECT context FROM messages ORDER BY id")
        .fetch_all(db.pool())
        .await
        .unwrap();
    assert_eq!(contexts.len(), 2);
    assert!(contexts[0].contains("first.example.invalid"));
    assert!(contexts[1].contains("second.example.invalid"));
}

async fn reply(client: &mut BufReader<tokio::io::DuplexStream>) -> String {
    loop {
        let mut line = String::new();
        client.read_line(&mut line).await.unwrap();
        assert!(line.len() >= 4, "unexpected EOF");
        if line.as_bytes()[3] == b' ' {
            return line;
        }
    }
}

async fn command(client: &mut BufReader<tokio::io::DuplexStream>, text: &str) -> String {
    client
        .get_mut()
        .write_all(format!("{text}\r\n").as_bytes())
        .await
        .unwrap();
    reply(client).await
}

#[tokio::test]
async fn data_database_wait_timeout_replies_for_all_without_late_intake() {
    let directory = tempfile::tempdir().unwrap();
    let db = Database::connect(
        &format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("intake.db").display()
        ),
        1,
    )
    .await
    .unwrap();
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "fixture", None)
        .await
        .unwrap();
    for name in ["first", "second"] {
        db.lists()
            .create(listmngr_db::NewList {
                list_id: format!("{name}.example.invalid").parse().unwrap(),
                display_name: name.into(),
                style: "legacy-default".into(),
            })
            .await
            .unwrap();
    }
    let mut intake = handler(db.clone());
    intake.command_timeout = Duration::from_millis(150);
    let (client, server) = tokio::io::duplex(8192);
    let task = tokio::spawn(async move {
        serve_session(server, &mut intake).await.unwrap();
    });
    let mut client = BufReader::new(client);
    assert!(reply(&mut client).await.starts_with("220 "));
    assert!(command(&mut client, "LHLO peer").await.starts_with("250 "));
    assert!(
        command(&mut client, "MAIL FROM:<>")
            .await
            .starts_with("250 ")
    );
    assert!(
        command(&mut client, "RCPT TO:<missing@example.invalid>")
            .await
            .starts_with("550 ")
    );
    for name in ["first", "second"] {
        assert!(
            command(&mut client, &format!("RCPT TO:<{name}@example.invalid>"))
                .await
                .starts_with("250 ")
        );
    }
    let held = db.pool().acquire().await.unwrap();
    assert!(command(&mut client, "DATA").await.starts_with("354 "));
    client
        .get_mut()
        .write_all(b"Message-ID: <timeout@example.invalid>\r\n\r\nbody\r\n.\r\n")
        .await
        .unwrap();
    for _ in 0..2 {
        assert!(reply(&mut client).await.starts_with("451 "));
    }
    drop(held);
    assert!(command(&mut client, "DATA").await.starts_with("503 "));
    assert!(command(&mut client, "QUIT").await.starts_with("221 "));
    task.await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM messages")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    // No detached enqueue continues after timeout: a fresh call works normally.
    let outcomes = handler(db.clone())
        .deliver(
            None,
            &[
                "first@example.invalid".into(),
                "second@example.invalid".into(),
            ],
            b"Message-ID: <retry@example.invalid>\r\n\r\nretry\r\n",
        )
        .await;
    assert!(outcomes.iter().all(|outcome| outcome.code == 250));
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM messages")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 2);
}

fn handler(db: Database) -> InboundHandler {
    InboundHandler {
        db,
        local_hostname: "mx.example.invalid".into(),
        max_message_bytes: 4096,
        max_recipients: 10,
        command_timeout: Duration::from_secs(2),
        in_max_attempts: 3,
        verp_delimiter: "+".into(),
    }
}

#[tokio::test]
async fn quick_database_failure_is_transient_not_unknown_recipient() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.pool().close().await;
    let mut handler = handler(db);
    let (client, server) = tokio::io::duplex(8192);
    let task = tokio::spawn(async move {
        serve_session(server, &mut handler).await.unwrap();
    });
    let mut client = BufReader::new(client);
    let mut line = String::new();
    client.read_line(&mut line).await.unwrap();
    for command in [
        "LHLO peer",
        "MAIL FROM:<sender@example.invalid>",
        "RCPT TO:<list@example.invalid>",
    ] {
        client
            .get_mut()
            .write_all(format!("{command}\r\n").as_bytes())
            .await
            .unwrap();
        loop {
            line.clear();
            client.read_line(&mut line).await.unwrap();
            if line.as_bytes()[3] == b' ' {
                break;
            }
        }
    }
    assert!(line.starts_with("451 "), "{line}");
    assert!(
        !line.contains("timed out"),
        "quick failure must not wait for hook timeout"
    );
    client.get_mut().write_all(b"QUIT\r\n").await.unwrap();
    task.await.unwrap();
}
