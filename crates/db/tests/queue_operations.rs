use listmngr_db::{
    AuditContext, Database,
    mail_queue::{ChildJob, JobId, JobState, NewMessage, Queue},
    queue_operations::{DeliveryResolution, ResolutionOutcome},
};

async fn quarantined(db: &Database, shunted: bool) -> JobId {
    db.migrate().await.unwrap();
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: b"fixture".to_vec(),
                external_id: "fixture".into(),
                context: "{}".into(),
                queue: Queue::In,
                max_attempts: 3,
            },
            100,
        )
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::In, "fixture", 100, 1000)
        .await
        .unwrap()
        .unwrap();
    db.mail_queue()
        .complete_with_children(
            &lease,
            101,
            &[ChildJob {
                queue: Queue::Out,
                max_attempts: 3,
                recipients: vec!["unknown@example.invalid".into()],
            }],
        )
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::Out, "fixture", 102, 1000)
        .await
        .unwrap()
        .unwrap();
    db.mail_queue()
        .begin_delivery(&lease, 103, &["unknown@example.invalid".into()])
        .await
        .unwrap();
    if shunted {
        db.mail_queue()
            .shunt(&lease, 104, "operator quarantine")
            .await
            .unwrap();
    } else {
        db.mail_queue()
            .finish_delivery(&lease, 104, &[], 0)
            .await
            .unwrap();
    }
    lease.job.id
}

const fn resolution(job: JobId, outcome: ResolutionOutcome) -> DeliveryResolution<'static> {
    DeliveryResolution {
        job,
        email: "unknown@example.invalid",
        outcome,
        reason: "operator reviewed relay log",
        acknowledge_duplicate_risk: true,
    }
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; unique disposable schema only"]
async fn postgres_isolated_resolution_is_atomic_and_single_winner() {
    let url = std::env::var("TEST_POSTGRES_URL").expect("TEST_POSTGRES_URL required");
    assert!(url.starts_with("postgres://") || url.starts_with("postgresql://"));
    assert!(!url.contains("options="));
    let admin = Database::connect(&url, 1).await.unwrap();
    let schema = format!("resolution_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(admin.pool())
        .await
        .unwrap();
    let separator = if url.contains('?') { '&' } else { '?' };
    let isolated = format!("{url}{separator}options=-csearch_path%3D{schema}");
    let expected = schema.clone();
    let result = tokio::spawn(async move {
        let db = Database::connect(&isolated, 4).await.unwrap();
        let current: String = sqlx::query_scalar("SELECT current_schema()::text").fetch_one(db.pool()).await.unwrap();
        assert_eq!(current, expected);
        let job = quarantined(&db, false).await;
        sqlx::query("CREATE FUNCTION fail_resolution() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='queue.resolve' THEN RAISE EXCEPTION 'fixture audit rejection'; END IF; RETURN NEW; END $$")
            .execute(db.pool()).await.unwrap();
        sqlx::query("CREATE TRIGGER fail_resolution BEFORE INSERT ON audit_log FOR EACH ROW EXECUTE FUNCTION fail_resolution()")
            .execute(db.pool()).await.unwrap();
        let actor = AuditContext::system();
        assert!(db.resolve_delivery(resolution(job, ResolutionOutcome::Retry), &actor, 110).await.is_err());
        assert_eq!(db.mail_queue().job(job).await.unwrap().state, JobState::Done);
        assert!(db.mail_queue().pending_recipients(job).await.unwrap().is_empty());
        sqlx::query("DROP TRIGGER fail_resolution ON audit_log").execute(db.pool()).await.unwrap();
        let (first, second) = tokio::join!(
            db.resolve_delivery(resolution(job, ResolutionOutcome::Sent), &actor, 110),
            db.resolve_delivery(resolution(job, ResolutionOutcome::Failed), &actor, 110),
        );
        assert_ne!(first.is_ok(), second.is_ok(), "exactly one resolution may commit");
        let audit_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='queue.resolve'")
            .fetch_one(db.pool()).await.unwrap();
        assert_eq!(audit_count, 1);
        db.pool().close().await;
    }).await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(admin.pool())
        .await
        .unwrap();
    admin.pool().close().await;
    result.unwrap();
}

#[tokio::test]
async fn explicit_resolution_of_shunted_attempt_restores_out_queue() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    let job = quarantined(&db, true).await;
    db.resolve_delivery(
        resolution(job, ResolutionOutcome::Retry),
        &AuditContext::system(),
        110,
    )
    .await
    .unwrap();
    let restored = db.mail_queue().job(job).await.unwrap();
    assert_eq!(restored.queue, Queue::Out);
    assert_eq!(restored.state, JobState::Ready);
    let lease = db
        .mail_queue()
        .claim(Queue::Out, "resumed", 110, 1000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lease.job.id, job);
    db.mail_queue()
        .begin_delivery(&lease, 111, &["unknown@example.invalid".into()])
        .await
        .unwrap();
}

#[tokio::test]
async fn resolution_audit_failure_rolls_back_status_attempt_token_and_job() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    let job = quarantined(&db, false).await;
    sqlx::query("CREATE TRIGGER reject_resolution BEFORE INSERT ON audit_log WHEN NEW.action='queue.resolve' BEGIN SELECT RAISE(ABORT, 'fixture audit failure'); END").execute(db.pool()).await.unwrap();
    assert!(
        db.resolve_delivery(
            resolution(job, ResolutionOutcome::Retry),
            &AuditContext::system(),
            110
        )
        .await
        .is_err()
    );
    assert_eq!(
        db.mail_queue().job(job).await.unwrap().state,
        JobState::Done
    );
    let state: (String, String) = sqlx::query_as(
        "SELECT status,COALESCE(attempt_token,'') FROM delivery_recipients WHERE job_id=$1",
    )
    .bind(job.0.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(state.0, "ambiguous");
    assert!(!state.1.is_empty());
    assert!(
        db.mail_queue()
            .pending_recipients(job)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn confirmed_terminal_outcomes_are_audited_without_requeue() {
    for (outcome, expected) in [
        (ResolutionOutcome::Sent, "sent"),
        (ResolutionOutcome::Failed, "failed"),
    ] {
        let db = Database::connect("sqlite::memory:", 1).await.unwrap();
        let job = quarantined(&db, false).await;
        db.resolve_delivery(resolution(job, outcome), &AuditContext::system(), 110)
            .await
            .unwrap();
        assert_eq!(
            db.mail_queue().job(job).await.unwrap().state,
            JobState::Done
        );
        let status: String =
            sqlx::query_scalar("SELECT status FROM delivery_recipients WHERE job_id=$1")
                .bind(job.0.to_string())
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(status, expected);
        assert!(
            db.mail_queue()
                .claim(Queue::Out, "worker", 120, 1000)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            db.resolve_delivery(resolution(job, outcome), &AuditContext::system(), 120)
                .await
                .is_err()
        );
    }
}
