use listmngr_db::Database;
use listmngr_db::mail_queue::{JobState, NewMessage, Queue};
use listmngr_db::moderation::Disposition;

fn input(raw: &[u8]) -> NewMessage {
    NewMessage {
        raw: raw.to_vec(),
        external_id: "<held@example.invalid>".into(),
        context: "route".into(),
        queue: Queue::In,
        max_attempts: 3,
    }
}

async fn seeded_list(db: &Database) -> listmngr_core::ListId {
    db.domains()
        .create("dev.example.invalid", "dev", None)
        .await
        .unwrap();
    let list_id: listmngr_core::ListId = "dev.dev.example.invalid".parse().unwrap();
    db.lists()
        .create(listmngr_db::NewList {
            list_id: list_id.clone(),
            display_name: "Dev".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    // Hold notices are covered by their own tests.
    db.lists()
        .update(
            &list_id,
            &serde_json::json!({"respond_to_post_requests": false, "admin_immed_notify": false}),
        )
        .await
        .unwrap();
    list_id
}

#[tokio::test]
async fn hold_atomically_finishes_the_in_job_and_fences_a_stale_lease() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let list_id = seeded_list(&db).await;
    let job = db
        .mail_queue()
        .enqueue(input(b"held body"), 100)
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::In, "worker", 100, 10)
        .await
        .unwrap()
        .unwrap();
    let held = db
        .moderation()
        .hold(
            &lease,
            &list_id,
            "alice@example.invalid",
            "Subject",
            "nonmember",
            101,
        )
        .await
        .unwrap();
    assert_eq!(held.list_id, list_id);
    assert_eq!(held.message_id, job.message_id);
    assert!(held.disposition.is_none());
    assert_eq!(
        db.mail_queue().job(job.id).await.unwrap().state,
        JobState::Done
    );

    // A second hold attempt on the now-stale lease must fence, not double-hold.
    assert!(
        db.moderation()
            .hold(
                &lease,
                &list_id,
                "alice@example.invalid",
                "Subject",
                "again",
                102
            )
            .await
            .is_err()
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM held_messages")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
    let pending = db.moderation().list_pending(&list_id).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, held.id);
}

#[tokio::test]
async fn accept_creates_a_fresh_out_job_with_the_recipient_snapshot_and_fences_double_accept() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let list_id = seeded_list(&db).await;
    db.mail_queue()
        .enqueue(input(b"held body"), 100)
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::In, "worker", 100, 10)
        .await
        .unwrap()
        .unwrap();
    let held = db
        .moderation()
        .hold(
            &lease,
            &list_id,
            "alice@example.invalid",
            "Subject",
            "nonmember",
            101,
        )
        .await
        .unwrap();

    let (accepted, job) = db
        .moderation()
        .accept(
            held.id,
            None,
            &["bob@example.invalid".into(), "carol@example.invalid".into()],
            5,
            200,
        )
        .await
        .unwrap();
    assert_eq!(accepted.disposition, Some(Disposition::Accepted));
    assert_eq!(job.queue, Queue::Out);
    assert_eq!(job.message_id, held.message_id);
    let mut pending = db.mail_queue().pending_recipients(job.id).await.unwrap();
    pending.sort();
    assert_eq!(
        pending,
        vec!["bob@example.invalid", "carol@example.invalid"]
    );

    // Double-accept (e.g. a racing moderator) must not create a second out job.
    assert!(
        db.moderation()
            .accept(held.id, None, &["dave@example.invalid".into()], 5, 201)
            .await
            .is_err()
    );
    let out_jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs WHERE queue='out'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(out_jobs, 1);
    // And an already-accepted message can no longer be pending, rejected, or discarded.
    assert!(
        db.moderation()
            .reject(held.id, None, "too late", 202)
            .await
            .is_err()
    );
    assert!(
        db.moderation()
            .list_pending(&list_id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn reject_and_discard_record_disposition_without_creating_delivery() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let list_id = seeded_list(&db).await;

    db.mail_queue()
        .enqueue(input(b"reject me"), 100)
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::In, "w", 100, 10)
        .await
        .unwrap()
        .unwrap();
    let rejected_held = db
        .moderation()
        .hold(
            &lease,
            &list_id,
            "bad@example.invalid",
            "Spam",
            "banned",
            101,
        )
        .await
        .unwrap();
    let rejected = db
        .moderation()
        .reject(rejected_held.id, None, "sender is banned", 200)
        .await
        .unwrap();
    assert_eq!(rejected.disposition, Some(Disposition::Rejected));

    db.mail_queue()
        .enqueue(input(b"discard me"), 200)
        .await
        .unwrap();
    let lease2 = db
        .mail_queue()
        .claim(Queue::In, "w", 200, 10)
        .await
        .unwrap()
        .unwrap();
    let discard_held = db
        .moderation()
        .hold(
            &lease2,
            &list_id,
            "spammer@example.invalid",
            "Junk",
            "spam",
            201,
        )
        .await
        .unwrap();
    let discarded = db
        .moderation()
        .discard(discard_held.id, None, "spam", 300)
        .await
        .unwrap();
    assert_eq!(discarded.disposition, Some(Disposition::Discarded));

    let out_jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs WHERE queue='out'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(out_jobs, 0);
    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM audit_log WHERE target_type='held_message' ORDER BY at,id",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(
        actions,
        [
            "moderation.hold",
            "moderation.rejected",
            "moderation.hold",
            "moderation.discarded"
        ]
    );
}

#[tokio::test]
async fn accept_rolls_back_entirely_when_the_moderation_log_write_fails() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let list_id = seeded_list(&db).await;
    db.mail_queue()
        .enqueue(input(b"held body"), 100)
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::In, "w", 100, 10)
        .await
        .unwrap()
        .unwrap();
    let held = db
        .moderation()
        .hold(
            &lease,
            &list_id,
            "alice@example.invalid",
            "Subject",
            "nonmember",
            101,
        )
        .await
        .unwrap();
    sqlx::query("CREATE TRIGGER fail_moderation_log BEFORE INSERT ON moderation_log BEGIN SELECT RAISE(ABORT, 'sabotage'); END")
        .execute(db.pool()).await.unwrap();
    assert!(
        db.moderation()
            .accept(held.id, None, &["bob@example.invalid".into()], 5, 200)
            .await
            .is_err()
    );
    sqlx::query("DROP TRIGGER fail_moderation_log")
        .execute(db.pool())
        .await
        .unwrap();
    // Disposition must still be NULL: the whole transaction rolled back.
    assert_eq!(
        db.moderation().get(held.id).await.unwrap().disposition,
        None
    );
    let out_jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs WHERE queue='out'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(out_jobs, 0);
}
