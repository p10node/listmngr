//! Notices are rendered in the recipient's language: the member's own
//! preference wins, then the list's `preferred_language`, then the site
//! default, each negotiated down to a shipped catalog with English fallback.
use listmngr_core::{ListId, MemberRole, SubscriptionMode};
use listmngr_db::mail_queue::{NewMessage, Queue};
use listmngr_db::{Database, NewList, NewMember};
use serde_json::json;

const RAW: &[u8] = b"From: author@elsewhere.invalid\r\nTo: dev@example.invalid\r\nSubject: private subject\r\nMessage-ID: <original@example.invalid>\r\n\r\nPRIVATE BODY";

async fn fixture(list_language: &str, site_default: &str) -> (Database, ListId) {
    let db = Database::connect("sqlite::memory:", 1)
        .await
        .unwrap()
        .with_default_language(site_default);
    fixture_on(db, list_language).await
}

async fn fixture_on(db: Database, list_language: &str) -> (Database, ListId) {
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "example", None)
        .await
        .unwrap();
    let list: ListId = "dev.example.invalid".parse().unwrap();
    db.lists()
        .create(NewList {
            list_id: list.clone(),
            display_name: "Dev Chat".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    db.lists()
        .update(
            &list,
            &json!({"preferred_language": list_language, "send_welcome_message": true}),
        )
        .await
        .unwrap();
    (db, list)
}

async fn notices(db: &Database) -> Vec<(String, String, String)> {
    // Welcomes are stamped with the wall clock; holds use the fixture clock.
    let now_ms = chrono::Utc::now().timestamp_millis() + 1_000;
    let mut out = Vec::new();
    // A lease long enough that a slow full-suite run cannot let an earlier
    // claim expire and be counted a second time.
    while let Some(lease) = db
        .mail_queue()
        .claim(Queue::Out, "out", now_ms, 60_000)
        .await
        .unwrap()
    {
        let message = db.mail_queue().message(lease.job.message_id).await.unwrap();
        let recipients = db
            .mail_queue()
            .pending_recipients(lease.job.id)
            .await
            .unwrap();
        let parsed = mail_parser::MessageParser::default()
            .parse(&message.raw)
            .unwrap();
        out.push((
            recipients.join(","),
            parsed.subject().unwrap_or_default().to_owned(),
            parsed.body_text(0).unwrap_or_default().into_owned(),
        ));
    }
    out.sort();
    out
}

async fn subscribe(
    db: &Database,
    list: &ListId,
    email: &str,
    role: MemberRole,
) -> listmngr_core::MemberId {
    db.members()
        .create(NewMember {
            list_id: list.clone(),
            email: email.into(),
            display_name: String::new(),
            role,
            subscription_mode: SubscriptionMode::AsAddress,
        })
        .await
        .unwrap()
        .id
}

#[tokio::test]
async fn a_welcome_follows_the_list_language_when_the_member_has_no_preference() {
    let (db, list) = fixture("vi", "en").await;
    subscribe(&db, &list, "member@example.invalid", MemberRole::Member).await;
    let notices = notices(&db).await;
    assert_eq!(notices.len(), 1);
    assert_eq!(
        notices[0].1,
        "Chào mừng bạn đến với hộp thư chung \"Dev Chat\""
    );
    assert!(notices[0].2.contains("dev@example.invalid"));
    assert!(!notices[0].2.contains("Welcome to the"), "{}", notices[0].2);
}

#[tokio::test]
async fn a_member_preference_beats_the_list_language() {
    let (db, list) = fixture("vi", "en").await;
    // Subscribe first (welcome in the list language), then set a preference
    // and trigger a second notice to see the member's own language used.
    let member = subscribe(&db, &list, "member@example.invalid", MemberRole::Member).await;
    let _ = notices(&db).await;
    db.preferences()
        .set_member(
            member,
            listmngr_core::Preferences {
                preferred_language: Some("en".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    db.lists()
        .update(
            &list,
            &json!({"respond_to_post_requests": true, "admin_immed_notify": false}),
        )
        .await
        .unwrap();
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: RAW.to_vec(),
                external_id: "<held@example.invalid>".into(),
                context: json!({"list_id": list, "envelope_sender": "member@example.invalid"})
                    .to_string(),
                queue: Queue::In,
                max_attempts: 3,
            },
            100,
        )
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::In, "worker", 100, 100)
        .await
        .unwrap()
        .unwrap();
    db.moderation()
        .hold(
            &lease,
            &list,
            "member@example.invalid",
            "private subject",
            "reason",
            101,
        )
        .await
        .unwrap();
    let notices = notices(&db).await;
    assert_eq!(notices.len(), 1);
    assert_eq!(
        notices[0].1,
        "Your message to dev@example.invalid awaits moderator approval"
    );
}

#[tokio::test]
async fn owner_notices_use_each_owners_own_language() {
    let (db, list) = fixture("en", "en").await;
    owner_language_scenario(&db, &list).await;
}

/// The per-recipient language query joins members, addresses and three
/// preference layers; this runs it against an isolated `PostgreSQL` schema.
#[tokio::test]
#[ignore = "requires explicit disposable TEST_POSTGRES_URL; uses own schema"]
async fn postgres_notice_language_contract() {
    sqlx::any::install_default_drivers();
    let url = std::env::var("TEST_POSTGRES_URL").expect("explicit fixture database required");
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("notice_language_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let isolated = format!(
        "{url}{}options=-csearch_path%3D{schema}",
        if url.contains('?') { '&' } else { '?' }
    );
    let result = tokio::spawn(async move {
        let db = Database::connect(&isolated, 2)
            .await
            .unwrap()
            .with_default_language("en");
        let (db, list) = fixture_on(db, "en").await;
        owner_language_scenario(&db, &list).await;
    })
    .await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
    result.unwrap();
}

async fn owner_language_scenario(db: &Database, list: &ListId) {
    let (db, list) = (db, list.clone());
    let owner_vi = subscribe(db, &list, "owner-vi@example.invalid", MemberRole::Owner).await;
    subscribe(db, &list, "owner-en@example.invalid", MemberRole::Owner).await;
    db.preferences()
        .set_member(
            owner_vi,
            listmngr_core::Preferences {
                preferred_language: Some("vi-VN".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    db.lists()
        .update(&list, &json!({"respond_to_post_requests": false}))
        .await
        .unwrap();
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: RAW.to_vec(),
                external_id: "<held@example.invalid>".into(),
                context: json!({"list_id": list, "envelope_sender": "author@elsewhere.invalid"})
                    .to_string(),
                queue: Queue::In,
                max_attempts: 3,
            },
            100,
        )
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::In, "worker", 100, 100)
        .await
        .unwrap()
        .unwrap();
    db.moderation()
        .hold(
            &lease,
            &list,
            "author@elsewhere.invalid",
            "private subject",
            "reason",
            101,
        )
        .await
        .unwrap();
    let notices = notices(db).await;
    assert_eq!(notices.len(), 2, "{notices:?}");
    let vi = notices
        .iter()
        .find(|(to, _, _)| to == "owner-vi@example.invalid")
        .unwrap();
    assert_eq!(
        vi.1,
        "Bài gửi tới dev@example.invalid từ author@elsewhere.invalid cần được duyệt"
    );
    assert!(vi.2.contains("đang được giữ lại"), "{}", vi.2);
    let en = notices
        .iter()
        .find(|(to, _, _)| to == "owner-en@example.invalid")
        .unwrap();
    assert_eq!(
        en.1,
        "dev@example.invalid post from author@elsewhere.invalid requires approval"
    );
}

#[tokio::test]
async fn unsupported_languages_fall_back_to_the_site_default_then_english() {
    let (db, list) = fixture("fr", "vi").await;
    subscribe(&db, &list, "member@example.invalid", MemberRole::Member).await;
    let notices = notices(&db).await;
    assert_eq!(notices.len(), 1);
    // `fr` is not shipped; the site default `vi` is.
    assert_eq!(
        notices[0].1,
        "Chào mừng bạn đến với hộp thư chung \"Dev Chat\""
    );

    let (db, list) = fixture("fr", "de").await;
    subscribe(&db, &list, "member@example.invalid", MemberRole::Member).await;
    let english = self::notices(&db).await;
    assert_eq!(
        english[0].1, "Welcome to the \"Dev Chat\" mailing list",
        "neither list nor site language ships, so English"
    );
}
