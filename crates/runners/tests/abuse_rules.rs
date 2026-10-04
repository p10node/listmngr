//! The abuse rules on the real `in` runner: `posting-rate` holds a
//! sender's post once the configured number were accepted in the window —
//! counting that sender on that list and only rows inside the window — and
//! `max-hops` discards a post relayed more times than the site allows.
use listmngr_core::{Config, MemberRole, SubscriptionMode};
use listmngr_db::{
    Database, NewList, NewMember,
    mail_queue::{JobState, NewMessage, Queue},
};
use serde_json::json;
use std::time::Duration;

const LIST: &str = "rules.example.invalid";
const POSTING: &str = "rules@example.invalid";
const MEMBER: &str = "member@example.invalid";
const OTHER: &str = "other@example.invalid";

struct Outcome {
    held_reason: Option<String>,
    outgoing: i64,
    /// `post.*` audit actions with their diffs, oldest first.
    post_audits: Vec<(String, String)>,
}

async fn fixture() -> Database {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: LIST.parse().unwrap(),
            display_name: "Rules".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    db.lists()
        .update(
            &list.id,
            &json!({"respond_to_post_requests": false, "admin_immed_notify": false}),
        )
        .await
        .unwrap();
    for email in [MEMBER, OTHER] {
        db.members()
            .create(NewMember {
                list_id: list.id.clone(),
                email: email.into(),
                display_name: String::new(),
                role: MemberRole::Member,
                subscription_mode: SubscriptionMode::AsAddress,
            })
            .await
            .unwrap();
    }
    db
}

async fn post(db: &Database, config: &Config, sender: &str, raw: &[u8]) -> Outcome {
    let job = db
        .mail_queue()
        .enqueue(
            NewMessage {
                raw: raw.to_vec(),
                external_id: format!("<{}@example.invalid>", uuid::Uuid::now_v7()),
                context: json!({"list_id": LIST, "envelope_sender": sender}).to_string(),
                queue: Queue::In,
                max_attempts: 3,
            },
            chrono::Utc::now().timestamp_millis(),
        )
        .await
        .unwrap();
    let role = listmngr_runners::MailRoleConfig::from_core(config).unwrap();
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let mut worker = tokio::spawn(listmngr_runners::run_in_processor(
        db.clone(),
        config.clone(),
        role,
        "abuse-fixture".into(),
        receiver,
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        while db.mail_queue().job(job.id).await.unwrap().state != JobState::Done {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("in runner did not finish the job");
    stop.send(true).unwrap();
    if tokio::time::timeout(Duration::from_secs(2), &mut worker)
        .await
        .is_err()
    {
        worker.abort();
        let _ = worker.await;
        panic!("in runner did not stop");
    }
    let held_reason: Option<String> =
        sqlx::query_scalar("SELECT reason FROM held_messages ORDER BY rowid DESC LIMIT 1")
            .fetch_optional(db.pool())
            .await
            .unwrap();
    let outgoing: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs WHERE queue='out' AND id NOT IN (SELECT job_id FROM workflow_notices)")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let post_audits: Vec<(String, String)> = sqlx::query_as(
        "SELECT action, diff FROM audit_log WHERE action LIKE 'post.%' ORDER BY at, rowid",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    Outcome {
        held_reason,
        outgoing,
        post_audits,
    }
}

fn message(from: &str, extra_headers: &str) -> Vec<u8> {
    format!(
        "{extra_headers}From: {from}\r\nTo: {POSTING}\r\nSubject: a post\r\nMessage-ID: <{}@example.invalid>\r\n\r\nhello\r\n",
        uuid::Uuid::now_v7()
    )
    .into_bytes()
}

async fn ledger_rows(db: &Database, email: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM posting_rate WHERE list_id=$1 AND email=$2")
        .bind(LIST)
        .bind(email)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn two_posts_an_hour_and_the_third_waits_for_a_moderator() {
    let db = fixture().await;
    let mut config = Config::default();
    config.security.rate_limit.post = Some("2/hour".into());
    for expected_outgoing in [1, 2] {
        let outcome = post(&db, &config, MEMBER, &message(MEMBER, "")).await;
        assert_eq!(outcome.held_reason, None);
        assert_eq!(outcome.outgoing, expected_outgoing);
    }
    let third = post(&db, &config, MEMBER, &message(MEMBER, "")).await;
    assert_eq!(
        third.held_reason.as_deref(),
        Some("Posting rate exceeded: 2 posts accepted in the last hour (at most 2)")
    );
    assert_eq!(third.outgoing, 2, "the held post was not delivered");
    assert_eq!(
        ledger_rows(&db, MEMBER).await,
        2,
        "a held post is not counted"
    );
    // Another sender is not slowed by this one.
    let other = post(&db, &config, OTHER, &message(OTHER, "")).await;
    assert_eq!(
        other.held_reason.as_deref(),
        Some("Posting rate exceeded: 2 posts accepted in the last hour (at most 2)"),
        "the held reason is still the third post's row"
    );
    assert_eq!(other.outgoing, 3);
    assert_eq!(ledger_rows(&db, OTHER).await, 1);
    // Rows older than the window no longer count.
    sqlx::query("UPDATE posting_rate SET posted_at=posted_at-3600001 WHERE email=$1")
        .bind(MEMBER)
        .execute(db.pool())
        .await
        .unwrap();
    let later = post(&db, &config, MEMBER, &message(MEMBER, "")).await;
    assert_eq!(later.outgoing, 4);
    assert_eq!(ledger_rows(&db, MEMBER).await, 3);
    // The sender is matched without regard to case.
    let shouting = post(&db, &config, "MEMBER@Example.INVALID", &message(MEMBER, "")).await;
    assert_eq!(shouting.outgoing, 5);
    assert_eq!(ledger_rows(&db, MEMBER).await, 4);
    let capped = post(&db, &config, MEMBER, &message(MEMBER, "")).await;
    assert_eq!(capped.outgoing, 5);
    assert!(
        capped
            .held_reason
            .as_deref()
            .unwrap()
            .starts_with("Posting rate exceeded")
    );
}

#[tokio::test]
async fn without_a_limit_nothing_is_counted_and_nothing_is_held() {
    let db = fixture().await;
    let config = Config::default();
    for expected_outgoing in [1, 2, 3, 4] {
        let outcome = post(&db, &config, MEMBER, &message(MEMBER, "")).await;
        assert_eq!(outcome.held_reason, None);
        assert_eq!(outcome.outgoing, expected_outgoing);
    }
    assert_eq!(ledger_rows(&db, MEMBER).await, 0);
}

fn received(n: usize) -> String {
    use std::fmt::Write as _;
    let mut headers = String::new();
    for i in 0..n {
        write!(
            headers,
            "Received: from hop{i}.example.invalid by hop{}.example.invalid; Tue, 2 Sep 2026 09:00:0{} +0000\r\n",
            i + 1,
            i % 10
        )
        .unwrap();
    }
    headers
}

#[tokio::test]
async fn a_post_relayed_more_than_max_received_hops_is_discarded_as_a_loop() {
    let db = fixture().await;
    let mut config = Config::default();
    config.mta.max_received_hops = 3;
    let within = post(&db, &config, MEMBER, &message(MEMBER, &received(3))).await;
    assert_eq!(within.held_reason, None);
    assert_eq!(within.outgoing, 1);
    let over = post(&db, &config, MEMBER, &message(MEMBER, &received(4))).await;
    assert_eq!(over.held_reason, None, "a loop is discarded, not held");
    assert_eq!(over.outgoing, 1);
    let (action, diff) = over.post_audits.last().expect("the discard is audited");
    assert_eq!(action, "post.discard");
    assert!(
        diff.contains("Too many Received: headers (4, at most 3): a mail loop"),
        "{diff}"
    );
    // Zero switches the rule off.
    config.mta.max_received_hops = 0;
    let unlimited = post(&db, &config, MEMBER, &message(MEMBER, &received(40))).await;
    assert_eq!(unlimited.held_reason, None);
    assert_eq!(unlimited.outgoing, 2);
}
