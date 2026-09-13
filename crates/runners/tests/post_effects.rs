//! Mailman's `after-delivery`, `acknowledge` and delivery-time `decorate`:
//! an accepted post bumps the list's post counter in the accepting
//! transaction, a member who asked for it gets a receipt in their language,
//! and only the subscriber copy carries the list header and footer.
use listmngr_core::{Config, MemberRole, SubscriptionMode};
use listmngr_db::{
    Database, NewList, NewMember,
    mail_queue::{JobState, NewMessage, Queue},
    templates::Scope,
};
use serde_json::json;
use std::time::Duration;

const LIST: &str = "effects.example.invalid";
const POSTING: &str = "effects@example.invalid";

async fn fixture() -> Database {
    fixture_with(Database::connect("sqlite::memory:", 1).await.unwrap()).await
}

async fn fixture_with(db: Database) -> Database {
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: LIST.parse().unwrap(),
            display_name: "Effects".into(),
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
    for email in ["author@example.invalid", "reader@example.invalid"] {
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

fn message(subject: &str) -> Vec<u8> {
    format!(
        "From: author@example.invalid\r\nTo: {POSTING}\r\nSubject: {subject}\r\nMessage-ID: <{}@example.invalid>\r\nContent-Type: text/plain; charset=us-ascii\r\n\r\nplain body\r\n",
        uuid::Uuid::now_v7()
    )
    .into_bytes()
}

/// Post as the author and run the `in` processor until the job is done.
async fn post(db: &Database, raw: &[u8]) -> listmngr_db::mail_queue::MessageId {
    let job = db
        .mail_queue()
        .enqueue(
            NewMessage {
                raw: raw.to_vec(),
                external_id: format!("<{}@example.invalid>", uuid::Uuid::now_v7()),
                context: json!({"list_id": LIST, "envelope_sender": "author@example.invalid"})
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
        "effects-fixture".into(),
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
    job.message_id
}

/// Every generated notice as (recipient, subject, body text).
async fn notices(db: &Database) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let ids: Vec<String> = sqlx::query_scalar("SELECT q.message_id FROM queue_jobs q JOIN workflow_notices n ON n.job_id=q.id ORDER BY q.run_after")
        .fetch_all(db.pool())
        .await
        .unwrap();
    for id in ids {
        let message = db
            .mail_queue()
            .message(listmngr_db::mail_queue::MessageId(id.parse().unwrap()))
            .await
            .unwrap();
        let parsed = mail_parser::MessageParser::default()
            .parse(&message.raw)
            .unwrap();
        out.push((
            parsed
                .to()
                .and_then(|to| to.first())
                .and_then(|addr| addr.address())
                .unwrap_or_default()
                .to_owned(),
            parsed.subject().unwrap_or_default().to_owned(),
            parsed.body_text(0).unwrap_or_default().into_owned(),
        ));
    }
    out
}

#[tokio::test]
async fn an_accepted_post_bumps_the_post_counter_in_the_accepting_transaction() {
    let db = fixture().await;
    let before = db.lists().get(&LIST.parse().unwrap()).await.unwrap();
    assert_eq!(before.post_id, 1);
    assert!(before.last_post_at.is_none());
    post(&db, &message("first")).await;
    post(&db, &message("second")).await;
    let after = db.lists().get(&LIST.parse().unwrap()).await.unwrap();
    assert_eq!(
        after.post_id, 3,
        "after-delivery ran once per accepted post"
    );
    let stamped = after.last_post_at.expect("last_post_at stamped");
    assert!(
        chrono::Utc::now()
            .signed_duration_since(stamped)
            .num_seconds()
            < 60
    );
}

#[tokio::test]
async fn a_member_who_asked_for_acknowledgements_gets_a_receipt_in_their_language() {
    let db = fixture().await;
    post(&db, &message("no receipt wanted")).await;
    assert!(
        notices(&db).await.is_empty(),
        "acknowledge_posts is off by default"
    );

    let author = db
        .members()
        .find("author@example.invalid")
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    db.preferences()
        .set_member(
            author.id,
            listmngr_core::Preferences {
                acknowledge_posts: Some(true),
                preferred_language: Some("vi".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    post(&db, &message("Tin mới")).await;
    let notices = notices(&db).await;
    assert_eq!(notices.len(), 1, "{notices:?}");
    let (to, subject, body) = &notices[0];
    assert_eq!(to, "author@example.invalid");
    assert_eq!(subject, "Xác nhận đã nhận bài gửi tới Effects");
    assert!(body.contains("Tin mới"), "{body}");
    assert!(body.contains("Effects"), "{body}");
    let from: Option<String> = sqlx::query_scalar("SELECT m.raw FROM messages mm JOIN message_blobs m ON m.store_key=mm.store_key JOIN queue_jobs q ON q.message_id=mm.id JOIN workflow_notices n ON n.job_id=q.id LIMIT 1")
        .fetch_optional(db.pool())
        .await
        .unwrap()
        .map(|raw: Vec<u8>| String::from_utf8_lossy(&raw).into_owned());
    assert!(
        from.unwrap_or_default()
            .contains(&format!("From: {}", "effects-bounces@example.invalid")),
        "the receipt comes from the bounces address"
    );
}

#[tokio::test]
async fn only_the_subscriber_copy_is_decorated_with_expanded_templates() {
    let db = fixture().await;
    let list_id: listmngr_core::ListId = LIST.parse().unwrap();
    db.templates()
        .set_body(
            &Scope::List(list_id.clone()),
            "list:member:regular:header",
            "en",
            "[$display_name] header for $listname\n",
        )
        .await
        .unwrap();
    let raw = message("decorated");
    let message_id = post(&db, &raw).await;
    let stored = db.mail_queue().message(message_id).await.unwrap();
    let context = stored.context.clone();

    let (subscriber_copy, mail_from) =
        listmngr_runners::prepare_individual(&db, &stored.raw, &context, uuid::Uuid::now_v7())
            .await
            .ok()
            .unwrap();
    assert_eq!(mail_from, "effects-bounces@example.invalid");
    let body = mail_parser::MessageParser::default()
        .parse(&subscriber_copy)
        .unwrap()
        .body_text(0)
        .unwrap()
        .replace("\r\n", "\n");
    assert!(
        body.starts_with("[Effects] header for effects@example.invalid\nplain body\n"),
        "{body}"
    );
    assert!(
        body.contains("Effects mailing list -- effects@example.invalid\nTo unsubscribe send an email to effects-leave@example.invalid"),
        "Mailman's default footer, expanded: {body}"
    );

    let list = db.lists().get(&list_id).await.unwrap();
    for target in [
        listmngr_mail::handlers::Target::Archive,
        listmngr_mail::handlers::Target::Digest,
    ] {
        let copy = listmngr_mail::handlers::cook_for(target, &stored.raw, &list, "id").unwrap();
        let text = String::from_utf8_lossy(&copy);
        assert!(
            !text.contains("header for"),
            "{target:?} must not be decorated"
        );
        assert!(!text.contains("To unsubscribe send an email"), "{target:?}");
    }
}

#[tokio::test]
async fn delivered_and_archived_copies_advertise_the_archive_when_the_site_url_is_known() {
    let db = fixture_with(
        Database::connect("sqlite::memory:", 1)
            .await
            .unwrap()
            .with_base_url("https://lists.example.invalid/"),
    )
    .await;
    let raw = message("archived");
    let message_id = post(&db, &raw).await;
    let stored = db.mail_queue().message(message_id).await.unwrap();
    let (copy, _) = listmngr_runners::prepare_individual(
        &db,
        &stored.raw,
        &stored.context,
        uuid::Uuid::now_v7(),
    )
    .await
    .ok()
    .unwrap();
    let hash = listmngr_mail::message_id_hash(
        &listmngr_mail::header_value(&stored.raw, "message-id").unwrap(),
    )
    .unwrap();
    assert_eq!(
        listmngr_mail::header_value(&copy, "List-Archive").as_deref(),
        Some("<https://lists.example.invalid/archives/list/effects.example.invalid/>")
    );
    assert_eq!(
        listmngr_mail::header_value(&copy, "Archived-At").as_deref(),
        Some(format!("<https://lists.example.invalid/archives/list/effects.example.invalid/message/{hash}/>").as_str())
    );
    assert_eq!(
        listmngr_mail::header_value(&copy, "Sender").as_deref(),
        Some("effects-bounces@example.invalid")
    );
    assert_eq!(
        listmngr_mail::header_value(&copy, "List-Help").as_deref(),
        Some("<mailto:effects-request@example.invalid?subject=help>")
    );

    // Without a configured base URL nothing points at a web origin.
    let db = fixture().await;
    let raw = message("unadvertised");
    let message_id = post(&db, &raw).await;
    let stored = db.mail_queue().message(message_id).await.unwrap();
    let (copy, _) = listmngr_runners::prepare_individual(
        &db,
        &stored.raw,
        &stored.context,
        uuid::Uuid::now_v7(),
    )
    .await
    .ok()
    .unwrap();
    assert!(listmngr_mail::header_value(&copy, "List-Archive").is_none());
    assert!(listmngr_mail::header_value(&copy, "Archived-At").is_none());
    assert!(listmngr_mail::header_value(&copy, "Message-ID-Hash").is_some());
}
