//! O2/O3: actual TCP DATA + failed audit commit + closed pool + reclaim.
use super::tests::fixture_at;
use super::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

async fn receive_data(sink: &tokio::net::TcpListener, final_reply: bool) {
    receive_data_with_barrier(sink, final_reply, None).await;
}

async fn receive_data_with_barrier(
    sink: &tokio::net::TcpListener,
    final_reply: bool,
    barrier: Option<tokio::sync::oneshot::Sender<()>>,
) {
    let (stream, _) = sink.accept().await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    writer.write_all(b"220 sink\r\n").await.unwrap();
    for (prefix, reply) in [
        ("EHLO", "250 sink\r\n"),
        ("MAIL", "250 ok\r\n"),
        ("RCPT", "250 ok\r\n"),
        ("DATA", "354 go\r\n"),
    ] {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).await.unwrap() > 0);
        assert!(line.starts_with(prefix));
        writer.write_all(reply.as_bytes()).await.unwrap();
    }
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).await.unwrap() > 0);
        if line == ".\r\n" {
            break;
        }
    }
    if final_reply {
        writer.write_all(b"250 accepted\r\n").await.unwrap();
    }
    if let Some(barrier) = barrier {
        barrier.send(()).unwrap();
        std::future::pending::<()>().await;
    }
}

#[tokio::test]
async fn canceled_after_data_restart_never_replays() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}/queue.sqlite?mode=rwc", dir.path().display());
    let (db, lease, mut role, sink) = fixture_at(
        &url,
        "{\"list_id\":\"test.example.invalid\"}",
        b"Subject: canceled\r\n\r\nbody",
        vec!["member@example.invalid".into()],
    )
    .await;
    role.command_timeout = Duration::from_secs(30);
    let (sent, received) = tokio::sync::oneshot::channel();
    {
        let relay = receive_data_with_barrier(&sink, false, Some(sent));
        let delivery = deliver_one(&db, &role, lease.clone());
        tokio::pin!(relay, delivery);
        tokio::select! {
            () = &mut relay => panic!("relay must hold final reply"),
            () = &mut delivery => panic!("delivery must wait for final reply"),
            result = received => result.unwrap(),
        }
        // Drop/cancel the real worker while DATA is at the relay, before finish.
    }
    db.pool().close().await;
    let db = Database::connect(&url, 1).await.unwrap();
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
    let (second_data, ()) = tokio::join!(
        tokio::time::timeout(Duration::from_millis(150), receive_data(&sink, true)),
        deliver_one(&db, &role, recovered),
    );
    assert!(second_data.is_err(), "canceled attempt replayed DATA");
    let (status, detail): (String, String) =
        sqlx::query_as("SELECT status,detail FROM delivery_recipients WHERE job_id=$1")
            .bind(lease.job.id.0.to_string())
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(status, "ambiguous");
    assert!(detail.contains("unknown"));
    db.pool().close().await;
}

async fn failed_outcome_recovery(final_reply: bool) {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}/queue.sqlite?mode=rwc", dir.path().display());
    failed_outcome_recovery_at(&url, final_reply).await;
}

async fn failed_outcome_recovery_at(url: &str, final_reply: bool) {
    let raw = b"Subject: durable\r\n\r\nbody";
    let (db, lease, role, sink) = fixture_at(
        url,
        "{\"list_id\":\"test.example.invalid\"}",
        raw,
        vec!["member@example.invalid".into()],
    )
    .await;
    if url.starts_with("sqlite:") {
        sqlx::query("CREATE TRIGGER fail_ack BEFORE INSERT ON audit_log WHEN NEW.action='queue.ack' BEGIN SELECT RAISE(ABORT, 'sabotage outcome audit'); END")
            .execute(db.pool()).await.unwrap();
    } else {
        sqlx::query("CREATE FUNCTION fail_ack_fn() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='queue.ack' THEN RAISE EXCEPTION 'sabotage outcome audit'; END IF; RETURN NEW; END $$")
            .execute(db.pool()).await.unwrap();
        sqlx::query("CREATE TRIGGER fail_ack BEFORE INSERT ON audit_log FOR EACH ROW EXECUTE FUNCTION fail_ack_fn()")
            .execute(db.pool()).await.unwrap();
    }
    tokio::join!(
        receive_data(&sink, final_reply),
        deliver_one(&db, &role, lease.clone())
    );
    assert_eq!(
        db.mail_queue().job(lease.job.id).await.unwrap().state,
        listmngr_db::mail_queue::JobState::Leased
    );
    let drop_trigger = if url.starts_with("sqlite:") {
        "DROP TRIGGER fail_ack"
    } else {
        "DROP TRIGGER fail_ack ON audit_log"
    };
    sqlx::query(drop_trigger).execute(db.pool()).await.unwrap();
    db.pool().close().await;
    drop(db);
    let db = Database::connect(url, 1).await.unwrap();
    let reclaimed = db
        .mail_queue()
        .claim(
            Queue::Out,
            "restarted",
            lease.job.lease_until.unwrap() + 1,
            20_000,
        )
        .await
        .unwrap()
        .unwrap();
    // Exercise a second real SMTP transaction if the reclaimed worker replays.
    let (second_data, ()) = tokio::join!(
        tokio::time::timeout(Duration::from_millis(150), receive_data(&sink, true)),
        deliver_one(&db, &role, reclaimed),
    );
    assert!(
        second_data.is_err(),
        "reclaimed uncertain attempt sent a second DATA"
    );
    let (status, detail): (String, String) =
        sqlx::query_as("SELECT status,detail FROM delivery_recipients WHERE job_id=$1")
            .bind(lease.job.id.0.to_string())
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(status, "ambiguous");
    assert!(
        detail.contains("unknown"),
        "uncertainty must remain inspectable: {detail}"
    );
    assert!(
        db.mail_queue()
            .pending_recipients(lease.job.id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        db.mail_queue()
            .message(lease.job.message_id)
            .await
            .unwrap()
            .raw,
        raw
    );
    db.pool().close().await;
}

#[tokio::test]
async fn final250_audit_failure_restart_never_replays_data() {
    failed_outcome_recovery(true).await;
}
#[tokio::test]
async fn missingfinalreply_audit_failure_restart_never_replays_data() {
    failed_outcome_recovery(false).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; creates and drops only unique test schemas"]
async fn postgres_isolated_audit_failure_restart_never_replays_data() {
    let url = std::env::var("TEST_POSTGRES_URL").expect("TEST_POSTGRES_URL required");
    assert!(url.starts_with("postgres://") || url.starts_with("postgresql://"));
    assert!(!url.contains("options="));
    let admin = Database::connect(&url, 1).await.unwrap();
    for final_reply in [true, false] {
        let schema = format!("attempt_test_{}", uuid::Uuid::now_v7().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(admin.pool())
            .await
            .unwrap();
        let separator = if url.contains('?') { '&' } else { '?' };
        let fixture_url = format!("{url}{separator}options=-csearch_path%3D{schema}");
        let expected = schema.clone();
        let result = tokio::spawn(async move {
            let probe = Database::connect(&fixture_url, 1).await.unwrap();
            let current: String = sqlx::query_scalar("SELECT current_schema()::text")
                .fetch_one(probe.pool())
                .await
                .unwrap();
            assert_eq!(current, expected);
            probe.pool().close().await;
            failed_outcome_recovery_at(&fixture_url, final_reply).await;
        })
        .await;
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(admin.pool())
            .await
            .unwrap();
        result.unwrap();
    }
    admin.pool().close().await;
}

#[tokio::test]
async fn reservation_audit_failure_fences_smtp_and_rolls_back() {
    let (db, lease, role, sink) = super::tests::fixture_at(
        "sqlite::memory:",
        "{\"list_id\":\"test.example.invalid\"}",
        b"Subject: test\r\n\r\nbody",
        vec!["member@example.invalid".into()],
    )
    .await;
    sqlx::query("CREATE TRIGGER fail_begin BEFORE INSERT ON audit_log WHEN NEW.action='queue.delivery_begin' BEGIN SELECT RAISE(ABORT, 'sabotage begin'); END")
        .execute(db.pool()).await.unwrap();
    deliver_one(&db, &role, lease.clone()).await;
    let (stream, _) = sink.accept().await.unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    assert_eq!(
        reader.read_line(&mut line).await.unwrap(),
        0,
        "SMTP must not start without durable intent"
    );
    assert_eq!(
        db.mail_queue()
            .pending_recipients(lease.job.id)
            .await
            .unwrap(),
        vec!["member@example.invalid"]
    );
    assert_eq!(
        db.mail_queue().job(lease.job.id).await.unwrap().state,
        listmngr_db::mail_queue::JobState::Leased
    );
}

#[tokio::test]
async fn reserved_mixed_results_retry_only_known_transient_and_fence_old_lease() {
    let recipients: Vec<String> = (0..4).map(|n| format!("r{n}@example.invalid")).collect();
    let (db, lease, role, _) = fixture_at(
        "sqlite::memory:",
        "{}",
        b"Subject: test\r\n\r\nbody",
        recipients.clone(),
    )
    .await;
    let now = chrono::Utc::now().timestamp_millis();
    db.mail_queue()
        .begin_delivery(&lease, now, &recipients)
        .await
        .unwrap();
    finish_delivery(
        &db,
        &role,
        &lease,
        &recipients,
        &[
            RecipientStatus::Sent,
            RecipientStatus::TransientFailure("451 known deferred".into()),
            RecipientStatus::Ambiguous("unknown final reply".into()),
            // Missing result must not erase the durable unknown attempt.
        ],
    )
    .await;
    assert_eq!(
        db.mail_queue()
            .pending_recipients(lease.job.id)
            .await
            .unwrap(),
        vec![recipients[1].clone()]
    );
    let job = db.mail_queue().job(lease.job.id).await.unwrap();
    let current = db
        .mail_queue()
        .claim(Queue::Out, "next", job.run_after, 20_000)
        .await
        .unwrap()
        .unwrap();
    assert!(
        db.mail_queue()
            .begin_delivery(&lease, job.run_after, &recipients[1..2])
            .await
            .is_err()
    );
    assert!(
        db.mail_queue()
            .finish_delivery(
                &lease,
                job.run_after,
                &[(
                    recipients[3].clone(),
                    RecipientOutcome::Transient,
                    "stale".into()
                )],
                0
            )
            .await
            .is_err()
    );
    // New token cannot resolve uncertainty owned by the old attempt either.
    db.mail_queue()
        .finish_delivery(
            &current,
            job.run_after,
            &[(
                recipients[3].clone(),
                RecipientOutcome::Transient,
                "unrelated".into(),
            )],
            0,
        )
        .await
        .unwrap();
    let states: Vec<String> =
        sqlx::query_scalar("SELECT status FROM delivery_recipients WHERE job_id=$1 ORDER BY email")
            .bind(job.id.0.to_string())
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(states, ["sent", "pending", "ambiguous", "ambiguous"]);
}
