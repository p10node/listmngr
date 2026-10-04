use listmngr_db::{Database, NewList, mail_queue::Queue};
use listmngr_mail::lmtp::{LmtpHandler, serve_session};
use listmngr_runners::InboundHandler;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const RAW: &[u8] = b"Message-ID: <dsn@example.invalid>\r\nAuto-Submitted: auto-generated\r\nContent-Type: multipart/report; report-type=delivery-status\r\n\r\nOpaque untrusted report: \x00\xff\r\n";

async fn setup(db: &Database, names: &[&str]) {
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    for &name in names {
        db.lists()
            .create(NewList {
                list_id: format!("{name}.example.invalid").parse().unwrap(),
                display_name: name.to_string(),
                style: "legacy-default".into(),
            })
            .await
            .unwrap();
    }
}

fn handler(db: Database) -> InboundHandler {
    InboundHandler {
        db,
        local_hostname: "fixture.invalid".into(),
        max_message_bytes: 4096,
        max_recipients: 4,
        command_timeout: Duration::from_secs(3),
        in_max_attempts: 3,
        verp_delimiter: "+".into(),
        structure: listmngr_mail::structure::Limits::default(),
    }
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

async fn command(client: &mut BufReader<tokio::io::DuplexStream>, text: &[u8]) -> String {
    client.get_mut().write_all(text).await.unwrap();
    reply(client).await
}

#[tokio::test]
async fn null_sender_bounce_is_durable_before_lmtp_success_and_never_a_post() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("bounce.db").display()
    );
    let db = Database::connect(&url, 1).await.unwrap();
    setup(&db, &["list"]).await;
    let mut intake = handler(db.clone());
    let (client, server) = tokio::io::duplex(8192);
    let task = tokio::spawn(async move {
        serve_session(server, &mut intake).await.unwrap();
    });
    let mut client = BufReader::new(client);
    assert!(reply(&mut client).await.starts_with("220"));
    assert!(
        command(&mut client, b"LHLO peer.invalid\r\n")
            .await
            .starts_with("250")
    );
    assert!(
        command(&mut client, b"MAIL FROM:<>\r\n")
            .await
            .starts_with("250")
    );
    let accepted = command(&mut client, b"RCPT TO:<LIST-BOUNCES@EXAMPLE.INVALID>\r\n").await;
    assert!(
        accepted.starts_with("250"),
        "bounce route rejected: {accepted}"
    );
    assert!(command(&mut client, b"DATA\r\n").await.starts_with("354"));
    client.get_mut().write_all(RAW).await.unwrap();
    assert!(command(&mut client, b".\r\n").await.starts_with("250"));
    // Independent read immediately after the wire success, before closing LMTP.
    let queued: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM queue_jobs WHERE queue='bounces' AND state='ready'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(queued, 1);
    assert!(command(&mut client, b"QUIT\r\n").await.starts_with("221"));
    task.await.unwrap();
    db.pool().close().await;
    let db = Database::connect(&url, 1).await.unwrap();
    let raw: Vec<u8> = sqlx::query_scalar("SELECT raw FROM message_blobs")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(raw, RAW);
    let context: String = sqlx::query_scalar("SELECT context FROM messages")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let context: serde_json::Value = serde_json::from_str(&context).unwrap();
    assert_eq!(context["list_id"], "list.example.invalid");
    assert!(context["envelope_sender"].is_null());
    assert!(context.get("subscription_command").is_none());
    let others: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs WHERE queue!='bounces'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(others, 0);
    let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM bounce_events")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(
        events, 0,
        "untrusted intake must not become a trusted failure event"
    );
    let audit: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='queue.enqueue'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(audit, 1);
    assert!(
        db.mail_queue()
            .claim(
                Queue::In,
                "ordinary",
                chrono::Utc::now().timestamp_millis(),
                1000
            )
            .await
            .unwrap()
            .is_none()
    );
}

async fn intake_counts(db: &Database) -> (i64, i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT COUNT(*) FROM message_blobs), (SELECT COUNT(*) FROM messages), (SELECT COUNT(*) FROM queue_jobs), (SELECT COUNT(*) FROM audit_log WHERE action='queue.enqueue')")
        .fetch_one(db.pool()).await.unwrap()
}

#[tokio::test]
async fn bounce_batch_audit_failure_rolls_back_and_valid_retry_preserves_both_lists() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    setup(&db, &["first", "second"]).await;
    sqlx::query("CREATE TRIGGER fail_second_enqueue AFTER INSERT ON audit_log WHEN NEW.action='queue.enqueue' AND (SELECT COUNT(*) FROM audit_log WHERE action='queue.enqueue')=2 BEGIN SELECT RAISE(ABORT, 'fixture audit failure'); END")
        .execute(db.pool()).await.unwrap();
    let mut intake = handler(db.clone());
    let recipients = vec![
        "first-bounces@example.invalid".into(),
        "second-bounces@example.invalid".into(),
    ];
    let outcomes = intake.deliver(None, &recipients, RAW).await;
    assert_eq!(
        outcomes.iter().map(|o| o.code).collect::<Vec<_>>(),
        [451, 451]
    );
    assert_eq!(intake_counts(&db).await, (0, 0, 0, 0));
    sqlx::query("DROP TRIGGER fail_second_enqueue")
        .execute(db.pool())
        .await
        .unwrap();
    let outcomes = intake.deliver(None, &recipients, RAW).await;
    assert_eq!(
        outcomes.iter().map(|o| o.code).collect::<Vec<_>>(),
        [250, 250]
    );
    assert_eq!(intake_counts(&db).await, (1, 2, 2, 2));
    let contexts: Vec<String> = sqlx::query_scalar("SELECT context FROM messages")
        .fetch_all(db.pool())
        .await
        .unwrap();
    let mut lists: Vec<String> = contexts
        .iter()
        .map(|s| {
            serde_json::from_str::<serde_json::Value>(s).unwrap()["list_id"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    lists.sort();
    assert_eq!(lists, ["first.example.invalid", "second.example.invalid"]);
}

#[tokio::test]
async fn exact_posting_list_wins_and_only_verp_shaped_plus_addresses_are_bounces() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    setup(&db, &["list", "list-bounces", "other"]).await;
    let mut intake = handler(db.clone());
    for address in [
        "list-bounces+token@example.invalid",
        "missing-bounces@example.invalid",
        "missing-bounces+local=domain@example.invalid",
        "other-request+local=domain@example.invalid",
    ] {
        assert!(
            intake.validate_recipient(address).await.is_err(),
            "{address}"
        );
    }
    let recipients: Vec<String> = vec![
        "list-bounces@example.invalid".into(),
        "OTHER-BOUNCES@EXAMPLE.INVALID".into(),
        "other-bounces+Alice=Elsewhere.invalid@example.invalid".into(),
    ];
    for recipient in &recipients {
        intake.validate_recipient(recipient).await.unwrap();
    }
    let outcomes = intake
        .deliver(Some("daemon@example.invalid"), &recipients, RAW)
        .await;
    assert_eq!(
        outcomes.iter().map(|o| o.code).collect::<Vec<_>>(),
        [250, 250, 250]
    );
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT q.queue,m.context FROM queue_jobs q JOIN messages m ON m.id=q.message_id ORDER BY q.queue, m.context")
        .fetch_all(db.pool()).await.unwrap();
    assert_eq!(rows.len(), 3);
    let contexts: Vec<serde_json::Value> = rows
        .iter()
        .map(|(_, context)| serde_json::from_str(context).unwrap())
        .collect();
    assert_eq!(rows[0].0, "bounces");
    assert_eq!(rows[1].0, "bounces");
    let verp = contexts[..2]
        .iter()
        .find(|context| context.get("verp_recipient").is_some())
        .expect("the VERP bounce names its recipient");
    assert_eq!(verp["list_id"], "other.example.invalid");
    assert_eq!(verp["verp_recipient"], "alice@elsewhere.invalid");
    let plain = contexts[..2]
        .iter()
        .find(|context| context.get("verp_recipient").is_none())
        .unwrap();
    assert_eq!(plain["list_id"], "other.example.invalid");
    assert_eq!(rows[2].0, "in");
    assert_eq!(contexts[2]["list_id"], "list-bounces.example.invalid");
}

#[tokio::test]
async fn optional_bounce_id_does_not_admit_malformed_or_oversized_headers() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    setup(&db, &["list"]).await;
    let mut intake = handler(db.clone());
    let mut reports = vec![
        b"Message-ID: invalid\r\n\r\nbody".to_vec(),
        b"Message-ID: <a@b>\r\nMessage-ID: <c@d>\r\n\r\nbody".to_vec(),
        b"Malformed header\r\n\r\nbody".to_vec(),
        b"Subject: unterminated\r\n".to_vec(),
    ];
    reports.push(
        format!(
            "Subject: {}\r\n\r\nbody",
            "a".repeat(listmngr_mail::MAX_HEADER_LINE_BYTES)
        )
        .into_bytes(),
    );
    for raw in reports {
        let outcomes = intake
            .deliver(None, &["list-bounces@example.invalid".into()], &raw)
            .await;
        assert_eq!(outcomes[0].code, 550);
    }
    assert_eq!(intake_counts(&db).await, (0, 0, 0, 0));
}
