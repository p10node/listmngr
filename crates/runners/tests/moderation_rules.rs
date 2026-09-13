//! The `in` runner gathers every fact the A2 rules need from the real
//! database and message, and the durable outcome (held row, out job, or
//! silent discard) follows the Mailman chain.
use listmngr_core::{Config, MemberRole, SubscriptionMode};
use listmngr_db::{
    Database, HeaderMatchRow, NewList, NewMember,
    mail_queue::{JobState, NewMessage, Queue},
};
use serde_json::{Value, json};
use std::time::Duration;

const LIST: &str = "rules.example.invalid";
const POSTING: &str = "rules@example.invalid";

struct Outcome {
    held_reason: Option<String>,
    outgoing: i64,
    /// The cooked bytes of the outgoing copy, when one was scheduled.
    outgoing_raw: Option<Vec<u8>>,
}

async fn fixture(settings: Value) -> Database {
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
    db.lists().update(&list.id, &settings).await.unwrap();
    db.members()
        .create(NewMember {
            list_id: list.id.clone(),
            email: "member@example.invalid".into(),
            display_name: String::new(),
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsAddress,
        })
        .await
        .unwrap();
    db
}

async fn post(db: &Database, config: Config, sender: &str, raw: &[u8]) -> Outcome {
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
    let role = listmngr_runners::MailRoleConfig::from_core(&config).unwrap();
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let mut worker = tokio::spawn(listmngr_runners::run_in_processor(
        db.clone(),
        config,
        role,
        "rules-fixture".into(),
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
    let outgoing_raw = if outgoing > 0 {
        let raw = db.mail_queue().message(job.message_id).await.unwrap().raw;
        let list = db.lists().get(&LIST.parse().unwrap()).await.unwrap();
        Some(listmngr_mail::cook_individual_post(&raw, &list, "identity").unwrap())
    } else {
        None
    };
    Outcome {
        held_reason,
        outgoing,
        outgoing_raw,
    }
}

fn message(from: &str, extra_headers: &str, body: &str) -> Vec<u8> {
    format!(
        "From: {from}\r\nTo: {POSTING}\r\nSubject: a post\r\nMessage-ID: <{}@example.invalid>\r\n{extra_headers}\r\n{body}\r\n",
        uuid::Uuid::now_v7()
    )
    .into_bytes()
}

#[tokio::test]
async fn a_list_header_rule_holds_a_member_post_with_the_named_reason() {
    let db = fixture(json!({})).await;
    db.header_matches()
        .replace(
            &LIST.parse().unwrap(),
            &[HeaderMatchRow {
                header: "X-Spam-Flag".into(),
                pattern: "^yes$".into(),
                chain: None,
                tag: None,
            }],
        )
        .await
        .unwrap();
    let outcome = post(
        &db,
        Config::default(),
        "member@example.invalid",
        &message("member@example.invalid", "X-Spam-Flag: YES\r\n", "hello"),
    )
    .await;
    assert_eq!(
        outcome.held_reason.as_deref(),
        Some("Header \"X-Spam-Flag\" matched a header rule")
    );
    assert_eq!(outcome.outgoing, 0);
}

#[tokio::test]
async fn a_list_header_rule_can_discard_silently() {
    let db = fixture(json!({})).await;
    db.header_matches()
        .replace(
            &LIST.parse().unwrap(),
            &[HeaderMatchRow {
                header: "X-Spam-Flag".into(),
                pattern: "yes".into(),
                chain: Some("discard".into()),
                tag: None,
            }],
        )
        .await
        .unwrap();
    let outcome = post(
        &db,
        Config::default(),
        "member@example.invalid",
        &message("member@example.invalid", "X-Spam-Flag: YES\r\n", "hello"),
    )
    .await;
    assert_eq!(outcome.held_reason, None);
    assert_eq!(outcome.outgoing, 0);
}

#[tokio::test]
async fn a_site_antispam_check_holds_via_suspicious_header() {
    let db = fixture(json!({})).await;
    let mut config = Config::default();
    config.antispam.header_checks = vec![listmngr_core::HeaderCheck {
        header: "X-Spam-Score".into(),
        pattern: "^[1-9][0-9]".into(),
    }];
    let outcome = post(
        &db,
        config,
        "member@example.invalid",
        &message("member@example.invalid", "X-Spam-Score: 42\r\n", "hello"),
    )
    .await;
    assert_eq!(
        outcome.held_reason.as_deref(),
        Some("Header \"X-Spam-Score\" matched a header rule")
    );
}

#[tokio::test]
async fn a_correct_approved_key_delivers_a_nonmember_post_without_the_key() {
    let db = fixture(json!({"moderator_password": "open sesame please"})).await;
    let outcome = post(
        &db,
        Config::default(),
        "outsider@elsewhere.invalid",
        &message(
            "outsider@elsewhere.invalid",
            "",
            "Approved: open sesame please\r\nreal content",
        ),
    )
    .await;
    assert_eq!(outcome.held_reason, None);
    assert_eq!(outcome.outgoing, 1);
    let cooked = String::from_utf8_lossy(&outcome.outgoing_raw.unwrap()).into_owned();
    assert!(!cooked.contains("open sesame"), "key leaked: {cooked}");
    assert!(cooked.contains("real content"));
}

#[tokio::test]
async fn a_wrong_or_absent_approved_key_falls_through_to_nonmember_moderation() {
    let db = fixture(json!({"moderator_password": "open sesame please"})).await;
    let outcome = post(
        &db,
        Config::default(),
        "outsider@elsewhere.invalid",
        &message(
            "outsider@elsewhere.invalid",
            "Approved: wrong\r\n",
            "content",
        ),
    )
    .await;
    assert_eq!(outcome.held_reason.as_deref(), Some("moderation policy"));
    assert_eq!(outcome.outgoing, 0);

    let db = fixture(json!({})).await;
    let outcome = post(
        &db,
        Config::default(),
        "outsider@elsewhere.invalid",
        &message(
            "outsider@elsewhere.invalid",
            "Approved: anything\r\n",
            "content",
        ),
    )
    .await;
    assert_eq!(
        outcome.held_reason.as_deref(),
        Some("moderation policy"),
        "a list without a password never approves"
    );
}

#[tokio::test]
async fn legacy_nonmember_lists_and_role_rows_drive_the_outcome() {
    let db = fixture(json!({"accept_these_nonmembers": ["^friend@"]})).await;
    let outcome = post(
        &db,
        Config::default(),
        "friend@elsewhere.invalid",
        &message("friend@elsewhere.invalid", "", "content"),
    )
    .await;
    assert_eq!(outcome.held_reason, None);
    assert_eq!(outcome.outgoing, 1, "accept list delivers");

    let db = fixture(json!({"discard_these_nonmembers": ["junk@elsewhere.invalid"]})).await;
    let outcome = post(
        &db,
        Config::default(),
        "junk@elsewhere.invalid",
        &message("junk@elsewhere.invalid", "", "content"),
    )
    .await;
    assert_eq!(outcome.held_reason, None);
    assert_eq!(outcome.outgoing, 0, "discard list drops silently");

    let db = fixture(json!({})).await;
    db.members()
        .create(NewMember {
            list_id: LIST.parse().unwrap(),
            email: "known@elsewhere.invalid".into(),
            display_name: String::new(),
            role: MemberRole::Nonmember,
            subscription_mode: SubscriptionMode::AsAddress,
        })
        .await
        .unwrap();
    let nonmember = db
        .members()
        .find("known@elsewhere.invalid")
        .await
        .unwrap()
        .into_iter()
        .find(|member| member.role == MemberRole::Nonmember)
        .unwrap();
    sqlx::query("UPDATE members SET moderation_action='accept' WHERE id=$1")
        .bind(nonmember.id.to_string())
        .execute(db.pool())
        .await
        .unwrap();
    let outcome = post(
        &db,
        Config::default(),
        "known@elsewhere.invalid",
        &message("known@elsewhere.invalid", "", "content"),
    )
    .await;
    assert_eq!(outcome.outgoing, 1, "nonmember role row override delivers");
}

#[tokio::test]
async fn deferred_checks_hold_member_posts_with_every_reason() {
    let db = fixture(json!({})).await;
    let raw = format!(
        "From: member@example.invalid\r\nMessage-ID: <{}@example.invalid>\r\n\r\nunsubscribe\r\n",
        uuid::Uuid::now_v7()
    );
    let outcome = post(
        &db,
        Config::default(),
        "member@example.invalid",
        raw.as_bytes(),
    )
    .await;
    assert_eq!(
        outcome.held_reason.as_deref(),
        Some(
            "Message contains administrivia; Message has implicit destination; Message has no subject"
        )
    );
}

#[tokio::test]
async fn an_owner_is_explicitly_accepted_but_emergency_still_holds() {
    let db = fixture(json!({})).await;
    db.members()
        .create(NewMember {
            list_id: LIST.parse().unwrap(),
            email: "owner@example.invalid".into(),
            display_name: String::new(),
            role: MemberRole::Owner,
            subscription_mode: SubscriptionMode::AsAddress,
        })
        .await
        .unwrap();
    // No subject, no destination: an owner still gets through.
    let raw = format!(
        "From: owner@example.invalid\r\nMessage-ID: <{}@example.invalid>\r\n\r\nhi\r\n",
        uuid::Uuid::now_v7()
    );
    let outcome = post(
        &db,
        Config::default(),
        "owner@example.invalid",
        raw.as_bytes(),
    )
    .await;
    assert_eq!(outcome.outgoing, 1);

    db.lists()
        .update(&LIST.parse().unwrap(), &json!({"emergency": true}))
        .await
        .unwrap();
    let outcome = post(
        &db,
        Config::default(),
        "owner@example.invalid",
        &message("owner@example.invalid", "", "hi"),
    )
    .await;
    assert_eq!(
        outcome.held_reason.as_deref(),
        Some("list is in emergency moderation mode")
    );
}
