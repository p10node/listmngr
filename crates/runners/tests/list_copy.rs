#[path = "list_copy/controls.rs"]
mod controls;

use listmngr_core::{Config, MemberRole, SubscriptionMode};
use listmngr_db::{
    Database, NewList, NewMember,
    mail_queue::{JobState, NewMessage, Queue},
};
use serde_json::json;
use std::time::Duration;

async fn fixture(db: Database) -> (Database, listmngr_core::ListId) {
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: "size.example.invalid".parse().unwrap(),
            display_name: "Size".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    db.lists()
        .update(&list.id, &json!({"max_num_recipients":0}))
        .await
        .unwrap();
    for (email, role) in [
        ("author@example.invalid", MemberRole::Owner),
        ("Reader@example.invalid", MemberRole::Member),
        ("Other@example.invalid", MemberRole::Member),
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
    sqlx::query("UPDATE preferences SET receive_list_copy=0 WHERE id IN (SELECT preferences_id FROM members WHERE role='member')").execute(db.pool()).await.unwrap();
    (db, list.id)
}

async fn post(
    db: &Database,
    list_id: &listmngr_core::ListId,
    headers: &str,
) -> listmngr_db::mail_queue::MessageId {
    let raw =
        format!("From: author@example.invalid\r\nSubject: Recipients\r\n{headers}\r\nbody\r\n")
            .into_bytes();
    let job = db
        .mail_queue()
        .enqueue(
            NewMessage {
                raw: raw.clone(),
                external_id: "size@example.invalid".into(),
                context: json!({"list_id":list_id,"envelope_sender":"author@example.invalid"})
                    .to_string(),
                queue: Queue::In,
                max_attempts: 3,
            },
            chrono::Utc::now().timestamp_millis(),
        )
        .await
        .unwrap();
    let config = Config::default();
    let role = listmngr_runners::MailRoleConfig::from_core(&config).unwrap();
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let mut worker = tokio::spawn(listmngr_runners::run_in_processor(
        db.clone(),
        config,
        role,
        "size-fixture".into(),
        receiver,
    ));
    let completed = tokio::time::timeout(Duration::from_secs(5), async {
        while db.mail_queue().job(job.id).await.unwrap().state != JobState::Done {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    stop.send(true).unwrap();
    if let Ok(result) = tokio::time::timeout(Duration::from_secs(2), &mut worker).await {
        result.unwrap();
    } else {
        worker.abort();
        let _ = worker.await;
        panic!("in runner did not stop");
    }
    completed.unwrap();
    assert_eq!(
        db.mail_queue().message(job.message_id).await.unwrap().raw,
        raw
    );
    job.message_id
}

async fn recipients(db: &Database, message: listmngr_db::mail_queue::MessageId) -> Vec<String> {
    sqlx::query_scalar("SELECT email FROM delivery_recipients WHERE job_id IN (SELECT id FROM queue_jobs WHERE message_id=$1 AND queue='out') ORDER BY email")
        .bind(message.0.to_string()).fetch_all(db.pool()).await.unwrap()
}
async fn sqlite() -> (Database, listmngr_core::ListId) {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    fixture(db).await
}
async fn held_post(
    db: &Database,
    list: &listmngr_core::ListId,
    headers: &str,
) -> listmngr_db::moderation::HeldMessage {
    db.lists()
        .update(list, &json!({"emergency":true}))
        .await
        .unwrap();
    let message = post(db, list, headers).await;
    db.lists()
        .update(list, &json!({"emergency":false}))
        .await
        .unwrap();
    let held = db
        .moderation()
        .list_pending(list)
        .await
        .unwrap()
        .into_iter()
        .find(|h| h.message_id == message)
        .unwrap();
    assert!(recipients(db, message).await.is_empty());
    held
}
#[tokio::test]
async fn held_list_copy_uses_original_raw_on_release() {
    let (db, list) = sqlite().await;
    let held = held_post(
        &db,
        &list,
        "To: outside@outside.invalid\r\nTo: team: READER@EXAMPLE.INVALID.;\r\n",
    )
    .await;
    db.moderation()
        .review(
            held.id,
            &listmngr_db::AuditContext::system(),
            &listmngr_db::moderation::ReviewAction::Accept { max_attempts: 3 },
            "fixture",
            chrono::Utc::now().timestamp_millis(),
        )
        .await
        .unwrap();
    assert_eq!(
        recipients(&db, held.message_id).await,
        ["Other@example.invalid"]
    );
}
#[tokio::test]
async fn digest_list_copy_checks_repeated_headers() {
    let (db, list) = sqlite().await;
    sqlx::query("UPDATE preferences SET delivery_mode='plaintext_digests' WHERE id IN (SELECT preferences_id FROM members WHERE role='member')").execute(db.pool()).await.unwrap();
    let message = post(
        &db,
        &list,
        "To: outside@outside.invalid\r\nTo: READER@EXAMPLE.INVALID.\r\n",
    )
    .await;
    listmngr_runners::digests::tick(&db, "copy-fixture", false)
        .await
        .unwrap();
    let snapshot: String = sqlx::query_scalar("SELECT recipients FROM digest_posts WHERE id=$1")
        .bind(message.0.to_string())
        .fetch_one(db.pool())
        .await
        .unwrap();
    let snapshot: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
    assert_eq!(
        snapshot,
        json!([{"email":"Other@example.invalid", "mode":"plaintext_digests"}])
    );
}
#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns isolated schema"]
async fn postgres_list_copy_controls() {
    let url = std::env::var("TEST_POSTGRES_URL").unwrap();
    let admin = Database::connect(&url, 1).await.unwrap();
    let schema = format!("runner_copy_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(admin.pool())
        .await
        .unwrap();
    let sep = if url.contains('?') { '&' } else { '?' };
    let fixture = format!("{url}{sep}options=-csearch_path%3D{schema}");
    let result = tokio::spawn(async move {
        let db = Database::connect(&fixture, 3).await.unwrap();
        db.migrate().await.unwrap();
        controls::controls(db).await;
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
async fn sqlite_list_copy_controls() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    controls::controls(db).await;
}

#[tokio::test]
async fn regular_list_copy_suppresses_direct_canonical_mailbox() {
    let (db, list) = sqlite().await;
    let message = post(
        &db,
        &list,
        "To: other@outside.invalid\r\nCc: team: READER@EXAMPLE.INVALID.;\r\n",
    )
    .await;
    assert_eq!(recipients(&db, message).await, ["Other@example.invalid"]);
}
