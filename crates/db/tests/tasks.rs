//! Mailman's task runner: the periodic sweep that expires what nobody
//! confirmed, drops what nobody references any more, resets stale bounce
//! scores — and `notify`, the daily summary of what moderators still owe.
// Scores are exact integers stored as doubles; equality is the assertion.
#![allow(clippy::float_cmp)]
use listmngr_core::{MemberRole, SubscriptionMode};
use listmngr_db::mail_queue::{NewMessage, Queue};
use listmngr_db::tasks::TaskSummary;
use listmngr_db::workflows::SubscriptionAction;
use listmngr_db::{Database, NewList, NewMember};
use serde_json::json;

const LIST: &str = "dev.example.invalid";
const DAY_MS: i64 = 86_400_000;

async fn fixture() -> (Database, listmngr_core::ListId) {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    prepare(db).await
}

/// The list, its owner and one member on an already connected database.
async fn prepare(db: Database) -> (Database, listmngr_core::ListId) {
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: LIST.parse().unwrap(),
            display_name: "Dev".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    db.lists()
        .update(
            &list.id,
            &json!({"respond_to_post_requests": false, "admin_immed_notify": false, "process_bounces": true, "bounce_info_stale_after": 7}),
        )
        .await
        .unwrap();
    for (email, role) in [
        ("owner@example.invalid", MemberRole::Owner),
        ("member@example.net", MemberRole::Member),
    ] {
        db.members()
            .create(NewMember {
                list_id: list.id.clone(),
                email: email.into(),
                display_name: String::new(),
                role,
                subscription_mode: SubscriptionMode::AsAddress,
            })
            .await
            .unwrap();
    }
    (db, list.id)
}

async fn count(db: &Database, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(db.pool()).await.unwrap()
}

/// An `in` job for a message with `subject`, claimed at `at`.
async fn claimed_job(db: &Database, subject: &str, at: i64) -> listmngr_db::mail_queue::Lease {
    let job = db
        .mail_queue()
        .enqueue(
            NewMessage {
                raw: format!("Subject: {subject}\r\n\r\nbody\r\n").into_bytes(),
                external_id: format!("<{}@example.invalid>", uuid::Uuid::now_v7()),
                context: json!({"list_id": LIST}).to_string(),
                queue: Queue::In,
                max_attempts: 3,
            },
            at,
        )
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::In, "task-test", at, 30_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lease.job.id, job.id);
    lease
}

/// A finished `in` job (and its message) acknowledged at `at`.
async fn finished_job(db: &Database, at: i64) {
    let lease = claimed_job(db, &format!("old {at}"), at).await;
    db.mail_queue()
        .complete_with_children(&lease, at, &[])
        .await
        .unwrap();
}

/// A post held for moderation at `at`.
async fn held_job(db: &Database, subject: &str, at: i64) {
    let lease = claimed_job(db, subject, at).await;
    db.moderation()
        .hold(
            &lease,
            &LIST.parse().unwrap(),
            "poster@example.net",
            subject,
            "moderation",
            at,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn the_sweep_expires_tokens_probes_and_cooldowns_past_their_time() {
    let (db, list) = fixture().await;
    the_sweep_expires_tokens_probes_and_cooldowns_past_their_time_on(&db, &list).await;
}

async fn the_sweep_expires_tokens_probes_and_cooldowns_past_their_time_on(
    db: &Database,
    list: &listmngr_core::ListId,
) {
    // A confirmation token issued at day 1 (24 h life), a help cooldown and
    // an autoresponse record from the same day, a probe with a 7-day life.
    db.workflows()
        .request(list, "joiner@example.net", SubscriptionAction::Join, DAY_MS)
        .await
        .unwrap();
    sqlx::query("INSERT INTO email_help_requests(list_id,email,requested_at) VALUES($1,'helped@example.net',$2)")
        .bind(LIST)
        .bind(DAY_MS)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO autoresponse_records(list_id,email,kind,responded_at) VALUES($1,'writer@example.net','owner',$2)")
        .bind(LIST)
        .bind(DAY_MS)
        .execute(db.pool())
        .await
        .unwrap();
    let member: String = sqlx::query_scalar("SELECT m.id FROM members m JOIN addresses a ON a.id=m.address_id WHERE a.email='member@example.net'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO bounce_probes(token_hash,member_id,list_id,sent_at,expires_at) VALUES('h',$1,$2,$3,$4)")
        .bind(&member)
        .bind(LIST)
        .bind(DAY_MS)
        .bind(DAY_MS + 7 * DAY_MS)
        .execute(db.pool())
        .await
        .unwrap();

    // Day 2: nothing but the 24 h token and the hour-old cooldown are due.
    let summary = db.tasks().sweep(2 * DAY_MS + 1, 30 * DAY_MS).await.unwrap();
    assert_eq!(summary.expired_workflows, 1, "{summary:?}");
    assert_eq!(summary.expired_help_requests, 1, "{summary:?}");
    assert_eq!(summary.expired_probes, 0, "{summary:?}");
    assert_eq!(
        count(db, "SELECT COUNT(*) FROM subscription_workflows").await,
        0
    );
    assert_eq!(
        count(db, "SELECT COUNT(*) FROM email_help_requests").await,
        0
    );
    assert_eq!(count(db, "SELECT COUNT(*) FROM bounce_probes").await, 1);

    // Day 9: the probe is past its life; the autoresponse record is past
    // the list's 90-day grace only on day 92.
    let summary = db.tasks().sweep(9 * DAY_MS, 30 * DAY_MS).await.unwrap();
    assert_eq!(summary.expired_probes, 1, "{summary:?}");
    assert_eq!(summary.expired_autoresponses, 0, "{summary:?}");
    let summary = db.tasks().sweep(92 * DAY_MS, 30 * DAY_MS).await.unwrap();
    assert_eq!(summary.expired_autoresponses, 1, "{summary:?}");
    assert_eq!(
        count(db, "SELECT COUNT(*) FROM autoresponse_records").await,
        0
    );
    let audit = count(
        db,
        "SELECT COUNT(*) FROM audit_log WHERE action='task.sweep'",
    )
    .await;
    assert!(audit >= 3, "each sweep that changed something is audited");
}

#[tokio::test]
async fn the_sweep_collects_finished_jobs_and_orphaned_messages_after_retention() {
    let (db, list) = fixture().await;
    the_sweep_collects_finished_jobs_and_orphaned_messages_after_retention_on(&db, &list).await;
}

/// Two finished jobs; the one past the retention goes with its message
/// and blob, the young one stays.
async fn aged_out_finished_jobs(db: &Database) {
    finished_job(db, DAY_MS).await;
    finished_job(db, 20 * DAY_MS).await;
    assert_eq!(count(db, "SELECT COUNT(*) FROM queue_jobs").await, 2);
    assert_eq!(count(db, "SELECT COUNT(*) FROM messages").await, 2);
    let summary = db.tasks().sweep(40 * DAY_MS, 30 * DAY_MS).await.unwrap();
    assert_eq!(summary.collected_jobs, 1, "{summary:?}");
    assert_eq!(summary.collected_messages, 1, "{summary:?}");
    assert_eq!(count(db, "SELECT COUNT(*) FROM queue_jobs").await, 1);
    assert_eq!(count(db, "SELECT COUNT(*) FROM messages").await, 1);
    assert_eq!(
        count(db, "SELECT COUNT(*) FROM message_blobs").await,
        1,
        "a blob nobody references is gone"
    );
    // The retained job is still the young one.
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM queue_jobs WHERE run_after=1728000000"
        )
        .await,
        1
    );
}

async fn the_sweep_collects_finished_jobs_and_orphaned_messages_after_retention_on(
    db: &Database,
    _list: &listmngr_core::ListId,
) {
    aged_out_finished_jobs(db).await;

    // A message still referenced by a live job, a pending held message or
    // a shunted job is never collected, however old.
    let old = DAY_MS;
    held_job(db, "held", old).await;
    let lease = claimed_job(db, "shunted", old).await;
    db.mail_queue().shunt(&lease, old, "fixture").await.unwrap();
    let summary = db.tasks().sweep(400 * DAY_MS, 30 * DAY_MS).await.unwrap();
    assert_eq!(
        summary.collected_jobs, 2,
        "the young done job and the held post's in job aged out: {summary:?}"
    );
    assert_eq!(
        count(db, "SELECT COUNT(*) FROM messages").await,
        2,
        "the held and the shunted messages stay"
    );

    // Once the held message is decided and old enough, it goes too.
    sqlx::query("UPDATE held_messages SET disposition='discarded', disposed_at=$1")
        .bind(old)
        .execute(db.pool())
        .await
        .unwrap();
    let summary = db.tasks().sweep(400 * DAY_MS, 30 * DAY_MS).await.unwrap();
    assert_eq!(summary.collected_held, 1, "{summary:?}");
    assert_eq!(count(db, "SELECT COUNT(*) FROM held_messages").await, 0);
    assert_eq!(
        count(db, "SELECT COUNT(*) FROM messages").await,
        1,
        "only the shunted one"
    );
}

#[tokio::test]
async fn the_sweep_resets_stale_bounce_scores() {
    let (db, list) = fixture().await;
    the_sweep_resets_stale_bounce_scores_on(&db, &list).await;
}

async fn the_sweep_resets_stale_bounce_scores_on(db: &Database, _list: &listmngr_core::ListId) {
    sqlx::query("UPDATE members SET bounce_score=2.5, last_bounce_received=$1 WHERE role='member'")
        .bind(
            chrono::DateTime::from_timestamp_millis(DAY_MS)
                .unwrap()
                .to_rfc3339(),
        )
        .execute(db.pool())
        .await
        .unwrap();
    // Within `bounce_info_stale_after` (7 days) the score stands.
    let summary = db.tasks().sweep(5 * DAY_MS, 30 * DAY_MS).await.unwrap();
    assert_eq!(summary.stale_bounces_reset, 0, "{summary:?}");
    let summary = db.tasks().sweep(9 * DAY_MS, 30 * DAY_MS).await.unwrap();
    assert_eq!(summary.stale_bounces_reset, 1, "{summary:?}");
    let (score, received): (f64, Option<String>) = sqlx::query_as(
        "SELECT bounce_score, last_bounce_received FROM members WHERE role='member'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(score, 0.0);
    assert_eq!(received, None);
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM audit_log WHERE action='bounce.stale_reset'"
        )
        .await,
        1
    );
    let empty = TaskSummary::default();
    assert!(!empty.changed(), "an idle sweep reports no change");
}

#[tokio::test]
async fn notify_tells_the_owners_what_is_waiting_and_only_when_something_is() {
    let (db, list) = fixture().await;
    notify_tells_the_owners_what_is_waiting_and_only_when_something_is_on(&db, &list).await;
}

async fn notify_tells_the_owners_what_is_waiting_and_only_when_something_is_on(
    db: &Database,
    list: &listmngr_core::ListId,
) {
    assert_eq!(
        db.tasks().notify(DAY_MS).await.unwrap(),
        0,
        "nothing pending, no mail"
    );
    db.lists()
        .update(list, &json!({"subscription_policy": "moderate"}))
        .await
        .unwrap();
    db.workflows()
        .request(list, "joiner@example.net", SubscriptionAction::Join, DAY_MS)
        .await
        .unwrap();
    held_job(db, "A held post", DAY_MS).await;

    assert_eq!(
        db.tasks().notify(DAY_MS + 1).await.unwrap(),
        1,
        "one list notified"
    );
    let raws: Vec<Vec<u8>> = sqlx::query_scalar(
        "SELECT b.raw FROM workflow_notices n JOIN queue_jobs q ON q.id=n.job_id JOIN messages m ON m.id=q.message_id JOIN message_blobs b ON b.store_key=m.store_key",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(raws.len(), 1, "one notice to the one owner");
    let text = String::from_utf8_lossy(&raws[0]);
    assert!(text.contains("To: owner@example.invalid"), "{text}");
    assert!(text.contains("has 2 moderation requests waiting"), "{text}");
    assert!(
        text.contains("Held messages:") && text.contains("poster@example.net: A held post"),
        "{text}"
    );
    assert!(
        text.contains("Held subscriptions:") && text.contains("    joiner@example.net"),
        "{text}"
    );
    assert!(!text.contains("Held unsubscriptions:"), "{text}");
    // Nothing new: no second reminder is forced, but the next run repeats it.
    assert_eq!(db.tasks().notify(DAY_MS + 2).await.unwrap(), 1);
    assert_eq!(db.tasks().pending(list).await.unwrap().total(), 2);
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM audit_log WHERE action='list.notify'"
        )
        .await,
        2
    );
}

/// Every scenario above once more on `PostgreSQL`, each in its own
/// disposable schema: the batched deletes, row-value `IN`, the bound
/// `LIMIT` and the bigint arithmetic are the parts an engine can read
/// differently.
#[tokio::test]
#[ignore = "requires explicit disposable TEST_POSTGRES_URL; uses own schemas"]
async fn postgres_task_sweep_and_notify_contract() {
    type Scenario =
        for<'a> fn(
            &'a Database,
            &'a listmngr_core::ListId,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>;
    sqlx::any::install_default_drivers();
    let url = std::env::var("TEST_POSTGRES_URL").expect("explicit fixture database required");
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .unwrap();
    let scenarios: [(&str, Scenario); 4] = [
        ("expire", |db, list| {
            Box::pin(the_sweep_expires_tokens_probes_and_cooldowns_past_their_time_on(db, list))
        }),
        ("collect", |db, list| {
            Box::pin(
                the_sweep_collects_finished_jobs_and_orphaned_messages_after_retention_on(db, list),
            )
        }),
        ("stale", |db, list| {
            Box::pin(the_sweep_resets_stale_bounce_scores_on(db, list))
        }),
        ("notify", |db, list| {
            Box::pin(
                notify_tells_the_owners_what_is_waiting_and_only_when_something_is_on(db, list),
            )
        }),
    ];
    for (name, scenario) in scenarios {
        let schema = format!("tasks_{name}_{}", uuid::Uuid::now_v7().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .unwrap();
        let isolated = format!(
            "{url}{}options=-csearch_path%3D{schema}",
            if url.contains('?') { '&' } else { '?' }
        );
        let result = tokio::spawn(async move {
            let (db, list) = prepare(Database::connect(&isolated, 2).await.unwrap()).await;
            scenario(&db, &list).await;
        })
        .await;
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(&admin)
            .await
            .unwrap();
        result.unwrap_or_else(|error| panic!("{name}: {error}"));
    }
}
