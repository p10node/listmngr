use listmngr_core::{Config, MemberRole, SubscriptionMode};
use listmngr_db::{
    Database, NewList, NewMember,
    mail_queue::{NewMessage, Queue},
};
use serde_json::json;

async fn post(style: &str, role: MemberRole, setting: Option<&str>) -> (i64, i64) {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: "news.example.invalid".parse().unwrap(),
            display_name: "News".into(),
            style: style.into(),
        })
        .await
        .unwrap();
    if let Some(action) = setting {
        db.lists()
            .update(&list.id, &json!({"default_member_action": action}))
            .await
            .unwrap();
    }
    db.members()
        .create(NewMember {
            list_id: list.id.clone(),
            email: "member@example.invalid".into(),
            role,
            subscription_mode: SubscriptionMode::AsAddress,
            display_name: String::new(),
        })
        .await
        .unwrap();
    // Give the accepted-path control a distinct regular recipient even for owner posts.
    db.members()
        .create(NewMember {
            list_id: list.id.clone(),
            email: "reader@example.invalid".into(),
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsAddress,
            display_name: String::new(),
        })
        .await
        .unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    let job = db.mail_queue().enqueue(NewMessage {
        raw: b"From: member@example.invalid\r\nTo: news@example.invalid\r\nSubject: An ordinary post\r\n\r\nPayload\r\n".to_vec(),
        external_id: "announcement@example.invalid".into(),
        context: json!({"list_id":list.id,"envelope_sender":"member@example.invalid"}).to_string(),
        queue: Queue::In, max_attempts: 3,
    }, now).await.unwrap();
    let config = Config::default();
    let role = listmngr_runners::MailRoleConfig::from_core(&config).unwrap();
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(listmngr_runners::run_in_processor(
        db.clone(),
        config,
        role,
        "fixture".into(),
        receiver,
    ));
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if db.mail_queue().job(job.id).await.unwrap().state
                == listmngr_db::mail_queue::JobState::Done
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    stop.send(true).unwrap();
    worker.await.unwrap();
    let held = sqlx::query_scalar("SELECT COUNT(*) FROM held_messages")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let out = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs WHERE queue='out' AND id NOT IN (SELECT job_id FROM workflow_notices)")
        .fetch_one(db.pool())
        .await
        .unwrap();
    (held, out)
}

#[tokio::test]
async fn announcement_holds_ordinary_members_but_allows_owners_and_regular_lists() {
    assert_eq!(
        post("legacy-announce", MemberRole::Member, None).await,
        (1, 0)
    );
    assert_eq!(
        post("legacy-announce", MemberRole::Owner, None).await,
        (0, 1)
    );
    assert_eq!(
        post("legacy-default", MemberRole::Member, None).await,
        (0, 1)
    );
}

#[tokio::test]
async fn per_list_member_policy_controls_real_queue_side_effect() {
    assert_eq!(
        post("legacy-default", MemberRole::Member, Some("hold")).await,
        (1, 0)
    );
    assert_eq!(
        post("legacy-announce", MemberRole::Member, Some("accept")).await,
        (0, 1)
    );
}
