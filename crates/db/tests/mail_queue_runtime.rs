//! Runtime prerequisites for a real mail path: heartbeat, unshunt, and atomic
//! downstream handoff. Complements `mail_queue.rs` and `mail_queue_security.rs`.
use listmngr_db::Database;
use listmngr_db::mail_queue::{ChildJob, JobState, NewMessage, Queue, RecipientOutcome};

/// Claim `job_id` from the `out` queue and immediately `finish_delivery` with
/// one outcome, returning the resulting job. A small helper shared by the
/// recipient-outcome tests below, each of which needs a fresh lease per call.
async fn claim_and_finish(
    db: &Database,
    job_id: listmngr_db::mail_queue::JobId,
    now_ms: i64,
    outcome: (&str, RecipientOutcome, &str),
) -> listmngr_db::mail_queue::QueueJob {
    let lease = db
        .mail_queue()
        .claim(Queue::Out, "outworker", now_ms, 10_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lease.job.id, job_id);
    db.mail_queue()
        .finish_delivery(
            &lease,
            now_ms + 1,
            &[(outcome.0.to_owned(), outcome.1, outcome.2.to_owned())],
            0,
        )
        .await
        .unwrap()
}

fn input(raw: &[u8], context: &str) -> NewMessage {
    NewMessage {
        raw: raw.to_vec(),
        external_id: "<same@example.org>".into(),
        context: context.into(),
        queue: Queue::In,
        max_attempts: 3,
    }
}

#[tokio::test]
async fn transient_delivery_waits_for_sqlite_writer_before_reading_snapshot() {
    struct TempDb(std::path::PathBuf);
    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let dir = TempDb(std::env::temp_dir().join(uuid::Uuid::now_v7().to_string()));
    std::fs::create_dir(&dir.0).unwrap();
    let db = Database::connect(
        &format!("sqlite://{}/queue.sqlite?mode=rwc", dir.0.display()),
        3,
    )
    .await
    .unwrap();
    db.migrate().await.unwrap();
    db.mail_queue()
        .enqueue(input(b"writer contention", "route"), 100)
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::In, "worker", 100, 10_000)
        .await
        .unwrap()
        .unwrap();
    db.mail_queue()
        .complete_with_children(
            &lease,
            100,
            &[ChildJob {
                queue: Queue::Out,
                recipients: vec!["retry@example.invalid".into()],
                max_attempts: 3,
            }],
        )
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::Out, "worker", 100, 10_000)
        .await
        .unwrap()
        .unwrap();
    // A competing claim/heartbeat holds the SQLite writer reservation. A
    // deferred read-then-write transaction cannot upgrade its read lock here.
    let blocker = db.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
    let queue = db.mail_queue();
    let finish = queue.finish_delivery(&lease, 101, &[], 10);
    tokio::pin!(finish);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut finish)
            .await
            .is_err(),
        "finishing must wait for the writer, not fail a snapshot lock upgrade"
    );
    blocker.commit().await.unwrap();
    let job = finish.await.unwrap();
    assert_eq!(job.state, JobState::Ready);
    assert_eq!(job.run_after, 111);
    assert_eq!(
        db.mail_queue().pending_recipients(job.id).await.unwrap(),
        vec!["retry@example.invalid"]
    );
    db.pool().close().await;
}

#[tokio::test]
async fn heartbeat_extends_a_valid_fenced_lease_and_rejects_stale_ones() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let job = db
        .mail_queue()
        .enqueue(input(b"heartbeat", "route"), 100)
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::In, "worker", 100, 10)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lease.job.lease_until, Some(110));
    // Extend well before expiry.
    let extended = db.mail_queue().heartbeat(&lease, 105, 30).await.unwrap();
    assert_eq!(extended.job.lease_until, Some(135));
    assert_eq!(extended.job.id, job.id);
    // The old, now-superseded lease value must not extend anything further via a stale token.
    assert!(db.mail_queue().heartbeat(&lease, 106, 30).await.is_ok());
    // A second worker cannot claim while the (heartbeat-extended) lease is alive.
    assert!(
        db.mail_queue()
            .claim(Queue::In, "intruder", 120, 10)
            .await
            .unwrap()
            .is_none()
    );
    // After true expiry, heartbeat on the old lease must fail (fenced), and the job is claimable.
    let expired_lease = extended;
    assert!(
        db.mail_queue()
            .heartbeat(&expired_lease, 400, 10)
            .await
            .is_err()
    );
    let recovered = db
        .mail_queue()
        .claim(Queue::In, "recovery", 400, 10)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.job.id, job.id);
    assert!(
        db.mail_queue()
            .heartbeat(&expired_lease, 401, 10)
            .await
            .is_err()
    );
    let heartbeats: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log WHERE action='queue.heartbeat' AND target_id=$1",
    )
    .bind(job.id.0.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(heartbeats, 2);
}

#[tokio::test]
async fn heartbeat_deadline_is_monotonic_and_never_shrinks() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.mail_queue()
        .enqueue(input(b"monotonic", "route"), 100)
        .await
        .unwrap();
    // claim now=100 TTL=100 => expiry=200
    let lease = db
        .mail_queue()
        .claim(Queue::In, "worker", 100, 100)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lease.job.lease_until, Some(200));
    // heartbeat now=101 TTL=1 proposes expiry=102, far short of the current 200:
    // a naive overwrite would shrink the deadline and let another worker
    // reclaim the job while this worker still believes it holds the lease.
    let shrunk_proposal = db.mail_queue().heartbeat(&lease, 101, 1).await.unwrap();
    assert_eq!(
        shrunk_proposal.job.lease_until,
        Some(200),
        "heartbeat must retain the higher existing deadline, not the shorter proposal"
    );
    // Out-of-order renewal: an earlier-queued heartbeat with a short TTL lands
    // after a later one with a long TTL already extended the deadline further.
    let extended = db.mail_queue().heartbeat(&lease, 150, 100).await.unwrap();
    assert_eq!(extended.job.lease_until, Some(250));
    let late_short_renewal = db.mail_queue().heartbeat(&lease, 160, 5).await.unwrap();
    assert_eq!(
        late_short_renewal.job.lease_until,
        Some(250),
        "a later, shorter-TTL heartbeat must not lower an already-extended deadline"
    );
    // No reclaim is possible anywhere before the true maximum expiry (250),
    // including at the moments the shrunk proposals would have expired (102, 165).
    for now in [102, 165, 200, 249] {
        assert!(
            db.mail_queue()
                .claim(Queue::In, "intruder", now, 10)
                .await
                .unwrap()
                .is_none(),
            "job must not be reclaimable before the true maximum expiry at now={now}"
        );
    }
    let recovered = db
        .mail_queue()
        .claim(Queue::In, "recovery", 250, 10)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.job.id, lease.job.id);
}

#[tokio::test]
async fn heartbeat_rejects_invalid_lease_budgets() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.mail_queue()
        .enqueue(input(b"budget", "route"), 100)
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::In, "worker", 100, 10)
        .await
        .unwrap()
        .unwrap();
    assert!(db.mail_queue().heartbeat(&lease, 101, 0).await.is_err());
    assert!(
        db.mail_queue()
            .heartbeat(&lease, i64::MAX, 1)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn unshunt_replays_a_shunted_job_with_a_fresh_budget_and_validates_target() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let mut msg = input(b"poison", "route");
    msg.max_attempts = 1;
    let job = db.mail_queue().enqueue(msg, 100).await.unwrap();
    db.mail_queue()
        .claim(Queue::In, "crashed", 100, 10)
        .await
        .unwrap()
        .unwrap();
    // Expired single-attempt lease is auto-shunted on the next claim attempt.
    assert!(
        db.mail_queue()
            .claim(Queue::In, "recovery", 200, 10)
            .await
            .unwrap()
            .is_none()
    );
    let shunted = db.mail_queue().job(job.id).await.unwrap();
    assert_eq!(shunted.state, JobState::Shunted);
    assert_eq!(shunted.queue, Queue::Shunt);

    // Reject nonsensical targets before mutating anything.
    assert!(
        db.mail_queue()
            .unshunt(job.id, 300, Queue::Shunt)
            .await
            .is_err()
    );
    assert!(
        db.mail_queue()
            .unshunt(job.id, 300, Queue::Bad)
            .await
            .is_err()
    );
    assert_eq!(db.mail_queue().job(job.id).await.unwrap(), shunted);

    let replayed = db
        .mail_queue()
        .unshunt(job.id, 300, Queue::In)
        .await
        .unwrap();
    assert_eq!(replayed.queue, Queue::In);
    assert_eq!(replayed.state, JobState::Ready);
    assert_eq!(replayed.attempts, 0);
    assert!(replayed.last_error.is_empty());

    let lease = db
        .mail_queue()
        .claim(Queue::In, "second-chance", 300, 10)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lease.job.id, job.id);
    db.mail_queue().ack(&lease, 301).await.unwrap();

    // A job that is not currently shunted cannot be unshunted again.
    assert!(
        db.mail_queue()
            .unshunt(job.id, 400, Queue::In)
            .await
            .is_err()
    );

    let actions: Vec<String> =
        sqlx::query_scalar("SELECT action FROM audit_log WHERE target_id=$1 ORDER BY at,id")
            .bind(job.id.0.to_string())
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(
        actions,
        [
            "queue.enqueue",
            "queue.claim",
            "queue.shunt",
            "queue.unshunt",
            "queue.claim",
            "queue.ack"
        ]
    );
}

#[tokio::test]
async fn complete_with_children_hands_off_atomically_and_fences_double_finish() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let job = db
        .mail_queue()
        .enqueue(input(b"handoff", "route"), 100)
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::In, "worker", 100, 10)
        .await
        .unwrap()
        .unwrap();
    let (source, children) = db
        .mail_queue()
        .complete_with_children(
            &lease,
            105,
            &[
                ChildJob {
                    queue: Queue::Out,
                    max_attempts: 5,
                    recipients: Vec::new(),
                },
                ChildJob {
                    queue: Queue::Archive,
                    max_attempts: 5,
                    recipients: Vec::new(),
                },
            ],
        )
        .await
        .unwrap();
    assert_eq!(source.state, JobState::Done);
    assert_eq!(children.len(), 2);
    assert_eq!(children[0].queue, Queue::Out);
    assert_eq!(children[1].queue, Queue::Archive);
    for child in &children {
        assert_eq!(child.message_id, job.message_id);
        assert_eq!(child.state, JobState::Ready);
    }

    // A stale/duplicate finish (crashed worker retrying the ack) must produce no new effects.
    assert!(
        db.mail_queue()
            .complete_with_children(
                &lease,
                106,
                &[ChildJob {
                    queue: Queue::Out,
                    max_attempts: 5,
                    recipients: Vec::new()
                }]
            )
            .await
            .is_err()
    );
    let out_jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs WHERE queue='out'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(out_jobs, 1, "double finish must not create extra jobs");

    let out_lease = db
        .mail_queue()
        .claim(Queue::Out, "out-worker", 110, 10)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(out_lease.job.message_id, job.message_id);
    assert_eq!(
        db.mail_queue().message(job.message_id).await.unwrap().raw,
        b"handoff"
    );
}

#[tokio::test]
async fn complete_with_children_rolls_back_children_when_audit_fails() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.mail_queue()
        .enqueue(input(b"rollback", "route"), 100)
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::In, "worker", 100, 10)
        .await
        .unwrap()
        .unwrap();
    sqlx::query("CREATE TRIGGER fail_child_audit BEFORE INSERT ON audit_log WHEN NEW.action='queue.enqueue' BEGIN SELECT RAISE(ABORT, 'audit sabotage'); END")
        .execute(db.pool()).await.unwrap();
    assert!(
        db.mail_queue()
            .complete_with_children(
                &lease,
                101,
                &[ChildJob {
                    queue: Queue::Out,
                    max_attempts: 5,
                    recipients: Vec::new()
                }]
            )
            .await
            .is_err()
    );
    sqlx::query("DROP TRIGGER fail_child_audit")
        .execute(db.pool())
        .await
        .unwrap();
    // Source job must still be leased (not falsely acked) since the whole transaction rolled back.
    assert_eq!(db.mail_queue().job(lease.job.id).await.unwrap(), lease.job);
    let out_jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs WHERE queue='out'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(out_jobs, 0);
}

#[tokio::test]
async fn recipient_snapshot_persists_and_never_resends_an_already_sent_recipient() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.mail_queue()
        .enqueue(input(b"fan out", "route"), 100)
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::In, "worker", 100, 10)
        .await
        .unwrap()
        .unwrap();
    let (_, children) = db
        .mail_queue()
        .complete_with_children(
            &lease,
            101,
            &[ChildJob {
                queue: Queue::Out,
                max_attempts: 5,
                recipients: vec!["a@example.invalid".into(), "b@example.invalid".into()],
            }],
        )
        .await
        .unwrap();
    let out_job = children[0].id;
    let mut pending = db.mail_queue().pending_recipients(out_job).await.unwrap();
    pending.sort();
    assert_eq!(pending, vec!["a@example.invalid", "b@example.invalid"]);

    claim_and_finish(
        &db,
        out_job,
        102,
        (
            "a@example.invalid",
            RecipientOutcome::Sent,
            "2.1.5 delivered",
        ),
    )
    .await;
    let pending = db.mail_queue().pending_recipients(out_job).await.unwrap();
    assert_eq!(pending, vec!["b@example.invalid"]);

    // Marking the already-sent recipient again (via a fresh lease, since the
    // job cycled back to `ready` for the still-pending "b") must not
    // resurrect it as pending.
    claim_and_finish(
        &db,
        out_job,
        104,
        (
            "a@example.invalid",
            RecipientOutcome::Failed,
            "should not apply",
        ),
    )
    .await;
    let status: String =
        sqlx::query_scalar("SELECT status FROM delivery_recipients WHERE job_id=$1 AND email=$2")
            .bind(out_job.0.to_string())
            .bind("a@example.invalid")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(status, "sent");

    claim_and_finish(
        &db,
        out_job,
        106,
        (
            "b@example.invalid",
            RecipientOutcome::Failed,
            "451 try later",
        ),
    )
    .await;
    assert!(
        db.mail_queue()
            .pending_recipients(out_job)
            .await
            .unwrap()
            .is_empty()
    );
    let failed: String =
        sqlx::query_scalar("SELECT detail FROM delivery_recipients WHERE job_id=$1 AND email=$2")
            .bind(out_job.0.to_string())
            .bind("b@example.invalid")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(failed, "451 try later");
}

#[tokio::test]
async fn stale_worker_must_not_overwrite_a_newer_workers_recipient_outcome() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.mail_queue()
        .enqueue(input(b"fan out", "route"), 100)
        .await
        .unwrap();
    let in_lease = db
        .mail_queue()
        .claim(Queue::In, "worker", 100, 10)
        .await
        .unwrap()
        .unwrap();
    let (_, children) = db
        .mail_queue()
        .complete_with_children(
            &in_lease,
            101,
            &[ChildJob {
                queue: Queue::Out,
                max_attempts: 5,
                recipients: vec!["a@example.invalid".into()],
            }],
        )
        .await
        .unwrap();
    let out_job = children[0].id;

    // w1 claims with a short lease that has already expired by the time w2 reclaims it.
    let stale = db
        .mail_queue()
        .claim(Queue::Out, "w1", 101, 50)
        .await
        .unwrap()
        .unwrap();
    let current = db
        .mail_queue()
        .claim(Queue::Out, "w2", 200, 10_000)
        .await
        .unwrap()
        .unwrap();

    // w2 is the legitimate current owner and correctly delivered the message.
    db.mail_queue()
        .finish_delivery(
            &current,
            201,
            &[(
                "a@example.invalid".to_owned(),
                RecipientOutcome::Sent,
                "2.1.5 delivered by w2".to_owned(),
            )],
            0,
        )
        .await
        .unwrap();

    // w1, unaware its lease is dead, must not be able to overwrite w2's result.
    let stale_result = db
        .mail_queue()
        .finish_delivery(
            &stale,
            202,
            &[(
                "a@example.invalid".to_owned(),
                RecipientOutcome::Failed,
                "stale w1 thinks it failed".to_owned(),
            )],
            0,
        )
        .await;
    assert!(
        stale_result.is_err(),
        "a stale/expired lease must not be able to record a recipient outcome"
    );

    let status: String =
        sqlx::query_scalar("SELECT status FROM delivery_recipients WHERE job_id=$1 AND email=$2")
            .bind(out_job.0.to_string())
            .bind("a@example.invalid")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        status, "sent",
        "the legitimate current owner's outcome must survive a stale worker's write attempt"
    );
}

#[tokio::test]
async fn ambiguous_recipients_are_quarantined_not_auto_retried_and_do_not_discard_transient_peers()
{
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.mail_queue()
        .enqueue(input(b"fan out", "route"), 100)
        .await
        .unwrap();
    let in_lease = db
        .mail_queue()
        .claim(Queue::In, "worker", 100, 10)
        .await
        .unwrap()
        .unwrap();
    let (_, children) = db
        .mail_queue()
        .complete_with_children(
            &in_lease,
            101,
            &[ChildJob {
                queue: Queue::Out,
                max_attempts: 5,
                recipients: vec!["a@example.invalid".into(), "b@example.invalid".into()],
            }],
        )
        .await
        .unwrap();
    let out_job = children[0].id;

    // First attempt: "a" is ambiguous (connection lost after the data write),
    // "b" is left out of the outcome list entirely (an ordinary transient
    // failure that must simply stay pending for automatic retry).
    let lease1 = db
        .mail_queue()
        .claim(Queue::Out, "w1", 102, 10_000)
        .await
        .unwrap()
        .unwrap();
    let job = db
        .mail_queue()
        .finish_delivery(
            &lease1,
            103,
            &[(
                "a@example.invalid".to_owned(),
                RecipientOutcome::Ambiguous,
                "connection lost after data write".to_owned(),
            )],
            0,
        )
        .await
        .unwrap();

    // The job must still be retried (b is genuinely pending), not acked.
    assert_eq!(job.state, JobState::Ready);
    let pending = db.mail_queue().pending_recipients(out_job).await.unwrap();
    assert_eq!(
        pending,
        vec!["b@example.invalid"],
        "an ambiguous recipient must never be reported as pending (no automatic replay), \
         and must not cause a genuinely transient peer to be discarded"
    );
    let (status, detail): (String, String) = sqlx::query_as(
        "SELECT status,detail FROM delivery_recipients WHERE job_id=$1 AND email=$2",
    )
    .bind(out_job.0.to_string())
    .bind("a@example.invalid")
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(status, "ambiguous");
    assert_eq!(detail, "connection lost after data write");

    // Second attempt resolves "b" normally; the job completes even though
    // "a" remains quarantined (no automatic resolution is invented for it).
    let lease2 = db
        .mail_queue()
        .claim(Queue::Out, "w1", 104, 10_000)
        .await
        .unwrap()
        .unwrap();
    let job = db
        .mail_queue()
        .finish_delivery(
            &lease2,
            105,
            &[(
                "b@example.invalid".to_owned(),
                RecipientOutcome::Sent,
                "2.1.5 delivered".to_owned(),
            )],
            0,
        )
        .await
        .unwrap();
    assert_eq!(job.state, JobState::Done);
    let status: String =
        sqlx::query_scalar("SELECT status FROM delivery_recipients WHERE job_id=$1 AND email=$2")
            .bind(out_job.0.to_string())
            .bind("a@example.invalid")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        status, "ambiguous",
        "an ambiguous recipient is never auto-resolved by a later, unrelated attempt"
    );
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; creates and drops only a unique test schema"]
async fn postgres_isolated_heartbeat_deadline_is_monotonic() {
    let url = std::env::var("TEST_POSTGRES_URL").expect("TEST_POSTGRES_URL is required");
    assert!(url.starts_with("postgres://") || url.starts_with("postgresql://"));
    assert!(
        !url.contains("options="),
        "fixture requires URL without pre-existing startup options"
    );
    let admin = Database::connect(&url, 1).await.unwrap();
    let schema = format!("heartbeat_test_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(admin.pool())
        .await
        .unwrap();
    let separator = if url.contains('?') { '&' } else { '?' };
    let fixture_url = format!("{url}{separator}options=-csearch_path%3D{schema}");
    let expected_schema = schema.clone();
    // Catch scenario panics via JoinHandle so our schema is cleaned even on failure.
    let result = tokio::spawn(async move {
        let db = Database::connect(&fixture_url, 4).await.unwrap();
        let current: String = sqlx::query_scalar("SELECT current_schema()::text")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(
            current, expected_schema,
            "never migrate outside the isolated fixture"
        );
        db.migrate().await.unwrap();
        db.mail_queue()
            .enqueue(input(b"pg monotonic", "route"), 100)
            .await
            .unwrap();
        let lease = db
            .mail_queue()
            .claim(Queue::In, "worker", 100, 100)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(lease.job.lease_until, Some(200));
        // The CASE-clamped UPDATE (portable SQL, no PostgreSQL-only GREATEST or
        // SQLite-only multi-arg max) must behave identically on PostgreSQL:
        // a shorter-TTL heartbeat must never shrink the deadline.
        let shrunk = db.mail_queue().heartbeat(&lease, 101, 1).await.unwrap();
        assert_eq!(shrunk.job.lease_until, Some(200));
        let extended = db.mail_queue().heartbeat(&lease, 150, 100).await.unwrap();
        assert_eq!(extended.job.lease_until, Some(250));
        assert!(
            db.mail_queue()
                .claim(Queue::In, "intruder", 200, 10)
                .await
                .unwrap()
                .is_none(),
            "job must not be reclaimable before the true maximum expiry"
        );
        let recovered = db
            .mail_queue()
            .claim(Queue::In, "recovery", 250, 10)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(recovered.job.id, lease.job.id);
        db.pool().close().await;
    })
    .await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(admin.pool())
        .await
        .unwrap();
    admin.pool().close().await;
    result.unwrap();
}

async fn assert_fenced_ambiguous_delivery(db: &Database) {
    db.mail_queue()
        .enqueue(input(b"pg finish_delivery", "route"), 100)
        .await
        .unwrap();
    let in_lease = db
        .mail_queue()
        .claim(Queue::In, "worker", 100, 10)
        .await
        .unwrap()
        .unwrap();
    let (_, children) = db
        .mail_queue()
        .complete_with_children(
            &in_lease,
            101,
            &[ChildJob {
                queue: Queue::Out,
                max_attempts: 5,
                recipients: vec!["a@example.invalid".into(), "b@example.invalid".into()],
            }],
        )
        .await
        .unwrap();
    let out_job = children[0].id;
    let stale = db
        .mail_queue()
        .claim(Queue::Out, "w1", 101, 50)
        .await
        .unwrap()
        .unwrap();
    let current = db
        .mail_queue()
        .claim(Queue::Out, "w2", 200, 10_000)
        .await
        .unwrap()
        .unwrap();
    assert!(
        db.mail_queue()
            .finish_delivery(
                &stale,
                201,
                &[(
                    "a@example.invalid".to_owned(),
                    RecipientOutcome::Sent,
                    "stale write must not apply".to_owned()
                )],
                0
            )
            .await
            .is_err(),
        "a stale/expired lease must be rejected"
    );
    let untouched: String =
        sqlx::query_scalar("SELECT status FROM delivery_recipients WHERE job_id=$1 AND email=$2")
            .bind(out_job.0.to_string())
            .bind("a@example.invalid")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        untouched, "pending",
        "the stale write must have rolled back entirely"
    );
    let job = db
        .mail_queue()
        .finish_delivery(
            &current,
            202,
            &[(
                "a@example.invalid".to_owned(),
                RecipientOutcome::Ambiguous,
                "connection lost after data write".to_owned(),
            )],
            0,
        )
        .await
        .unwrap();
    assert_eq!(job.state, JobState::Ready);
    assert_eq!(
        db.mail_queue().pending_recipients(out_job).await.unwrap(),
        vec!["b@example.invalid".to_owned()]
    );
    let status: String =
        sqlx::query_scalar("SELECT status FROM delivery_recipients WHERE job_id=$1 AND email=$2")
            .bind(out_job.0.to_string())
            .bind("a@example.invalid")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(status, "ambiguous");
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; creates and drops only a unique test schema"]
async fn postgres_isolated_finish_delivery_is_fenced_and_quarantines_ambiguous() {
    let url = std::env::var("TEST_POSTGRES_URL").expect("TEST_POSTGRES_URL is required");
    assert!(url.starts_with("postgres://") || url.starts_with("postgresql://"));
    assert!(
        !url.contains("options="),
        "fixture requires URL without pre-existing startup options"
    );
    let admin = Database::connect(&url, 1).await.unwrap();
    let schema = format!("finish_delivery_test_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(admin.pool())
        .await
        .unwrap();
    let separator = if url.contains('?') { '&' } else { '?' };
    let fixture_url = format!("{url}{separator}options=-csearch_path%3D{schema}");
    let result = tokio::spawn(async move {
        let db = Database::connect(&fixture_url, 4).await.unwrap();
        db.migrate().await.unwrap();
        assert_fenced_ambiguous_delivery(&db).await;
        db.pool().close().await;
    })
    .await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(admin.pool())
        .await
        .unwrap();
    admin.pool().close().await;
    result.unwrap();
}
