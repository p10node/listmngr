use super::*;

#[tokio::test]
async fn single_recipient_config_reaches_real_smtp_envelopes() {
    for enabled in [false, true] {
        let recipients = vec!["a@example.invalid".into(), "b@example.invalid".into()];
        let (db, lease, mut role, sink) = fixture_with_recipients(
            "{\"list_id\":\"test.example.invalid\"}",
            b"Subject: isolated\r\n\r\nfixture-body\r\n",
            recipients.clone(),
        )
        .await;
        let config: listmngr_core::Config = serde_json::from_value(serde_json::json!({
            "mta": {"smtp_tls":"plaintext_trusted_relay", "smtp_single_recipient":enabled}
        }))
        .unwrap();
        let configured = MailRoleConfig::from_core(&config).unwrap();
        // Preserve the fixture relay; use every other runtime setting from config.
        let relay = role.smtp_relay;
        role = configured;
        role.smtp_relay = relay;
        role.command_timeout = Duration::from_secs(2);
        let expected = if enabled {
            vec![vec![recipients[0].clone()], vec![recipients[1].clone()]]
        } else {
            vec![recipients.clone()]
        };
        let capture = async {
            let mut payloads = Vec::new();
            for group in &expected {
                payloads.push(capture_envelope(&sink, "test-bounces@example.invalid", group).await);
            }
            payloads
        };
        let (payloads, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(capture, deliver_one(&db, &role, lease.clone()))
        })
        .await
        .unwrap();
        assert_eq!(payloads.len(), expected.len());
        for payload in &payloads {
            assert!(payload.ends_with(b"\r\n\r\nfixture-body\r\n"));
            assert_eq!(payload, &payloads[0]);
        }
        assert_eq!(
            db.mail_queue().job(lease.job.id).await.unwrap().state,
            JobState::Done
        );
        assert!(
            db.mail_queue()
                .pending_recipients(lease.job.id)
                .await
                .unwrap()
                .is_empty()
        );
        let sent: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM delivery_recipients WHERE job_id=$1 AND status='sent'",
        )
        .bind(lease.job.id.0.to_string())
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(sent, 2);
    }
}

pub async fn capture_envelope(
    sink: &tokio::net::TcpListener,
    sender: &str,
    recipients: &[String],
) -> Vec<u8> {
    capture_envelope_reply(sink, sender, recipients, Some(b"250 accepted\r\n")).await
}

async fn capture_envelope_reply(
    sink: &tokio::net::TcpListener,
    sender: &str,
    recipients: &[String],
    final_reply: Option<&[u8]>,
) -> Vec<u8> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (stream, _) = sink.accept().await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    writer.write_all(b"220 fixture\r\n").await.unwrap();
    let mut commands = vec![(
        "EHLO listmngr.invalid\r\n".to_owned(),
        "250-fixture\r\n250 8BITMIME\r\n",
    )];
    commands.push((format!("MAIL FROM:<{sender}>\r\n"), "250 ok\r\n"));
    commands.extend(
        recipients
            .iter()
            .map(|r| (format!("RCPT TO:<{r}>\r\n"), "250 ok\r\n")),
    );
    commands.push(("DATA\r\n".to_owned(), "354 go\r\n"));
    for (expected, response) in commands {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).await.unwrap() > 0);
        assert_eq!(line, expected);
        println!("SMTP fixture: {}", line.trim_end());
        writer.write_all(response.as_bytes()).await.unwrap();
    }
    let mut data = Vec::new();
    loop {
        let mut line = Vec::new();
        assert!(reader.read_until(b'\n', &mut line).await.unwrap() > 0);
        if line == b".\r\n" {
            break;
        }
        data.extend_from_slice(if line.starts_with(b"..") {
            &line[1..]
        } else {
            &line
        });
    }
    if let Some(reply) = final_reply {
        writer.write_all(reply).await.unwrap();
    }
    data
}

#[test]
fn single_recipient_is_default_off_and_requires_boolean() {
    let config = plaintext_config();
    assert!(!config.mta.smtp_single_recipient);
    assert!(
        !MailRoleConfig::from_core(&config)
            .unwrap()
            .smtp_single_recipient
    );
    for invalid in [
        serde_json::json!("true"),
        serde_json::json!(1),
        serde_json::Value::Null,
    ] {
        assert!(
            serde_json::from_value::<listmngr_core::Config>(
                serde_json::json!({"mta":{"smtp_single_recipient":invalid}})
            )
            .is_err()
        );
    }
}

#[tokio::test]
async fn single_recipient_preserves_null_sender_batch() {
    let recipients = vec!["a@example.invalid".into(), "b@example.invalid".into()];
    let sink = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut role = MailRoleConfig::from_core(&plaintext_config()).unwrap();
    role.smtp_single_recipient = true;
    role.smtp_relay = sink.local_addr().unwrap();
    let stream = tokio::net::TcpStream::connect(role.smtp_relay)
        .await
        .unwrap();
    let raw = b"Subject: private notice\r\n\r\nnotice\r\n";
    let (data, results) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            capture_envelope(&sink, "", &recipients),
            send_transactions(stream, &role, None, &recipients, raw)
        )
    })
    .await
    .unwrap();
    assert_eq!(data, raw);
    assert_eq!(results, vec![RecipientStatus::Sent, RecipientStatus::Sent]);
}

#[tokio::test]
async fn single_recipient_cancel_between_sessions_quarantines_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}/queue.sqlite?mode=rwc", dir.path().display());
    let recipients = vec![
        "a@example.invalid".into(),
        "b@example.invalid".into(),
        "c@example.invalid".into(),
    ];
    let (db, lease, mut role, sink) = fixture_at(
        &url,
        "{\"list_id\":\"test.example.invalid\"}",
        b"Subject: cancel\r\n\r\nfirst accepted\r\n",
        recipients.clone(),
    )
    .await;
    role.smtp_single_recipient = true;
    role.command_timeout = Duration::from_secs(30);
    let (arrived, received) = tokio::sync::oneshot::channel();
    {
        let capture = async {
            let data =
                capture_envelope(&sink, "test-bounces@example.invalid", &recipients[..1]).await;
            assert!(data.ends_with(b"first accepted\r\n"));
            let (_stream, _) = sink.accept().await.unwrap();
            arrived.send(()).unwrap();
            std::future::pending::<()>().await;
        };
        let delivery = deliver_one(&db, &role, lease.clone());
        tokio::pin!(capture, delivery);
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                () = &mut capture => panic!("fixture must hold second greeting"),
                () = &mut delivery => panic!("delivery must wait for greeting"),
                result = received => result.unwrap(),
            }
        })
        .await
        .unwrap();
        // Cancel the owned worker with one accepted and two unresolved recipients.
    }
    db.pool().close().await;
    let db = Database::connect(&url, 1).await.unwrap();
    let statuses: Vec<(String, String)> = sqlx::query_as(
        "SELECT email,status FROM delivery_recipients WHERE job_id=$1 ORDER BY email",
    )
    .bind(lease.job.id.0.to_string())
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(
        statuses,
        recipients
            .into_iter()
            .map(|email| (email, "ambiguous".into()))
            .collect::<Vec<_>>()
    );
    let recovered = db
        .mail_queue()
        .claim(
            Queue::Out,
            "restart",
            lease.job.lease_until.unwrap() + 1,
            20_000,
        )
        .await
        .unwrap()
        .unwrap();
    deliver_one(&db, &role, recovered).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(150), sink.accept())
            .await
            .is_err()
    );
    assert!(
        db.mail_queue()
            .pending_recipients(lease.job.id)
            .await
            .unwrap()
            .is_empty()
    );
    db.pool().close().await;
}

#[tokio::test]
async fn single_recipient_mixed_sessions_retry_only_transient() {
    let recipients: Vec<String> = ["a", "b", "c", "d"]
        .iter()
        .map(|s| format!("{s}@example.invalid"))
        .collect();
    let (db, lease, mut role, sink) = fixture_with_recipients(
        "{\"list_id\":\"test.example.invalid\"}",
        b"Subject: mixed\r\n\r\nbody\r\n",
        recipients.clone(),
    )
    .await;
    role.smtp_single_recipient = true;
    role.command_timeout = Duration::from_secs(2);
    let capture = async {
        let replies: [Option<&[u8]>; 4] = [
            Some(b"250 accepted\r\n"),
            Some(b"451 later\r\n"),
            None,
            Some(b"250 accepted\r\n"),
        ];
        for (recipient, reply) in recipients.iter().zip(replies) {
            capture_envelope_reply(
                &sink,
                "test-bounces@example.invalid",
                std::slice::from_ref(recipient),
                reply,
            )
            .await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(capture, deliver_one(&db, &role, lease.clone()))
    })
    .await
    .unwrap();
    let statuses: Vec<String> =
        sqlx::query_scalar("SELECT status FROM delivery_recipients WHERE job_id=$1 ORDER BY email")
            .bind(lease.job.id.0.to_string())
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(statuses, ["sent", "pending", "ambiguous", "sent"]);
    assert_eq!(
        db.mail_queue()
            .pending_recipients(lease.job.id)
            .await
            .unwrap(),
        recipients[1..2]
    );
    let job = db.mail_queue().job(lease.job.id).await.unwrap();
    let retry = db
        .mail_queue()
        .claim(Queue::Out, "retry", job.run_after + 1, 20_000)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            capture_envelope(&sink, "test-bounces@example.invalid", &recipients[1..2]),
            deliver_one(&db, &role, retry)
        )
    })
    .await
    .unwrap();
    let statuses: Vec<String> =
        sqlx::query_scalar("SELECT status FROM delivery_recipients WHERE job_id=$1 ORDER BY email")
            .bind(lease.job.id.0.to_string())
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(statuses, ["sent", "sent", "ambiguous", "sent"]);
}

#[tokio::test]
async fn single_recipient_later_connect_and_greeting_failure_preserve_prior_sent() {
    let recipients = vec![
        "a@example.invalid".into(),
        "b@example.invalid".into(),
        "c@example.invalid".into(),
    ];
    let (db, lease, mut role, sink) = fixture_with_recipients(
        "{\"list_id\":\"test.example.invalid\"}",
        b"Subject: reconnect\r\n\r\nbody\r\n",
        recipients.clone(),
    )
    .await;
    role.smtp_single_recipient = true;
    role.command_timeout = Duration::from_secs(2);
    let capture = async move {
        capture_envelope(&sink, "test-bounces@example.invalid", &recipients[..1]).await;
        let (stream, _) = sink.accept().await.unwrap();
        // Session two loses its greeting; session three cannot connect.
        drop(sink);
        drop(stream);
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(capture, deliver_one(&db, &role, lease.clone()))
    })
    .await
    .unwrap();
    let statuses: Vec<String> =
        sqlx::query_scalar("SELECT status FROM delivery_recipients WHERE job_id=$1 ORDER BY email")
            .bind(lease.job.id.0.to_string())
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(statuses, ["sent", "pending", "pending"]);
}

pub(super) fn plaintext_config() -> listmngr_core::Config {
    serde_json::from_value(serde_json::json!({"mta": {"smtp_tls": "plaintext_trusted_relay"}}))
        .unwrap()
}

#[tokio::test]
async fn munge_setting_reaches_smtp_and_invalid_author_never_connects() {
    for enabled in [false, true] {
        let raw = b"From: Author <author@elsewhere.invalid>\r\nSender: old@elsewhere.invalid\r\nMessage-ID: <p@elsewhere.invalid>\r\n\r\nbody\r\n";
        let (db, lease, role, sink) = fixture("{\"list_id\":\"test.example.invalid\"}", raw).await;
        if enabled {
            db.lists().update(&"test.example.invalid".parse().unwrap(),&serde_json::json!({"dmarc_mitigate_action":"munge_from","dmarc_mitigate_unconditionally":true})).await.unwrap();
        }
        let (sent, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(capture_data(&sink), deliver_one(&db, &role, lease))
        })
        .await
        .unwrap();
        let from = listmngr_mail::header_value(&sent, "From").unwrap();
        assert_eq!(from.contains("via test@example.invalid"), enabled);
        assert_eq!(
            listmngr_mail::header_value(&sent, "Sender").is_none(),
            enabled
        );
        assert!(sent.ends_with(b"\r\n\r\nbody\r\n"));
    }
    let raw = b"From: invalid\r\nMessage-ID: <bad@example.invalid>\r\n\r\nbody\r\n";
    let (db, lease, role, sink) = fixture("{\"list_id\":\"test.example.invalid\"}", raw).await;
    let job = lease.job.id;
    db.lists().update(&"test.example.invalid".parse().unwrap(),&serde_json::json!({"dmarc_mitigate_action":"munge_from","dmarc_mitigate_unconditionally":true})).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), deliver_one(&db, &role, lease))
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), sink.accept())
            .await
            .is_err()
    );
    let queue: String = sqlx::query_scalar("SELECT queue FROM queue_jobs WHERE id=$1")
        .bind(job.0.to_string())
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(queue, "shunt");
}

#[tokio::test]
async fn loop_checks_all_original_list_post_fields_before_cooking() {
    for markers in [
        "List-Post: <mailto:other@example.invalid>\r\nlIsT-pOsT:\r\n\t<MAILTO:TEST@example.invalid>\r\n",
        "List-Post: <https://example.invalid/post>,\r\n <mailto:test@example.invalid?subject=post>\r\n",
    ] {
        let raw = format!("Message-ID: <loop@example.invalid>\r\n{markers}\r\nbody");
        let (db, _, _, _) = fixture("{\"list_id\":\"test.example.invalid\"}", raw.as_bytes()).await;
        let ctx = crate::policy_facts::gather_context(
            &db,
            &listmngr_core::Config::default(),
            &"test.example.invalid".parse().unwrap(),
            Some("author@example.invalid"),
            raw.as_bytes(),
        )
        .await
        .unwrap();
        assert!(ctx.sender.is_loop, "{markers}");
        assert!(matches!(
            listmngr_pipeline::decide_posting(&ctx),
            listmngr_pipeline::Disposition::Discard(_)
        ));
    }
}

#[tokio::test]
async fn cross_list_history_survives_anonymous_smtp_normalization() {
    let raw = b"Message-ID: <cross@example.invalid>\r\nList-Post: <mailto:prior@example.invalid>\r\nlIsT-pOsT:\r\n <mailto:older@example.invalid>\r\nX-BeenThere: oldest@example.invalid\r\n\r\nbody\r\n";
    let (db, lease, mut role, sink) = fixture("{\"list_id\":\"test.example.invalid\"}", raw).await;
    role.command_timeout = Duration::from_secs(2);
    sqlx::query("UPDATE mailing_lists SET anonymous_list=1")
        .execute(db.pool())
        .await
        .unwrap();
    let (sent, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(capture_data(&sink), deliver_one(&db, &role, lease))
    })
    .await
    .unwrap();
    assert_eq!(
        listmngr_mail::header_value(&sent, "list-post").as_deref(),
        Some("<mailto:test@example.invalid>")
    );
    for name in ["prior", "older", "oldest", "unrelated", "notest"] {
        db.lists()
            .create(NewList {
                list_id: format!("{name}.example.invalid").parse().unwrap(),
                display_name: name.into(),
                style: "legacy-default".into(),
            })
            .await
            .unwrap();
    }
    for name in ["test", "prior", "older", "oldest", "unrelated", "notest"] {
        let ctx = crate::policy_facts::gather_context(
            &db,
            &listmngr_core::Config::default(),
            &format!("{name}.example.invalid").parse().unwrap(),
            Some("author@example.invalid"),
            &sent,
        )
        .await
        .unwrap();
        assert_eq!(
            ctx.sender.is_loop,
            !["unrelated", "notest"].contains(&name),
            "{name}"
        );
    }
    // A second list replaces List-Post again, but must not erase prior hops.
    let (again, _) = prepare(
        &db,
        &sent,
        "{\"list_id\":\"unrelated.example.invalid\"}",
        uuid::Uuid::now_v7(),
    )
    .await
    .ok()
    .unwrap();
    let ctx = crate::policy_facts::gather_context(
        &db,
        &listmngr_core::Config::default(),
        &"test.example.invalid".parse().unwrap(),
        Some("author@example.invalid"),
        &again,
    )
    .await
    .unwrap();
    assert!(ctx.sender.is_loop);
}

#[tokio::test]
async fn anonymous_message_id_is_stable_across_retry_preparation() {
    let context = "{\"list_id\":\"test.example.invalid\"}";
    let raw = b"Message-ID: <author@private.invalid>\r\n\r\nbody";
    let (db, lease, _, _) = fixture(context, raw).await;
    sqlx::query("UPDATE mailing_lists SET anonymous_list=1")
        .execute(db.pool())
        .await
        .unwrap();
    let first = prepare_delivery(&db, &lease, raw, context).await.unwrap();
    let retry = prepare_delivery(&db, &lease, raw, context).await.unwrap();
    assert_eq!(first, retry);
}

pub async fn capture_data(sink: &tokio::net::TcpListener) -> Vec<u8> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (stream, _) = sink.accept().await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    writer.write_all(b"220 sink\r\n").await.unwrap();
    for (prefix, reply) in [
        ("EHLO", "250-sink\r\n250 8BITMIME\r\n"),
        ("MAIL FROM:<test-bounces@example.invalid>", "250 ok\r\n"),
        ("RCPT TO:<member@example.invalid>", "250 ok\r\n"),
        ("DATA", "354 go\r\n"),
    ] {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).await.unwrap() > 0);
        assert!(line.starts_with(prefix), "{line}");
        writer.write_all(reply.as_bytes()).await.unwrap();
    }
    let mut data = Vec::new();
    loop {
        let mut line = Vec::new();
        assert!(reader.read_until(b'\n', &mut line).await.unwrap() > 0);
        if line == b".\r\n" {
            break;
        }
        data.extend_from_slice(if line.starts_with(b"..") {
            &line[1..]
        } else {
            &line
        });
    }
    writer.write_all(b"250 accepted\r\n").await.unwrap();
    data
}

#[tokio::test]
async fn anonymous_setting_controls_production_smtp_identity_not_mime_body() {
    let raw = b"From: Author <author@private.invalid>\r\nfRoM: duplicate@private.invalid\r\nReply-To: reply@private.invalid\r\nSender: sender@private.invalid\r\nTo: test@example.invalid, author@private.invalid\r\nCc: cc@private.invalid\r\nMessage-ID: <identity@private.invalid>\r\nReferences: <thread@private.invalid>\r\nIn-Reply-To: <thread@private.invalid>\r\nReceived: from private.invalid\r\n\tby private.invalid\r\nResent-From: resent@private.invalid\r\nX-Original-From: private.invalid\r\nX-Arbitrary-Identity: private.invalid\r\nAuthentication-Results: private.invalid\r\nDKIM-Signature: private.invalid\r\n folded-signature\r\nARC-Seal: private.invalid\r\nARC-Message-Signature: private.invalid\r\nARC-Authentication-Results: private.invalid\r\nDomainKey-Signature: private.invalid\r\nBcc: hidden-secret\r\nResent-Bcc: hidden-secret\r\nApproved: hidden-secret\r\nList-Archive: <https://obsolete.invalid>\r\nSubject: Hello\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed;\r\n boundary=boundary\r\n\r\n--boundary\r\nContent-Type: application/octet-stream\r\nContent-Transfer-Encoding: base64\r\n\r\nAP9ib2R5\r\n--boundary--\r\n";
    for anonymous in [true, false] {
        let (db, lease, mut role, sink) =
            fixture("{\"list_id\":\"test.example.invalid\"}", raw).await;
        role.command_timeout = Duration::from_secs(2);
        sqlx::query("UPDATE mailing_lists SET anonymous_list=$1 WHERE list_id=$2")
            .bind(i64::from(anonymous))
            .bind("test.example.invalid")
            .execute(db.pool())
            .await
            .unwrap();
        let (sent, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(capture_data(&sink), deliver_one(&db, &role, lease.clone()))
        })
        .await
        .unwrap();
        let boundary = sent.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let original_boundary = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        assert_eq!(&sent[boundary + 4..], &raw[original_boundary + 4..]);
        let headers = String::from_utf8_lossy(&sent[..boundary]);
        assert_eq!(headers.contains("private.invalid"), !anonymous, "{headers}");
        if anonymous {
            assert_eq!(
                listmngr_mail::header_value(&sent, "from").as_deref(),
                Some("test@example.invalid")
            );
            assert_eq!(
                listmngr_mail::header_value(&sent, "reply-to").as_deref(),
                Some("test@example.invalid")
            );
            assert!(
                listmngr_mail::parse_message_id(&sent)
                    .unwrap()
                    .ends_with("@example.invalid")
            );
        } else {
            assert_eq!(
                listmngr_mail::header_value(&sent, "from").as_deref(),
                Some("Author <author@private.invalid>")
            );
            assert_eq!(
                listmngr_mail::header_value(&sent, "reply-to").as_deref(),
                Some("reply@private.invalid")
            );
            assert_eq!(
                listmngr_mail::parse_message_id(&sent).unwrap(),
                "identity@private.invalid"
            );
        }
        for name in [
            "bcc",
            "resent-bcc",
            "approved",
            "list-archive",
            "dkim-signature",
            "domainkey-signature",
            "arc-seal",
            "arc-message-signature",
            "arc-authentication-results",
        ] {
            assert_eq!(listmngr_mail::header_value(&sent, name), None, "{name}");
        }
        assert!(!headers.contains("hidden-secret"));
        assert!(headers.contains("Content-Type: multipart/mixed;\r\n boundary=boundary"));
        assert_eq!(
            db.mail_queue().job(lease.job.id).await.unwrap().state,
            JobState::Done
        );
        assert_eq!(
            db.mail_queue()
                .message(lease.job.message_id)
                .await
                .unwrap()
                .raw,
            raw
        );
    }
}

use listmngr_db::{
    NewList,
    mail_queue::{ChildJob, JobState, NewMessage},
};

pub async fn fixture(
    context: &str,
    raw: &[u8],
) -> (Database, Lease, MailRoleConfig, tokio::net::TcpListener) {
    fixture_with_recipients(context, raw, vec!["member@example.invalid".into()]).await
}

async fn fixture_with_recipients(
    context: &str,
    raw: &[u8],
    recipients: Vec<String>,
) -> (Database, Lease, MailRoleConfig, tokio::net::TcpListener) {
    fixture_at("sqlite::memory:", context, raw, recipients).await
}

pub(super) async fn fixture_at(
    url: &str,
    context: &str,
    raw: &[u8],
    recipients: Vec<String>,
) -> (Database, Lease, MailRoleConfig, tokio::net::TcpListener) {
    let db = Database::connect(url, 1).await.unwrap();
    fixture_on(db, context, raw, recipients).await
}

pub(super) async fn fixture_on(
    db: Database,
    context: &str,
    raw: &[u8],
    recipients: Vec<String>,
) -> (Database, Lease, MailRoleConfig, tokio::net::TcpListener) {
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "test", None)
        .await
        .unwrap();
    db.lists()
        .create(NewList {
            list_id: "test.example.invalid".parse().unwrap(),
            display_name: "Test".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    // These fixtures assert transport invariants on byte-identical bodies;
    // delivery decoration has its own suite (`tests/post_effects.rs`).
    db.templates()
        .set_body(
            &listmngr_db::templates::Scope::List("test.example.invalid".parse().unwrap()),
            "list:member:regular:footer",
            "en",
            "",
        )
        .await
        .unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: raw.to_vec(),
                external_id: "out-test".into(),
                context: context.into(),
                queue: Queue::In,
                max_attempts: 5,
            },
            now,
        )
        .await
        .unwrap();
    let source = db
        .mail_queue()
        .claim(Queue::In, "in", now, 20000)
        .await
        .unwrap()
        .unwrap();
    db.mail_queue()
        .complete_with_children(
            &source,
            now,
            &[ChildJob {
                queue: Queue::Out,
                max_attempts: 5,
                recipients,
            }],
        )
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::Out, "out", now, 20000)
        .await
        .unwrap()
        .unwrap();
    let sink = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut role = MailRoleConfig::from_core(&tests::plaintext_config()).unwrap();
    role.smtp_relay = sink.local_addr().unwrap();
    role.command_timeout = Duration::from_millis(50);
    (db, lease, role, sink)
}

#[tokio::test]
async fn mixed_and_missing_outcomes_persist_and_retry_only_pending_recipients() {
    let recipients: Vec<String> = (0..5).map(|n| format!("r{n}@example.invalid")).collect();
    let raw = b"Subject: mixed\r\n\r\nbody";
    let (db, lease, role, _sink) = fixture_with_recipients(
        "{\"list_id\":\"test.example.invalid\"}",
        raw,
        recipients.clone(),
    )
    .await;
    finish_delivery(
        &db,
        &role,
        &lease,
        &recipients,
        &[
            RecipientStatus::Sent,
            RecipientStatus::PermanentFailure("550 rejected".into()),
            RecipientStatus::Ambiguous("remote outcome unknown".into()),
            RecipientStatus::TransientFailure("451 deferred".into()),
            // Fifth recipient deliberately has no outcome: it must stay pending.
        ],
    )
    .await;
    let states: Vec<(String, String)> = sqlx::query_as(
        "SELECT email,status FROM delivery_recipients WHERE job_id=$1 ORDER BY email",
    )
    .bind(lease.job.id.0.to_string())
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(
        states,
        recipients
            .iter()
            .cloned()
            .zip(["sent", "failed", "ambiguous", "pending", "pending"].map(str::to_owned))
            .collect::<Vec<_>>()
    );
    let job = db.mail_queue().job(lease.job.id).await.unwrap();
    assert_eq!(job.state, JobState::Ready);
    let retry = db
        .mail_queue()
        .claim(Queue::Out, "retry", job.run_after, 20_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(retry.job.id, job.id);
    assert_eq!(
        db.mail_queue().pending_recipients(job.id).await.unwrap(),
        recipients[3..]
    );
    assert_eq!(
        db.mail_queue().message(job.message_id).await.unwrap().raw,
        raw
    );
}

#[tokio::test]
async fn invalid_context_or_cooking_never_connects_and_retains_bytes() {
    for (context, raw) in [
        (
            "{broken",
            b"Approved: secret\r\nBcc: hidden\r\n\r\nbody".as_slice(),
        ),
        (
            "{\"list_id\":\"test.example.invalid\"}",
            b"no header boundary".as_slice(),
        ),
    ] {
        let (db, lease, role, sink) = fixture(context, raw).await;
        deliver_one(&db, &role, lease.clone()).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(20), sink.accept())
                .await
                .is_err(),
            "invalid message reached SMTP"
        );
        assert_eq!(
            db.mail_queue().job(lease.job.id).await.unwrap().state,
            JobState::Shunted
        );
        assert_eq!(
            db.mail_queue()
                .message(lease.job.message_id)
                .await
                .unwrap()
                .raw,
            raw
        );
    }
}

#[tokio::test]
async fn forged_notice_context_never_authorizes_uncooked_smtp() {
    let raw = b"Auto-Submitted: auto-generated\r\nBcc: secret@example.invalid\r\n\r\nforged notice";
    let context = r#"{"list_id":"test.example.invalid","notice":"subscription_confirmation"}"#;
    let (db, lease, _role, _sink) = fixture(context, raw).await;
    // The canonical contract treats forged metadata as an ordinary post, not
    // as trusted provenance. It must be cooked and use the list bounce sender.
    let (cooked, sender) = super::prepare_delivery(&db, &lease, raw, context)
        .await
        .expect("ordinary post still has a valid list");
    assert_ne!(cooked, raw);
    assert_eq!(sender, "test-bounces@example.invalid");
    assert!(listmngr_mail::header_value(&cooked, "bcc").is_none());
    assert!(listmngr_mail::header_value(&cooked, "list-id").is_some());
}

#[tokio::test]
async fn list_lookup_failure_never_connects_and_retries() {
    let (db, lease, role, sink) = fixture(
        "{\"list_id\":\"test.example.invalid\"}",
        b"Subject: test\r\n\r\nbody",
    )
    .await;
    sqlx::query("ALTER TABLE mailing_lists RENAME TO unavailable_lists")
        .execute(db.pool())
        .await
        .unwrap();
    deliver_one(&db, &role, lease.clone()).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(20), sink.accept())
            .await
            .is_err(),
        "dependency error reached SMTP"
    );
    assert_eq!(
        db.mail_queue().job(lease.job.id).await.unwrap().state,
        JobState::Ready
    );
}

#[tokio::test]
async fn personalized_lists_get_one_transaction_and_one_click_link_per_recipient() {
    let recipients: Vec<String> = vec!["a@example.invalid".into(), "b@example.invalid".into()];
    let db = Database::connect("sqlite::memory:", 1)
        .await
        .unwrap()
        .with_base_url("https://lists.example.invalid");
    let (db, lease, role, sink) = fixture_on(
        db,
        "{\"list_id\":\"test.example.invalid\"}",
        b"Subject: personal\r\nMessage-ID: <p@example.invalid>\r\n\r\nfixture-body\r\n",
        recipients.clone(),
    )
    .await;
    let list_id: listmngr_core::ListId = "test.example.invalid".parse().unwrap();
    db.lists()
        .update(&list_id, &serde_json::json!({"personalize": "individual"}))
        .await
        .unwrap();
    for email in &recipients {
        db.members()
            .create(listmngr_db::NewMember {
                list_id: list_id.clone(),
                email: email.clone(),
                display_name: String::new(),
                role: listmngr_core::MemberRole::Member,
                subscription_mode: listmngr_core::SubscriptionMode::AsAddress,
            })
            .await
            .unwrap();
    }
    let capture = async {
        let mut payloads = Vec::new();
        for recipient in &recipients {
            payloads.push(
                capture_envelope(
                    &sink,
                    "test-bounces@example.invalid",
                    std::slice::from_ref(recipient),
                )
                .await,
            );
        }
        payloads
    };
    let (payloads, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(capture, deliver_one(&db, &role, lease.clone()))
    })
    .await
    .unwrap();
    assert_eq!(payloads.len(), 2, "one transaction per recipient");
    let mut links = Vec::new();
    for (payload, recipient) in payloads.iter().zip(&recipients) {
        let unsubscribe = listmngr_mail::header_value(payload, "List-Unsubscribe").unwrap();
        assert!(
            unsubscribe.starts_with(
                "<https://lists.example.invalid/unsubscribe/test.example.invalid?token="
            ),
            "{recipient}: {unsubscribe}"
        );
        assert!(
            unsubscribe.ends_with(">, <mailto:test-leave@example.invalid>"),
            "{unsubscribe}"
        );
        assert_eq!(
            listmngr_mail::header_value(payload, "List-Unsubscribe-Post").as_deref(),
            Some("List-Unsubscribe=One-Click")
        );
        assert!(
            !unsubscribe.contains(recipient.as_str()),
            "no address in the link"
        );
        links.push(unsubscribe);
        assert!(payload.ends_with(b"fixture-body\r\n"));
    }
    assert_ne!(links[0], links[1], "each recipient gets their own token");
    redeems_for_its_own_recipient(&db, &list_id, &links[0]).await;
    assert_eq!(
        db.mail_queue().job(lease.job.id).await.unwrap().state,
        JobState::Done
    );
}

/// Every link redeems for its own recipient only.
async fn redeems_for_its_own_recipient(db: &Database, list_id: &listmngr_core::ListId, link: &str) {
    let token = link
        .split("token=")
        .nth(1)
        .unwrap()
        .split('>')
        .next()
        .unwrap()
        .to_owned();
    let member = db
        .one_click()
        .redeem(
            list_id,
            &token,
            chrono::Utc::now().timestamp(),
            &listmngr_db::AuditContext::system(),
        )
        .await
        .unwrap();
    assert!(db.members().get(member.id).await.is_err());
    assert_eq!(
        db.members().find("b@example.invalid").await.unwrap().len(),
        1
    );
}

#[tokio::test]
async fn unpersonalized_lists_keep_one_transaction_and_the_mailto_only_header() {
    let recipients: Vec<String> = vec!["a@example.invalid".into(), "b@example.invalid".into()];
    let db = Database::connect("sqlite::memory:", 1)
        .await
        .unwrap()
        .with_base_url("https://lists.example.invalid");
    let (db, lease, role, sink) = fixture_on(
        db,
        "{\"list_id\":\"test.example.invalid\"}",
        b"Subject: shared\r\nMessage-ID: <s@example.invalid>\r\n\r\nfixture-body\r\n",
        recipients.clone(),
    )
    .await;
    let (payload, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            capture_envelope(&sink, "test-bounces@example.invalid", &recipients),
            deliver_one(&db, &role, lease.clone())
        )
    })
    .await
    .unwrap();
    assert_eq!(
        listmngr_mail::header_value(&payload, "List-Unsubscribe").as_deref(),
        Some("<mailto:test-leave@example.invalid>")
    );
    assert!(listmngr_mail::header_value(&payload, "List-Unsubscribe-Post").is_none());
}

fn role_with(sink: &tokio::net::TcpListener, mta: &serde_json::Value) -> MailRoleConfig {
    let mut config = serde_json::json!({"mta": {"smtp_tls": "plaintext_trusted_relay"}});
    config["mta"]
        .as_object_mut()
        .unwrap()
        .extend(mta.as_object().unwrap().clone());
    let config: listmngr_core::Config = serde_json::from_value(config).unwrap();
    let mut role = MailRoleConfig::from_core(&config).unwrap();
    role.smtp_relay = sink.local_addr().unwrap();
    role.command_timeout = Duration::from_secs(2);
    role
}

#[tokio::test]
async fn full_personalization_rewrites_to_expands_user_placeholders_and_verps_the_envelope() {
    let recipients: Vec<String> = vec!["a@example.invalid".into(), "b@example.invalid".into()];
    let db = Database::connect("sqlite::memory:", 1)
        .await
        .unwrap()
        .with_base_url("https://lists.example.invalid");
    let (db, lease, _, sink) = fixture_on(
        db,
        "{\"list_id\":\"test.example.invalid\"}",
        b"From: author@elsewhere.invalid\r\nTo: test@example.invalid\r\nSubject: personal\r\nMessage-ID: <f@example.invalid>\r\nContent-Type: text/plain\r\n\r\nfixture-body\r\n",
        recipients.clone(),
    )
    .await;
    let role = role_with(
        &sink,
        &serde_json::json!({"verp_personalized_deliveries": true}),
    );
    let list_id: listmngr_core::ListId = "test.example.invalid".parse().unwrap();
    db.lists()
        .update(&list_id, &serde_json::json!({"personalize": "full"}))
        .await
        .unwrap();
    db.templates()
        .set_body(
            &listmngr_db::templates::Scope::List(list_id.clone()),
            "list:member:regular:footer",
            "en",
            "-- \nfor $user_email ($user_name, $user_language)\n",
        )
        .await
        .unwrap();
    for (email, name) in [("a@example.invalid", "Ann"), ("b@example.invalid", "")] {
        db.members()
            .create(listmngr_db::NewMember {
                list_id: list_id.clone(),
                email: email.into(),
                display_name: name.into(),
                role: listmngr_core::MemberRole::Member,
                subscription_mode: listmngr_core::SubscriptionMode::AsAddress,
            })
            .await
            .unwrap();
    }
    let capture = async {
        let mut payloads = Vec::new();
        for recipient in &recipients {
            let local = recipient.split('@').next().unwrap();
            payloads.push(
                capture_envelope(
                    &sink,
                    &format!("test-bounces+{local}=example.invalid@example.invalid"),
                    std::slice::from_ref(recipient),
                )
                .await,
            );
        }
        payloads
    };
    let (payloads, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(capture, deliver_one(&db, &role, lease.clone()))
    })
    .await
    .unwrap();
    let first = String::from_utf8_lossy(&payloads[0]);
    assert!(first.contains("To: Ann <a@example.invalid>\r\n"), "{first}");
    assert!(!first.contains("To: test@example.invalid"), "{first}");
    assert!(first.contains("for a@example.invalid (Ann, en)"), "{first}");
    assert!(
        first.contains("List-Unsubscribe-Post: List-Unsubscribe=One-Click"),
        "{first}"
    );
    let second = String::from_utf8_lossy(&payloads[1]);
    assert!(second.contains("To: b@example.invalid\r\n"), "{second}");
    assert!(second.contains("for b@example.invalid (, en)"), "{second}");
    assert_eq!(
        db.mail_queue().job(lease.job.id).await.unwrap().state,
        JobState::Done
    );
}

#[tokio::test]
async fn the_verp_delivery_interval_splits_an_ordinary_post_without_personalizing_it() {
    let recipients: Vec<String> = vec!["a@example.invalid".into(), "b@example.invalid".into()];
    let (db, lease, _, sink) = fixture_with_recipients(
        "{\"list_id\":\"test.example.invalid\"}",
        b"From: author@elsewhere.invalid\r\nTo: test@example.invalid\r\nSubject: interval\r\nMessage-ID: <i@example.invalid>\r\n\r\nfixture-body\r\n",
        recipients.clone(),
    )
    .await;
    let role = role_with(&sink, &serde_json::json!({"verp_delivery_interval": 1}));
    let capture = async {
        let mut payloads = Vec::new();
        for recipient in &recipients {
            let local = recipient.split('@').next().unwrap();
            payloads.push(
                capture_envelope(
                    &sink,
                    &format!("test-bounces+{local}=example.invalid@example.invalid"),
                    std::slice::from_ref(recipient),
                )
                .await,
            );
        }
        payloads
    };
    let (payloads, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(capture, deliver_one(&db, &role, lease.clone()))
    })
    .await
    .unwrap();
    assert_eq!(
        payloads[0], payloads[1],
        "the copies are identical; only the envelope differs"
    );
    let text = String::from_utf8_lossy(&payloads[0]);
    assert!(text.contains("To: test@example.invalid\r\n"), "{text}");
    assert!(!text.contains("List-Unsubscribe-Post"), "{text}");

    // Interval 2 on post_id 1 (fresh list): a single shared transaction.
    let (db, lease, _, sink) = fixture_with_recipients(
        "{\"list_id\":\"test.example.invalid\"}",
        b"Subject: shared\r\nMessage-ID: <s2@example.invalid>\r\n\r\nfixture-body\r\n",
        recipients.clone(),
    )
    .await;
    let role = role_with(&sink, &serde_json::json!({"verp_delivery_interval": 2}));
    let (payload, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            capture_envelope(&sink, "test-bounces@example.invalid", &recipients),
            deliver_one(&db, &role, lease)
        )
    })
    .await
    .unwrap();
    assert!(payload.ends_with(b"fixture-body\r\n"));
}

#[tokio::test]
async fn shared_deliveries_are_chunked_by_domain_up_to_the_transaction_limit() {
    let recipients: Vec<String> = vec![
        "a@one.invalid".into(),
        "b@two.invalid".into(),
        "c@one.invalid".into(),
    ];
    let (db, lease, _, sink) = fixture_with_recipients(
        "{\"list_id\":\"test.example.invalid\"}",
        b"Subject: chunked\r\nMessage-ID: <c@example.invalid>\r\n\r\nfixture-body\r\n",
        recipients.clone(),
    )
    .await;
    let role = role_with(
        &sink,
        &serde_json::json!({"max_recipients_per_transaction": 2}),
    );
    let capture = async {
        let first = capture_envelope(
            &sink,
            "test-bounces@example.invalid",
            &["a@one.invalid".to_owned(), "c@one.invalid".to_owned()],
        )
        .await;
        let second = capture_envelope(
            &sink,
            "test-bounces@example.invalid",
            &["b@two.invalid".to_owned()],
        )
        .await;
        (first, second)
    };
    let ((first, second), ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(capture, deliver_one(&db, &role, lease.clone()))
    })
    .await
    .unwrap();
    assert_eq!(first, second, "one signed copy shared by every transaction");
    assert_eq!(
        db.mail_queue().job(lease.job.id).await.unwrap().state,
        JobState::Done
    );
    let sent: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM delivery_recipients WHERE job_id=$1 AND status='sent'",
    )
    .bind(lease.job.id.0.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(sent, 3);
}

#[tokio::test]
async fn an_unreachable_relay_backs_off_exponentially_with_jitter() {
    let (db, lease, mut role, sink) = fixture(
        "{\"list_id\":\"test.example.invalid\"}",
        b"Subject: retry\r\nMessage-ID: <r@example.invalid>\r\n\r\nfixture-body\r\n",
    )
    .await;
    // Nothing listens once the sink is dropped: the connection is refused.
    let relay = sink.local_addr().unwrap();
    drop(sink);
    role.smtp_relay = relay;
    role.backoff = crate::delivery_policy::Backoff {
        initial_ms: 10_000,
        max_ms: 3_600_000,
    };
    let before = chrono::Utc::now().timestamp_millis();
    deliver_one(&db, &role, lease.clone()).await;
    let job = db.mail_queue().job(lease.job.id).await.unwrap();
    assert_eq!(job.state, JobState::Ready);
    assert_eq!(job.attempts, 1);
    let delay = job.run_after - before;
    assert!(
        (8_000..=12_500).contains(&delay),
        "first retry ≈ 10 s ± 20%: {delay}"
    );

    // The second attempt backs off twice as long.
    let lease = db
        .mail_queue()
        .claim(Queue::Out, "out", job.run_after + 1, 20_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lease.job.attempts, 2);
    let before = chrono::Utc::now().timestamp_millis();
    deliver_one(&db, &role, lease.clone()).await;
    let job = db.mail_queue().job(lease.job.id).await.unwrap();
    let delay = job.run_after - before;
    assert!(
        (16_000..=24_500).contains(&delay),
        "second retry ≈ 20 s ± 20%: {delay}"
    );
}
