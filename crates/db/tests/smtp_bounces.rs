#[path = "smtp_bounces/disable_cases.rs"]
mod disable_cases;
#[path = "smtp_bounces/increment_cases.rs"]
mod increment_cases;
#[path = "smtp_bounces/notice_cases.rs"]
mod notice_cases;
#[path = "smtp_bounces/preference_races.rs"]
mod preference_races;
#[path = "smtp_bounces/score_cases.rs"]
mod score_cases;

use listmngr_db::{
    Database, NewList,
    mail_queue::{ChildJob, Lease, NewMessage, Queue, RecipientOutcome},
};

#[tokio::test]
async fn historical_schema_upgrade_keeps_unknown_metadata() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    let migrations = sqlx::migrate!("./migrations");
    for migration in migrations.iter().filter(|m| m.version <= 16) {
        sqlx::raw_sql(&migration.sql)
            .execute(db.pool())
            .await
            .unwrap();
    }
    // Seed the owning rows with the historical column set. Current repositories
    // insert columns introduced after migration 16, so they cannot run here.
    sqlx::query("INSERT INTO domains(id,mail_host,description,created_at) VALUES('domain','example.com','','1970-01-01T00:00:00+00:00')")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO mailing_lists(list_id,list_name,mail_host,display_name,subject_prefix,created_at) VALUES('test.example.com','test','example.com','Test','[Test] ','1970-01-01T00:00:00+00:00')")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO bounce_events VALUES('old','test.example.com','Mixed@Example.com','job','message',100,'smtp_permanent_failure','normal',0)")
        .execute(db.pool()).await.unwrap();
    // Retain the historical row, but finish upgrading before using current
    // repositories, whose list projection requires the current schema.
    for migration in migrations.iter().filter(|m| m.version > 16) {
        sqlx::raw_sql(&migration.sql)
            .execute(db.pool())
            .await
            .unwrap();
    }
    let events = db
        .bounces()
        .list(&"test.example.com".parse().unwrap(), 100, 0)
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert!(events[0].smtp_stage.is_none());
    assert!(events[0].smtp_code.is_none());
}

#[tokio::test]
async fn scoring_daily_order_and_stale_boundary() {
    for (last, expected) in [
        ("1969-12-31T00:00:00+00:00", 4.0),
        ("1970-01-01T00:00:00+00:00", 3.0),
        ("1970-01-01T00:00:00.200+00:00", 3.0),
        ("1970-01-02T00:00:00+00:00", 3.0),
        ("1969-12-25T00:00:00+00:00", 1.0),
        ("1969-12-25T00:00:00.104+00:00", 1.0),
        ("1969-12-25T00:00:00.105+00:00", 4.0),
    ] {
        let (db, lease) = fixture().await;
        db.lists()
            .update(
                &"test.example.com".parse().unwrap(),
                &serde_json::json!({"process_bounces":true}),
            )
            .await
            .unwrap();
        db.members()
            .create(listmngr_db::NewMember {
                list_id: "test.example.com".parse().unwrap(),
                email: "Mixed@Example.com".into(),
                display_name: String::new(),
                role: listmngr_core::MemberRole::Member,
                subscription_mode: listmngr_core::SubscriptionMode::AsAddress,
            })
            .await
            .unwrap();
        sqlx::query("UPDATE members SET bounce_score=3,last_bounce_received=$1")
            .bind(last)
            .execute(db.pool())
            .await
            .unwrap();
        db.mail_queue()
            .begin_delivery(&lease, 103, &["Mixed@Example.com".into()])
            .await
            .unwrap();
        db.mail_queue()
            .finish_delivery_with_smtp(
                &lease,
                104,
                &[(
                    "Mixed@Example.com".into(),
                    RecipientOutcome::Failed,
                    String::new(),
                )],
                0,
                &[(
                    "Mixed@Example.com".into(),
                    listmngr_core::SmtpFailure {
                        stage: listmngr_core::SmtpFailureStage::Rcpt,
                        code: 550,
                    },
                )],
            )
            .await
            .unwrap();
        let (score, received): (f64, String) =
            sqlx::query_as("SELECT bounce_score,last_bounce_received FROM members")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert!((score - expected).abs() < f64::EPSILON, "{last}");
        let prior = chrono::DateTime::parse_from_rfc3339(last).unwrap();
        if prior.timestamp_millis() > 104 {
            assert_eq!(received, last);
        } else {
            assert_eq!(
                chrono::DateTime::parse_from_rfc3339(&received)
                    .unwrap()
                    .timestamp_millis(),
                104,
                "same-day events must refresh last receipt without incrementing score"
            );
        }
    }
}

async fn fixture() -> (Database, Lease) {
    fixture_at("sqlite::memory:").await
}

async fn fixture_at(url: &str) -> (Database, Lease) {
    let db = Database::connect(url, 1).await.unwrap();
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
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: b"From: author@example.com\r\n\r\nbody".to_vec(),
                external_id: "external".into(),
                context: "{\"list_id\":\"test.example.com\"}".into(),
                queue: Queue::In,
                max_attempts: 4,
            },
            100,
        )
        .await
        .unwrap();
    let source = db
        .mail_queue()
        .claim(Queue::In, "in", 100, 1000)
        .await
        .unwrap()
        .unwrap();
    db.mail_queue()
        .complete_with_children(
            &source,
            101,
            &[ChildJob {
                queue: Queue::Out,
                max_attempts: 4,
                recipients: vec!["Mixed@Example.com".into(), "later@example.com".into()],
            }],
        )
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::Out, "out", 102, 1000)
        .await
        .unwrap()
        .unwrap();
    (db, lease)
}

#[tokio::test]
async fn sabotage_rolls_back_event_outcome_and_audit_then_valid_retry_commits() {
    for action in ["bounce.record", "queue.retry"] {
        let (db, lease) = fixture().await;
        db.mail_queue()
            .begin_delivery(&lease, 103, &["Mixed@Example.com".into()])
            .await
            .unwrap();
        let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
            .fetch_one(db.pool())
            .await
            .unwrap();
        sqlx::query(&format!("CREATE TRIGGER sabotage BEFORE INSERT ON audit_log WHEN NEW.action='{action}' BEGIN SELECT RAISE(ABORT, 'fixture'); END")).execute(db.pool()).await.unwrap();
        let outcomes = [(
            "Mixed@Example.com".into(),
            RecipientOutcome::Failed,
            "private".into(),
        )];
        assert!(
            db.mail_queue()
                .finish_delivery(&lease, 104, &outcomes, 0)
                .await
                .is_err()
        );
        assert_eq!(count(&db).await, 0);
        let status: String = sqlx::query_scalar(
            "SELECT status FROM delivery_recipients WHERE email='Mixed@Example.com'",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(status, "ambiguous");
        assert_eq!(
            db.mail_queue().job(lease.job.id).await.unwrap().state,
            listmngr_db::mail_queue::JobState::Leased
        );
        let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(before, after);
        sqlx::query("DROP TRIGGER sabotage")
            .execute(db.pool())
            .await
            .unwrap();
        db.mail_queue()
            .finish_delivery(&lease, 105, &outcomes, 0)
            .await
            .unwrap();
        assert_eq!(count(&db).await, 1);
        let events = db
            .bounces()
            .list(&"test.example.com".parse().unwrap(), 100, 0)
            .await
            .unwrap();
        assert!(events[0].smtp_stage.is_none());
        assert!(events[0].smtp_code.is_none());
    }
}

#[tokio::test]
async fn nonfailures_unreserved_and_internal_deliveries_do_not_become_normal_bounces() {
    for outcome in [
        RecipientOutcome::Sent,
        RecipientOutcome::Transient,
        RecipientOutcome::Ambiguous,
    ] {
        let (db, lease) = fixture().await;
        db.mail_queue()
            .begin_delivery(&lease, 103, &["Mixed@Example.com".into()])
            .await
            .unwrap();
        db.mail_queue()
            .finish_delivery(
                &lease,
                104,
                &[("Mixed@Example.com".into(), outcome, String::new())],
                0,
            )
            .await
            .unwrap();
        assert_eq!(count(&db).await, 0);
    }
    for kind in [
        "workflow_notices",
        "owner_deliveries",
        "digest_deliveries",
        "unreserved",
        "nonout",
    ] {
        let (db, lease) = fixture().await;
        if kind != "unreserved" {
            db.mail_queue()
                .begin_delivery(&lease, 103, &["Mixed@Example.com".into()])
                .await
                .unwrap();
        }
        match kind {
            "workflow_notices" | "owner_deliveries" => {
                sqlx::query(&format!("INSERT INTO {kind}(job_id) VALUES($1)"))
                    .bind(lease.job.id.0.to_string())
                    .execute(db.pool())
                    .await
                    .unwrap();
            }
            "digest_deliveries" => {
                sqlx::query("INSERT INTO digest_issues(id,list_id,volume,number,created_at) VALUES('issue','test.example.com',1,1,100)").execute(db.pool()).await.unwrap();
                sqlx::query("INSERT INTO digest_deliveries(job_id,issue_id,mode) VALUES($1,'issue','plaintext_digests')").bind(lease.job.id.0.to_string()).execute(db.pool()).await.unwrap();
            }
            "nonout" => {
                sqlx::query("UPDATE queue_jobs SET queue='virgin' WHERE id=$1")
                    .bind(lease.job.id.0.to_string())
                    .execute(db.pool())
                    .await
                    .unwrap();
            }
            _ => {}
        }
        db.mail_queue()
            .finish_delivery(
                &lease,
                104,
                &[(
                    "Mixed@Example.com".into(),
                    RecipientOutcome::Failed,
                    String::new(),
                )],
                0,
            )
            .await
            .unwrap();
        assert_eq!(count(&db).await, 0, "{kind}");
    }
}

#[tokio::test]
async fn reopen_retains_events_and_list_deletion_is_owned_and_atomic() {
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let directory = Cleanup(std::env::temp_dir().join(uuid::Uuid::now_v7().to_string()));
    std::fs::create_dir(&directory.0).unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        directory.0.join("bounce.db").display()
    );
    let (db, lease) = fixture_at(&url).await;
    db.mail_queue()
        .begin_delivery(&lease, 103, &["Mixed@Example.com".into()])
        .await
        .unwrap();
    db.mail_queue()
        .finish_delivery(
            &lease,
            104,
            &[(
                "Mixed@Example.com".into(),
                RecipientOutcome::Failed,
                String::new(),
            )],
            0,
        )
        .await
        .unwrap();
    db.pool().close().await;
    let db = Database::connect(&url, 1).await.unwrap();
    let list = "test.example.com".parse().unwrap();
    assert_eq!(db.bounces().count(&list).await.unwrap(), 1);
    let rows = db.bounces().list(&list, 1, 0).await.unwrap();
    assert_eq!(rows[0].recipient, "Mixed@Example.com");
    for (limit, offset) in [(0, 0), (101, 0), (1, -1)] {
        assert!(db.bounces().list(&list, limit, offset).await.is_err());
    }
    db.lists()
        .create(NewList {
            list_id: "other.example.com".parse().unwrap(),
            display_name: "Other".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    // Differential metadata ownership fixture; message/job IDs are retained values,
    // not restrictive spool FKs. Cross-list API tests use actual queue producers.
    sqlx::query("INSERT INTO bounce_events(id,list_id,recipient,job_id,message_id,created_at,source,context,processed) SELECT 'other','other.example.com',recipient,'other-job',message_id,created_at,source,context,processed FROM bounce_events").execute(db.pool()).await.unwrap();
    sqlx::query("CREATE TRIGGER reject_delete BEFORE INSERT ON audit_log WHEN NEW.action='list.delete' BEGIN SELECT RAISE(ABORT,'fixture'); END").execute(db.pool()).await.unwrap();
    assert!(db.lists().delete(&list).await.is_err());
    assert_eq!(count(&db).await, 2);
    sqlx::query("DROP TRIGGER reject_delete")
        .execute(db.pool())
        .await
        .unwrap();
    db.lists().delete(&list).await.unwrap();
    assert_eq!(count(&db).await, 1);
    assert!(matches!(
        db.bounces().count(&list).await,
        Err(listmngr_core::Error::NotFound(_))
    ));
    assert_eq!(
        db.bounces()
            .count(&"other.example.com".parse().unwrap())
            .await
            .unwrap(),
        1
    );
    db.pool().close().await;
}

#[derive(Debug)]
struct AuditExpiry(std::sync::atomic::AtomicUsize);
impl listmngr_db::mail_queue::LeaseClock for AuditExpiry {
    fn now_ms(&self) -> i64 {
        if self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 2 {
            104
        } else {
            1102
        }
    }
}

#[tokio::test]
async fn event_and_outcome_roll_back_when_clock_expires_after_audit() {
    let (db, lease) = fixture().await;
    db.mail_queue()
        .begin_delivery(&lease, 103, &["Mixed@Example.com".into()])
        .await
        .unwrap();
    let job_before = db.mail_queue().job(lease.job.id).await.unwrap();
    let token_before: Option<String> = sqlx::query_scalar(
        "SELECT attempt_token FROM delivery_recipients WHERE job_id=$1 AND email=$2",
    )
    .bind(lease.job.id.0.to_string())
    .bind("Mixed@Example.com")
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(token_before.is_some());
    let audit_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let clock = AuditExpiry(std::sync::atomic::AtomicUsize::new(0));
    let result = db
        .mail_queue()
        .with_clock(&clock)
        .finish_delivery(
            &lease,
            104,
            &[(
                "Mixed@Example.com".into(),
                RecipientOutcome::Failed,
                String::new(),
            )],
            0,
        )
        .await;
    assert!(matches!(result, Err(listmngr_core::Error::Conflict(_))));
    assert_eq!(count(&db).await, 0);
    assert_eq!(db.mail_queue().job(lease.job.id).await.unwrap(), job_before);
    let reservation: (String, Option<String>) = sqlx::query_as(
        "SELECT status,attempt_token FROM delivery_recipients WHERE job_id=$1 AND email=$2",
    )
    .bind(lease.job.id.0.to_string())
    .bind("Mixed@Example.com")
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(reservation, ("ambiguous".into(), token_before));
    let audit_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(audit_before, audit_after);
}

async fn count(db: &Database) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM bounce_events")
        .fetch_one(db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn permanent_recipient_event_is_atomic_unique_and_uses_stored_identity() {
    let (db, mut lease) = fixture().await;
    let message_id = lease.job.message_id;
    db.mail_queue()
        .begin_delivery(&lease, 103, &["Mixed@Example.com".into()])
        .await
        .unwrap();
    // Public lease metadata is not event authority.
    lease.job.message_id = listmngr_db::mail_queue::MessageId(uuid::Uuid::now_v7());
    lease.job.queue = Queue::Virgin;
    let outcomes = vec![
        (
            "Mixed@Example.com".into(),
            RecipientOutcome::Failed,
            "550 secret SMTP detail".into(),
        ),
        (
            "bogus@example.com".into(),
            RecipientOutcome::Failed,
            String::new(),
        ),
    ];
    db.mail_queue()
        .finish_delivery(&lease, 104, &outcomes, 0)
        .await
        .unwrap();
    let row: (String, String, String, String, String, String, i64, i64) = sqlx::query_as("SELECT list_id,recipient,job_id,message_id,source,context,created_at,processed FROM bounce_events").fetch_one(db.pool()).await.unwrap();
    assert_eq!(
        row,
        (
            "test.example.com".into(),
            "Mixed@Example.com".into(),
            lease.job.id.0.to_string(),
            message_id.0.to_string(),
            "smtp_permanent_failure".into(),
            "normal".into(),
            104,
            0
        )
    );
    assert!(
        db.mail_queue()
            .finish_delivery(&lease, 105, &outcomes, 0)
            .await
            .is_err()
    );
    let retry = db
        .mail_queue()
        .claim(Queue::Out, "retry", 105, 1000)
        .await
        .unwrap()
        .unwrap();
    db.mail_queue()
        .finish_delivery(&retry, 106, &outcomes, 0)
        .await
        .unwrap();
    assert_eq!(count(&db).await, 1);
}
