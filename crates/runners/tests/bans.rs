use listmngr_core::{Config, ListId, MemberRole, SubscriptionMode};
use listmngr_db::{
    AuditContext, Database, NewList, NewMember,
    mail_queue::{JobState, NewMessage, Queue},
};
use std::time::Duration;

async fn post(db: &Database, list: &ListId, sender: &str, label: &str) -> Vec<String> {
    let raw = format!(
        "From: {sender}\r\nMessage-ID: <{label}@example.net>\r\nSubject: Ban fixture\r\n\r\n{label}"
    )
    .into_bytes();
    let job = db
        .mail_queue()
        .enqueue(
            NewMessage {
                raw: raw.clone(),
                external_id: format!("{label}@example.net"),
                context: serde_json::json!({"list_id":list,"envelope_sender":sender}).to_string(),
                queue: Queue::In,
                max_attempts: 1,
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
        "ban-fixture".into(),
        receiver,
    ));
    let completed = tokio::time::timeout(Duration::from_secs(5), async {
        while db.mail_queue().job(job.id).await.unwrap().state != JobState::Done {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    let _ = stop.send(true);
    if let Ok(result) = tokio::time::timeout(Duration::from_secs(2), &mut worker).await {
        result.unwrap();
    } else {
        worker.abort();
        let _ = worker.await;
        panic!("in worker failed to stop");
    }
    completed.unwrap();
    assert_eq!(
        db.mail_queue().message(job.message_id).await.unwrap().raw,
        raw
    );
    sqlx::query_scalar(
        "SELECT queue FROM queue_jobs WHERE message_id=$1 AND queue!='in' ORDER BY queue",
    )
    .bind(job.message_id.0.to_string())
    .fetch_all(db.pool())
    .await
    .unwrap()
}

#[tokio::test]
async fn durable_ban_blocks_all_post_fanout_and_delete_restores_it() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains().create("example.net", "", None).await.unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: "team.example.net".parse().unwrap(),
            display_name: "Team".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    for (email, role) in [
        ("author@example.net", MemberRole::Owner),
        ("reader@example.net", MemberRole::Member),
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
    let audit = AuditContext::new(None, None, None);
    db.bans()
        .create(&list.id, "Author@Example.NET.", &audit)
        .await
        .unwrap();
    assert!(
        post(&db, &list.id, "AUTHOR@EXAMPLE.NET.", "blocked")
            .await
            .is_empty()
    );
    db.bans()
        .delete(&list.id, "author@example.net", &audit)
        .await
        .unwrap();
    assert_eq!(
        post(&db, &list.id, "author@example.net", "released").await,
        ["archive", "digest", "out"]
    );
    db.bans()
        .create(&list.id, "^author@", &audit)
        .await
        .unwrap();
    assert!(
        post(&db, &list.id, "author@example.net", "regex-blocked")
            .await
            .is_empty()
    );
    assert_eq!(
        post(&db, &list.id, "AUTHOR@example.net", "regex-case-control").await,
        ["archive", "digest", "out"]
    );
}
