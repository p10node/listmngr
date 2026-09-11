use super::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::test]
async fn dsn_issuance_reaches_real_relay_and_survives_readback() {
    dsn_tracer(None).await;
}
async fn dsn_tracer(postgres: Option<String>) {
    let (dir, _) = key_fixture();
    let key = dir.path().join("issuer.key");
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("queue.sqlite").display()
    );
    let url = postgres.unwrap_or(url);
    let recipients = vec![
        "Mixed@Example.invalid".to_owned(),
        "Zed@Example.invalid".to_owned(),
    ];
    let (db, lease, role, sink) = tests::fixture_at(
        &url,
        "{\"list_id\":\"test.example.invalid\"}",
        b"From: author@example.invalid\r\nSubject: issued_at\r\n\r\ndsn-tracer\r\n",
        recipients.clone(),
    )
    .await;
    for email in &recipients {
        add_member(&db, email).await;
    }
    let lease = fresh_delivery(&db, &lease, &recipients).await;
    let config: listmngr_core::Config = serde_json::from_value(serde_json::json!({"mta":{"smtp_tls":"plaintext_trusted_relay","smtp_single_recipient":true,"dsn_issuance_enabled":true,"dsn_key_file":key,"dsn_key_id":"test"}})).unwrap();
    let mut configured = MailRoleConfig::from_core(&config).unwrap();
    configured.smtp_relay = role.smtp_relay;
    configured.command_timeout = Duration::from_secs(2);
    let peer = capture_dsn(&db, &lease, &sink, &recipients);
    let (tokens, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(peer, deliver_one(&db, &configured, lease.clone()))
    })
    .await
    .unwrap();
    assert_ne!(tokens[0], tokens[1]);
    db.pool().close().await;
    let reopened = Database::connect(&url, 1).await.unwrap();
    let stored: Vec<String> =
        sqlx::query_scalar("SELECT envid FROM dsn_issuances WHERE job_id=$1 ORDER BY recipient")
            .bind(lease.job.id.0.to_string())
            .fetch_all(reopened.pool())
            .await
            .unwrap();
    assert_eq!(stored, tokens);
    assert!(
        reopened
            .mail_queue()
            .pending_recipients(lease.job.id)
            .await
            .unwrap()
            .is_empty()
    );
    reopened.pool().close().await;
}

async fn capture_dsn(
    db: &Database,
    lease: &Lease,
    sink: &tokio::net::TcpListener,
    recipients: &[String],
) -> Vec<String> {
    let mut tokens = Vec::new();
    for recipient in recipients {
        let (stream, _) = sink.accept().await.unwrap();
        let (r, mut w) = stream.into_split();
        let mut r = BufReader::new(r);
        w.write_all(b"220 fixture\r\n").await.unwrap();
        let mut line = String::new();
        r.read_line(&mut line).await.unwrap();
        assert!(line.starts_with("EHLO "));
        w.write_all(b"250-fixture\r\n250 DSN\r\n").await.unwrap();
        line.clear();
        r.read_line(&mut line).await.unwrap();
        let token = line
            .trim_end()
            .strip_prefix("MAIL FROM:<test-bounces@example.invalid> ENVID=")
            .expect("durable ENVID must reach actual MAIL command")
            .to_owned();
        assert!(token.len() <= 100);
        let committed: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM dsn_issuances WHERE envid=$1 AND job_id=$2")
                .bind(&token)
                .bind(lease.job.id.0.to_string())
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(committed, 1, "issuance must commit before MAIL is accepted");
        tokens.push(token);
        w.write_all(b"250 ok\r\n").await.unwrap();
        line.clear();
        r.read_line(&mut line).await.unwrap();
        assert_eq!(line, format!("RCPT TO:<{recipient}>\r\n"));
        w.write_all(b"250 ok\r\n").await.unwrap();
        line.clear();
        r.read_line(&mut line).await.unwrap();
        assert_eq!(line, "DATA\r\n");
        w.write_all(b"354 go\r\n").await.unwrap();
        loop {
            line.clear();
            assert!(r.read_line(&mut line).await.unwrap() > 0);
            if line == ".\r\n" {
                break;
            }
        }
        w.write_all(b"250 accepted\r\n").await.unwrap();
    }
    tokens
}

fn issuer_config(path: &std::path::Path) -> listmngr_core::Config {
    serde_json::from_value(serde_json::json!({"mta":{"smtp_tls":"plaintext_trusted_relay","smtp_single_recipient":true,"dsn_issuance_enabled":true,"dsn_key_file":path,"dsn_key_id":"test"}})).unwrap()
}
fn key_fixture() -> (tempfile::TempDir, listmngr_core::dsn_issuance::Issuer) {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/dsn-issuance");
    std::fs::create_dir_all(&root).unwrap();
    let dir = tempfile::tempdir_in(root).unwrap();
    let key = dir.path().join("issuer.key");
    std::fs::write(&key, [42_u8; 32]).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let issuer = listmngr_core::dsn_issuance::Issuer::load(&issuer_config(&key).mta)
        .unwrap()
        .unwrap();
    (dir, issuer)
}
async fn add_member(db: &Database, email: &str) -> listmngr_core::Member {
    db.members()
        .create(listmngr_db::NewMember {
            list_id: "test.example.invalid".parse().unwrap(),
            email: email.into(),
            display_name: String::new(),
            role: listmngr_core::MemberRole::Member,
            subscription_mode: listmngr_core::SubscriptionMode::AsAddress,
        })
        .await
        .unwrap()
}
async fn fresh_delivery(db: &Database, old: &Lease, recipients: &[String]) -> Lease {
    let msg = db.mail_queue().message(old.job.message_id).await.unwrap();
    let plan = db
        .mail_queue()
        .plan_recipients(
            &"test.example.invalid".parse().unwrap(),
            "author@example.invalid",
            &msg.raw,
        )
        .await
        .unwrap()
        .restrict_to(recipients);
    fresh_delivery_with_plan(db, old, &plan).await
}
async fn fresh_delivery_with_plan(
    db: &Database,
    old: &Lease,
    plan: &listmngr_db::mail_queue::RecipientPlan,
) -> Lease {
    let recipients = plan.emails();
    let now = chrono::Utc::now().timestamp_millis();
    db.mail_queue().ack(old, now).await.unwrap();
    let msg = db.mail_queue().message(old.job.message_id).await.unwrap();
    db.mail_queue()
        .enqueue(
            listmngr_db::mail_queue::NewMessage {
                raw: msg.raw,
                external_id: "fresh-delivery".into(),
                context: msg.context,
                queue: Queue::In,
                max_attempts: 5,
            },
            now,
        )
        .await
        .unwrap();
    let source = db
        .mail_queue()
        .claim(Queue::In, "dsn-in", now, 30_000)
        .await
        .unwrap()
        .unwrap();
    db.mail_queue()
        .complete_with_plan(
            &source,
            now,
            &[listmngr_db::mail_queue::ChildJob {
                queue: Queue::Out,
                max_attempts: 5,
                recipients: recipients.clone(),
            }],
            Some(plan),
        )
        .await
        .unwrap();
    db.mail_queue()
        .claim(Queue::Out, "dsn-out", now, 30_000)
        .await
        .unwrap()
        .unwrap()
}
#[tokio::test]
async fn dsn_issuance_rejects_relink_aba() {
    let (_dir, issuer) = key_fixture();
    let recipients = vec!["Mixed@Example.invalid".into()];
    let (db, old) = control_fixture("sqlite::memory:", false, &recipients).await;
    let user = db
        .users()
        .create(listmngr_db::NewUser {
            display_name: "Owner".into(),
            email: "owner@example.invalid".into(),
            password: "fixture-owner-password".into(),
            server_owner: false,
        })
        .await
        .unwrap();
    let lease = fresh_delivery(&db, &old, &recipients).await;
    db.addresses()
        .link(&recipients[0], Some(user.id))
        .await
        .unwrap();
    db.addresses().link(&recipients[0], None).await.unwrap();
    assert!(
        db.mail_queue()
            .begin_delivery_with_dsn(
                &lease,
                chrono::Utc::now().timestamp_millis(),
                &recipients,
                Some(&issuer)
            )
            .await
            .is_err(),
        "relink ABA must revoke old plan authority"
    );
}

#[tokio::test]
async fn dsn_issuance_rejects_pre_reset_plan() {
    let (_dir, issuer) = key_fixture();
    let recipients = vec!["Mixed@Example.invalid".into()];
    let (db, old) = control_fixture("sqlite::memory:", false, &recipients).await;
    let lease = fresh_delivery(&db, &old, &recipients).await;
    let member = db
        .members()
        .roster(
            &"test.example.invalid".parse().unwrap(),
            listmngr_core::MemberRole::Member,
        )
        .await
        .unwrap()
        .remove(0);
    db.preferences()
        .set_member(
            member.id,
            listmngr_core::Preferences {
                delivery_status: Some(listmngr_core::DeliveryStatus::ByBounces),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    db.preferences()
        .set_member(member.id, listmngr_core::Preferences::default())
        .await
        .unwrap();
    assert!(
        db.mail_queue()
            .begin_delivery_with_dsn(
                &lease,
                chrono::Utc::now().timestamp_millis(),
                &recipients,
                Some(&issuer)
            )
            .await
            .is_err(),
        "old plan must not acquire post-reset authority"
    );
}

#[tokio::test]
async fn dsn_issuance_rejects_replacement_before_handoff() {
    let (_dir, issuer) = key_fixture();
    let recipients = vec!["Mixed@Example.invalid".into()];
    let (db, old) = control_fixture("sqlite::memory:", false, &recipients).await;
    let selected = db
        .mail_queue()
        .plan_recipients(
            &"test.example.invalid".parse().unwrap(),
            "author@example.invalid",
            b"\r\n",
        )
        .await
        .unwrap();
    assert_eq!(selected.emails(), recipients);
    let member = db
        .members()
        .roster(
            &"test.example.invalid".parse().unwrap(),
            listmngr_core::MemberRole::Member,
        )
        .await
        .unwrap()
        .remove(0);
    db.members().delete(member.id).await.unwrap();
    let replacement = add_member(&db, &recipients[0]).await;
    assert_ne!(member.id, replacement.id);
    let lease = fresh_delivery_with_plan(&db, &old, &selected).await;
    assert!(
        db.mail_queue()
            .begin_delivery_with_dsn(
                &lease,
                chrono::Utc::now().timestamp_millis(),
                &selected.emails(),
                Some(&issuer)
            )
            .await
            .is_err(),
        "selected delivery must not adopt replacement before handoff"
    );
}

#[tokio::test]
async fn dsn_issuance_rejects_recreated_recipient_incarnation() {
    let (_dir, issuer) = key_fixture();
    let recipients = vec!["Mixed@Example.invalid".into()];
    let (db, old, _, _) = tests::fixture_at(
        "sqlite::memory:",
        "{\"list_id\":\"test.example.invalid\"}",
        b"Subject: old\r\n\r\nold",
        recipients.clone(),
    )
    .await;
    let member = add_member(&db, &recipients[0]).await;
    let lease = fresh_delivery(&db, &old, &recipients).await;
    db.members().delete(member.id).await.unwrap();
    add_member(&db, &recipients[0]).await;
    assert!(
        db.mail_queue()
            .begin_delivery_with_dsn(
                &lease,
                chrono::Utc::now().timestamp_millis(),
                &recipients,
                Some(&issuer)
            )
            .await
            .is_err(),
        "old delivery must not adopt replacement membership"
    );
}

#[tokio::test]
#[ignore = "requires owned TEST_POSTGRES_URL; never falls back to SQLite"]
async fn dsn_issuance_postgres() {
    let url = std::env::var("TEST_POSTGRES_URL").expect("owned PostgreSQL fixture required");
    assert!(url.starts_with("postgres://") || url.starts_with("postgresql://"));
    let admin = Database::connect(&url, 1).await.unwrap();
    let schema = format!("dsn_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(admin.pool())
        .await
        .unwrap();
    let sep = if url.contains('?') { '&' } else { '?' };
    let fixture = format!("{url}{sep}options=-csearch_path%3D{schema}");
    let result = tokio::spawn(async move {
        dsn_tracer(Some(fixture.clone())).await;
        dsn_controls(&fixture, true).await;
    })
    .await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(admin.pool())
        .await
        .unwrap();
    admin.pool().close().await;
    result.unwrap();
}
#[tokio::test]
async fn dsn_issuance_authority_retry_audit_and_expiry_controls() {
    dsn_controls("sqlite::memory:", false).await;
}
async fn control_fixture(url: &str, existing: bool, recipients: &[String]) -> (Database, Lease) {
    if existing {
        let db = Database::connect(url, 1).await.unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        db.mail_queue()
            .enqueue(
                listmngr_db::mail_queue::NewMessage {
                    raw: b"Subject: controls\r\n\r\ncontrol".to_vec(),
                    external_id: "control".into(),
                    context: "{\"list_id\":\"test.example.invalid\"}".into(),
                    queue: Queue::Out,
                    max_attempts: 5,
                },
                now,
            )
            .await
            .unwrap();
        let lease = db
            .mail_queue()
            .claim(Queue::Out, "control", now, 30_000)
            .await
            .unwrap()
            .unwrap();
        (db, lease)
    } else {
        let (db, lease, _, _) = tests::fixture_at(
            url,
            "{\"list_id\":\"test.example.invalid\"}",
            b"Subject: controls\r\n\r\ncontrol",
            recipients.to_vec(),
        )
        .await;
        add_member(&db, &recipients[0]).await;
        (db, lease)
    }
}

async fn audit_rollback(
    db: &Database,
    lease: &Lease,
    recipients: &[String],
    existing: bool,
    now: i64,
    issuer: &listmngr_core::dsn_issuance::Issuer,
) {
    // Audit sabotage must rollback both issuance and the pre-send reservation.
    if existing {
        sqlx::raw_sql("CREATE FUNCTION reject_dsn_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='dsn.issue' THEN RAISE EXCEPTION 'owned audit sabotage'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_dsn_audit BEFORE INSERT ON audit_log FOR EACH ROW EXECUTE FUNCTION reject_dsn_audit();").execute(db.pool()).await.unwrap();
    } else {
        sqlx::query("CREATE TRIGGER reject_dsn_audit BEFORE INSERT ON audit_log WHEN NEW.action='dsn.issue' BEGIN SELECT RAISE(ABORT,'owned audit sabotage'); END").execute(db.pool()).await.unwrap();
    }
    assert!(
        db.mail_queue()
            .begin_delivery_with_dsn(lease, now, recipients, Some(issuer))
            .await
            .is_err()
    );
    assert_eq!(
        db.mail_queue()
            .pending_recipients(lease.job.id)
            .await
            .unwrap(),
        recipients
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM dsn_issuances WHERE job_id=$1")
        .bind(lease.job.id.0.to_string())
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    sqlx::query(if existing {
        "DROP TRIGGER reject_dsn_audit ON audit_log"
    } else {
        "DROP TRIGGER reject_dsn_audit"
    })
    .execute(db.pool())
    .await
    .unwrap();
}

async fn dsn_controls(url: &str, existing: bool) {
    let (_dir, issuer) = key_fixture();
    let recipients = vec!["Mixed@Example.invalid".into()];
    let (db, old) = control_fixture(url, existing, &recipients).await;
    let lease = fresh_delivery(&db, &old, &recipients).await;
    let now = chrono::Utc::now().timestamp_millis();
    audit_rollback(&db, &lease, &recipients, existing, now, &issuer).await;
    let first = db
        .mail_queue()
        .begin_delivery_with_dsn(&lease, now, &recipients, Some(&issuer))
        .await
        .unwrap();
    let (claims, issued_at, expires): (String, i64, i64) =
        sqlx::query_as("SELECT claims,issued_at,expires_at FROM dsn_issuances WHERE job_id=$1")
            .bind(lease.job.id.0.to_string())
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(issuer.verify(&first[0], &claims, now, issued_at, expires));
    assert!(!issuer.verify(&first[0], &claims, expires, issued_at, expires));
    assert!(!issuer.verify(&first[0], &claims, issued_at - 1, issued_at, expires));
    let value: serde_json::Value = serde_json::from_str(&claims).unwrap();
    for field in [
        "job_id",
        "message_id",
        "list_id",
        "list_incarnation",
        "recipient",
        "canonical_recipient",
        "member_id",
        "address_id",
        "user_id",
        "preferences",
        "nonce",
        "version",
        "key_id",
        "issued_at",
        "expires_at",
        "attempt",
    ] {
        let mut tampered = value.clone();
        tampered[field] = serde_json::json!("different");
        assert!(
            !issuer.verify(&first[0], &tampered.to_string(), now, issued_at, expires),
            "{field}"
        );
    }
    // A known transient clears reservation, not immutable old issuance history.
    db.mail_queue()
        .finish_delivery(
            &lease,
            now,
            &[(
                recipients[0].clone(),
                RecipientOutcome::Transient,
                "known retry".into(),
            )],
            0,
        )
        .await
        .unwrap();
    let retry = db
        .mail_queue()
        .claim(Queue::Out, "retry", now, 30_000)
        .await
        .unwrap()
        .unwrap();
    let second = db
        .mail_queue()
        .begin_delivery_with_dsn(&retry, now, &recipients, Some(&issuer))
        .await
        .unwrap();
    assert_ne!(first, second);
    let stored: Vec<String> = sqlx::query_scalar("SELECT envid FROM dsn_issuances WHERE job_id=$1")
        .bind(lease.job.id.0.to_string())
        .fetch_all(db.pool())
        .await
        .unwrap();
    assert_eq!(stored.len(), 2);
    assert!(stored.contains(&first[0]));
    assert!(stored.contains(&second[0]));
    db.mail_queue()
        .finish_delivery(
            &retry,
            now,
            &[(
                recipients[0].clone(),
                RecipientOutcome::Sent,
                "accepted".into(),
            )],
            0,
        )
        .await
        .unwrap();
    db.pool().close().await;
}

#[tokio::test]
async fn dsn_issuance_legacy_and_recreated_list_fail_closed() {
    let (_dir, issuer) = key_fixture();
    let recipients = vec!["Mixed@Example.invalid".into()];
    let (db, old, _, _) = tests::fixture_at(
        "sqlite::memory:",
        "{\"list_id\":\"test.example.invalid\"}",
        b"Subject: old\r\n\r\nold",
        recipients.clone(),
    )
    .await;
    add_member(&db, &recipients[0]).await;
    // Original fixture was published without a member snapshot: no trusted backfill.
    assert!(
        db.mail_queue()
            .begin_delivery_with_dsn(
                &old,
                chrono::Utc::now().timestamp_millis(),
                &recipients,
                Some(&issuer)
            )
            .await
            .is_err()
    );
    let lease = fresh_delivery(&db, &old, &recipients).await;
    db.lists()
        .delete(&"test.example.invalid".parse().unwrap())
        .await
        .unwrap();
    db.lists()
        .create(listmngr_db::NewList {
            list_id: "test.example.invalid".parse().unwrap(),
            display_name: "replacement".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    add_member(&db, &recipients[0]).await;
    assert!(
        db.mail_queue()
            .begin_delivery_with_dsn(
                &lease,
                chrono::Utc::now().timestamp_millis(),
                &recipients,
                Some(&issuer)
            )
            .await
            .is_err()
    );
    assert_eq!(
        db.mail_queue()
            .pending_recipients(lease.job.id)
            .await
            .unwrap(),
        recipients
    );
}

#[tokio::test]
async fn dsn_issuance_missing_capability_retries_before_mail() {
    let (_dir, issuer) = key_fixture();
    let recipients = vec!["Mixed@Example.invalid".into()];
    let (db, old, mut role, sink) = tests::fixture_at(
        "sqlite::memory:",
        "{\"list_id\":\"test.example.invalid\"}",
        b"Subject: no capability\r\n\r\nbody",
        recipients.clone(),
    )
    .await;
    add_member(&db, &recipients[0]).await;
    let lease = fresh_delivery(&db, &old, &recipients).await;
    role.dsn_issuer = Some(issuer);
    role.smtp_single_recipient = true;
    let peer = async {
        let (stream, _) = sink.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut read = BufReader::new(read);
        write.write_all(b"220 fixture\r\n").await.unwrap();
        let mut line = String::new();
        read.read_line(&mut line).await.unwrap();
        assert!(line.starts_with("EHLO "));
        write.write_all(b"250-DSN\r\n250 XDSN\r\n").await.unwrap();
        line.clear();
        assert_eq!(read.read_line(&mut line).await.unwrap(), 0);
        assert!(line.is_empty());
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(peer, deliver_one(&db, &role, lease.clone()));
    })
    .await
    .unwrap();
    assert_eq!(
        db.mail_queue()
            .pending_recipients(lease.job.id)
            .await
            .unwrap(),
        recipients
    );
    let job = db.mail_queue().job(lease.job.id).await.unwrap();
    assert_eq!(job.state, listmngr_db::mail_queue::JobState::Ready);
    assert!(job.locked_by.is_none());
    let ledger: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM dsn_issuances")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(ledger, 1);
    let bounces: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM bounce_events")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(bounces, 0);
}

#[test]
fn dsn_issuance_secret_and_configuration_controls() {
    let (dir, issuer) = key_fixture();
    let path = dir.path().join("issuer.key");
    let mut config = issuer_config(&path);
    config.mta.smtp_single_recipient = false;
    assert!(MailRoleConfig::from_core(&config).is_err());
    config.mta.smtp_single_recipient = true;
    for id in ["", "bad.key", "line\r\nMAIL", "abcdefghijklmnopq"] {
        config.mta.dsn_key_id = id.into();
        assert!(MailRoleConfig::from_core(&config).is_err());
    }
    config.mta.dsn_key_id = "test".into();
    for ttl in [0, 59, 2_592_001] {
        config.mta.dsn_ttl_secs = ttl;
        assert!(MailRoleConfig::from_core(&config).is_err());
    }
    config.mta.dsn_ttl_secs = 60;
    for bytes in [vec![], vec![42; 31], vec![42; 33], vec![42; 100_000]] {
        std::fs::write(&path, bytes).unwrap();
        assert!(MailRoleConfig::from_core(&config).is_err());
    }
    std::fs::write(&path, [43_u8; 32]).unwrap();
    let wrong = MailRoleConfig::from_core(&config)
        .unwrap()
        .dsn_issuer
        .unwrap();
    let claims = "fixture";
    let nonce = "0123456789abcdef0123456789abcdef";
    let envid = issuer.issue(claims, nonce);
    assert!(!wrong.verify(&envid, claims, 1, 0, 2));
    assert!(issuer.verify(&envid, claims, 1, 0, 2));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(MailRoleConfig::from_core(&config).is_err());
    }
    config.mta.dsn_issuance_enabled = false;
    assert!(
        MailRoleConfig::from_core(&config)
            .unwrap()
            .dsn_issuer
            .is_none()
    );
}

#[derive(Debug)]
struct IssuanceClock {
    calls: std::sync::atomic::AtomicUsize,
    now: i64,
    expires: i64,
}
impl listmngr_db::mail_queue::LeaseClock for IssuanceClock {
    fn now_ms(&self) -> i64 {
        if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 3 {
            self.expires
        } else {
            self.now
        }
    }
}
#[tokio::test]
async fn dsn_issuance_final_audit_deadline_rolls_back() {
    let (_dir, issuer) = key_fixture();
    let recipients = vec!["Mixed@Example.invalid".into()];
    let (db, old, _, _) = tests::fixture_at(
        "sqlite::memory:",
        "{\"list_id\":\"test.example.invalid\"}",
        b"Subject: fence\r\n\r\nfence",
        recipients.clone(),
    )
    .await;
    add_member(&db, &recipients[0]).await;
    let lease = fresh_delivery(&db, &old, &recipients).await;
    let clock = IssuanceClock {
        calls: std::sync::atomic::AtomicUsize::new(0),
        now: chrono::Utc::now().timestamp_millis(),
        expires: lease.job.lease_until.unwrap(),
    };
    let result = db
        .mail_queue()
        .with_clock(&clock)
        .begin_delivery_with_dsn(&lease, clock.now, &recipients, Some(&issuer))
        .await;
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("after final write")
    );
    assert_eq!(
        db.mail_queue()
            .pending_recipients(lease.job.id)
            .await
            .unwrap(),
        recipients
    );
    for table in ["dsn_issuances", "audit_log"] {
        let sql = if table == "audit_log" {
            "SELECT COUNT(*) FROM audit_log WHERE action='dsn.issue'"
        } else {
            "SELECT COUNT(*) FROM dsn_issuances"
        };
        let count: i64 = sqlx::query_scalar(sql).fetch_one(db.pool()).await.unwrap();
        assert_eq!(count, 0);
    }
    assert!(
        db.mail_queue()
            .begin_delivery_with_dsn(&lease, clock.now, &recipients, Some(&issuer))
            .await
            .is_ok()
    );
}
