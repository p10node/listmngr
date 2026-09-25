//! Webhooks: created with a secret shown once, matched to the audit
//! events they subscribe to, and owed one delivery per event in the
//! transaction that records it.
use listmngr_core::{ListId, MemberRole, SubscriptionMode, WebhookId};
use listmngr_db::mail_queue::{NewMessage, Queue};
use listmngr_db::{
    AuditContext, Database, DeliveryState, NewList, NewMember, NewWebhook, WebhookPatch,
};
use serde_json::json;

/// Thirty-two characters, the least a signing key may be.
const KEY: &str = "0123456789abcdef0123456789abcdef";
const DEV: &str = "dev.example.invalid";
const OTHER: &str = "other.example.invalid";

async fn fixture_on(db: Database) -> Database {
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    for (id, name) in [(DEV, "Dev"), (OTHER, "Other")] {
        db.lists()
            .create(NewList {
                list_id: id.parse().unwrap(),
                display_name: name.into(),
                style: "legacy-default".into(),
            })
            .await
            .unwrap();
    }
    db
}

async fn fixture() -> Database {
    let db = Database::connect("sqlite::memory:", 1)
        .await
        .unwrap()
        .with_webhooks(Some(KEY), false);
    fixture_on(db).await
}

fn hook(url: &str, events: &[&str], list: Option<&str>) -> NewWebhook {
    NewWebhook {
        url: url.into(),
        events: events.iter().map(|event| (*event).to_owned()).collect(),
        list_id: list.map(|list| list.parse().unwrap()),
        description: "for the test".into(),
    }
}

fn dev() -> ListId {
    DEV.parse().unwrap()
}

async fn audit(db: &Database, action: &str) -> Vec<String> {
    sqlx::query_scalar("SELECT diff FROM audit_log WHERE action=$1 ORDER BY at")
        .bind(action)
        .fetch_all(db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn a_webhook_has_a_secret_shown_once_that_only_the_site_can_derive_again() {
    let db = fixture().await;
    let repo = db.webhooks();
    let (webhook, secret) = repo
        .create_with_context(
            hook("https://hooks.example.invalid/site", &["*"], None),
            &AuditContext::system(),
        )
        .await
        .unwrap();
    assert_eq!(secret.len(), 64);
    assert!(secret.bytes().all(|b| b.is_ascii_hexdigit()), "{secret}");
    assert_eq!(webhook.url, "https://hooks.example.invalid/site");
    assert_eq!(webhook.events, ["*"]);
    assert_eq!(webhook.list_id, None);
    assert!(webhook.enabled);
    assert_eq!(webhook.secret_fingerprint.len(), 8);
    assert_eq!(webhook.description, "for the test");
    // The secret is not stored: it is derived again from the key and the
    // webhook's salt, and never reaches the audit log.
    assert_eq!(repo.secret(webhook.id).await.unwrap(), secret);
    let created = audit(&db, "webhook.create").await;
    assert_eq!(created.len(), 1);
    assert!(!created[0].contains(&secret), "{}", created[0]);
    assert!(
        created[0].contains("hooks.example.invalid"),
        "{}",
        created[0]
    );
    assert_eq!(repo.get(webhook.id).await.unwrap(), webhook);
    assert_eq!(repo.list(None).await.unwrap(), [webhook.clone()]);
    // Rotating gives a new secret and a new fingerprint.
    let rotated = repo
        .rotate_with_context(webhook.id, &AuditContext::system())
        .await
        .unwrap();
    assert_ne!(rotated, secret);
    assert_eq!(repo.secret(webhook.id).await.unwrap(), rotated);
    let after = repo.get(webhook.id).await.unwrap();
    assert_ne!(after.secret_fingerprint, webhook.secret_fingerprint);
    assert!(
        audit(&db, "webhook.rotate").await[0].contains(&webhook.secret_fingerprint),
        "the old fingerprint is what the audit names"
    );
    // A patch changes what it names, audited from → to.
    let patched = repo
        .update_with_context(
            webhook.id,
            WebhookPatch {
                enabled: Some(false),
                events: Some(vec!["member.*".into(), "list.config".into()]),
                ..WebhookPatch::default()
            },
            &AuditContext::system(),
        )
        .await
        .unwrap();
    assert!(!patched.enabled);
    assert_eq!(patched.events, ["member.*", "list.config"]);
    assert_eq!(patched.url, webhook.url);
    let updated = audit(&db, "webhook.update").await;
    assert!(updated[0].contains("\"enabled\""), "{}", updated[0]);
    assert!(!updated[0].contains("\"url\""), "{}", updated[0]);
    // Without a signing key nothing can be created or signed.
    let keyless = fixture_on(Database::connect("sqlite::memory:", 1).await.unwrap()).await;
    assert!(matches!(
        keyless
            .webhooks()
            .create_with_context(
                hook("https://hooks.example.invalid/x", &["*"], None),
                &AuditContext::system()
            )
            .await,
        Err(listmngr_core::Error::Validation(_))
    ));
    // Deleting takes the deliveries with it.
    repo.ping_with_context(webhook.id, &AuditContext::system())
        .await
        .unwrap();
    repo.delete_with_context(webhook.id, &AuditContext::system())
        .await
        .unwrap();
    assert!(matches!(
        repo.get(webhook.id).await,
        Err(listmngr_core::Error::NotFound(_))
    ));
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM webhook_deliveries")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(left, 0);
    assert!(matches!(
        repo.secret(WebhookId::new()).await,
        Err(listmngr_core::Error::NotFound(_))
    ));
}

#[tokio::test]
async fn a_webhook_is_refused_what_it_cannot_take() {
    let db = fixture().await;
    let repo = db.webhooks();
    let refused = |new: NewWebhook| async {
        matches!(
            repo.create_with_context(new, &AuditContext::system()).await,
            Err(listmngr_core::Error::Validation(_))
        )
    };
    assert!(refused(hook("http://hooks.example.invalid/x", &["*"], None)).await);
    assert!(refused(hook("https://a:b@hooks.example.invalid/x", &["*"], None)).await);
    assert!(refused(hook("hooks.example.invalid", &["*"], None)).await);
    assert!(refused(hook("https://hooks.example.invalid/x", &[], None)).await);
    assert!(refused(hook("https://hooks.example.invalid/x", &["Member.*"], None)).await);
    assert!(refused(hook("https://hooks.example.invalid/x", &["mem ber"], None)).await);
    assert!(matches!(
        repo.create_with_context(
            hook(
                "https://hooks.example.invalid/x",
                &["*"],
                Some("nobody.example.invalid")
            ),
            &AuditContext::system()
        )
        .await,
        Err(listmngr_core::Error::NotFound(_))
    ));
    // With `allow_http`, a lab target is taken.
    let lab = fixture_on(
        Database::connect("sqlite::memory:", 1)
            .await
            .unwrap()
            .with_webhooks(Some(KEY), true),
    )
    .await;
    assert!(
        lab.webhooks()
            .create_with_context(
                hook("http://127.0.0.1:1/x", &["*"], None),
                &AuditContext::system()
            )
            .await
            .is_ok()
    );
}

async fn create(db: &Database, new: NewWebhook) -> listmngr_db::Webhook {
    db.webhooks()
        .create_with_context(new, &AuditContext::system())
        .await
        .unwrap()
        .0
}

/// A webhook's deliveries as `(event, list)`, sorted.
async fn events(db: &Database, id: WebhookId) -> Vec<(String, Option<String>)> {
    let mut events: Vec<(String, Option<String>)> = db
        .webhooks()
        .deliveries(id, 100)
        .await
        .unwrap()
        .into_iter()
        .map(|delivery| (delivery.event, delivery.list_id))
        .collect();
    events.sort();
    events
}

#[allow(clippy::too_many_lines, clippy::cognitive_complexity)] // One ordered contract per backend.
async fn scenario(db: &Database) {
    let repo = db.webhooks();
    // A hook on everything hears of its own creation and of the hooks
    // created after it — the site's own audit chatter, `webhook.*` and
    // `tasks.*` included — so the site hook below subscribes to what a
    // list does.
    let off = create(db, hook("https://hooks.example.invalid/off", &["*"], None)).await;
    let site_all = create(
        db,
        hook(
            "https://hooks.example.invalid/all",
            &["list.*", "member.*", "moderation.*"],
            None,
        ),
    )
    .await;
    let dev_members = create(
        db,
        hook(
            "https://hooks.example.invalid/dev",
            &["member.*"],
            Some(DEV),
        ),
    )
    .await;
    let other_lists = create(
        db,
        hook(
            "https://hooks.example.invalid/other",
            &["list.*"],
            Some(OTHER),
        ),
    )
    .await;
    assert_eq!(
        events(db, off.id).await.len(),
        4,
        "its own and one each after"
    );
    repo.update_with_context(
        off.id,
        WebhookPatch {
            enabled: Some(false),
            ..WebhookPatch::default()
        },
        &AuditContext::system(),
    )
    .await
    .unwrap();
    assert_eq!(events(db, off.id).await.len(), 4, "disabled: nothing more");
    sqlx::query("DELETE FROM webhook_deliveries")
        .execute(db.pool())
        .await
        .unwrap();
    // A list setting: the site hook, and the hook bound to that list
    // when the event is one it subscribes to.
    db.lists()
        .update(&dev(), &json!({"description": "changed"}))
        .await
        .unwrap();
    // A member: the site hook and the dev members hook.
    let member = db
        .members()
        .create(NewMember {
            list_id: dev(),
            email: "alice@example.invalid".into(),
            display_name: String::new(),
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsAddress,
        })
        .await
        .unwrap();
    // The other list's setting: the site hook and the other list's hook.
    db.lists()
        .update(&OTHER.parse().unwrap(), &json!({"description": "changed"}))
        .await
        .unwrap();
    // A held post, audited through moderation's own path: the site hook,
    // with the list the held message belongs to.
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: b"From: stranger@elsewhere.invalid\r\nTo: dev@example.invalid\r\nSubject: hold me\r\n\r\nbody".to_vec(),
                external_id: "<held@example.invalid>".into(),
                context: json!({"list_id": DEV, "envelope_sender": "stranger@elsewhere.invalid"})
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
            &dev(),
            "stranger@elsewhere.invalid",
            "hold me",
            "nonmember",
            101,
        )
        .await
        .unwrap();
    let dev_list = Some(DEV.to_owned());
    let other_list = Some(OTHER.to_owned());
    assert_eq!(
        events(db, site_all.id).await,
        [
            ("list.config".to_owned(), dev_list.clone()),
            ("list.config".to_owned(), other_list.clone()),
            ("member.create".to_owned(), dev_list.clone()),
            ("moderation.hold".to_owned(), dev_list.clone()),
        ]
    );
    assert_eq!(
        events(db, dev_members.id).await,
        [("member.create".to_owned(), dev_list.clone())]
    );
    assert_eq!(
        events(db, other_lists.id).await,
        [("list.config".to_owned(), other_list)]
    );
    assert!(events(db, off.id).await.is_empty());
    // What a delivery carries: the event, when, what it is about, who did
    // it, and the audit diff — redacted as the audit log is.
    let delivery = repo.deliveries(dev_members.id, 1).await.unwrap().remove(0);
    assert_eq!(delivery.state, DeliveryState::Pending);
    assert_eq!(delivery.attempts, 0);
    assert_eq!(delivery.next_attempt_at, delivery.created_at);
    assert_eq!(delivery.last_status, None);
    let payload = &delivery.payload;
    assert_eq!(payload["id"], delivery.id);
    assert_eq!(payload["event"], "member.create");
    assert_eq!(payload["list_id"], DEV);
    assert_eq!(payload["target"]["type"], "member");
    assert_eq!(payload["target"]["id"], member.id.to_string());
    assert_eq!(payload["actor"]["user_id"], serde_json::Value::Null);
    assert_eq!(payload["data"]["list_id"], DEV);
    assert_eq!(payload["data"]["role"], "member");
    assert!(payload["at"].as_str().unwrap().contains('T'));
    // A ping is a delivery like any other.
    let ping = repo
        .ping_with_context(site_all.id, &AuditContext::system())
        .await
        .unwrap();
    assert_eq!(ping.event, "ping");
    assert_eq!(ping.payload["event"], "ping");
    assert_eq!(repo.delivery(&ping.id).await.unwrap(), ping);
    // The sweep collects deliveries posted or given up longer ago than
    // the retention, and leaves the pending ones.
    let old = 1_000;
    sqlx::query(
        "UPDATE webhook_deliveries SET state='delivered', finished_at=$1 WHERE event='list.config'",
    )
    .bind(old)
    .execute(db.pool())
    .await
    .unwrap();
    let summary = db.tasks().sweep(old + 10_000, 5_000).await.unwrap();
    assert_eq!(summary.collected_webhook_deliveries, 3, "{summary:?}");
    let pending: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM webhook_deliveries WHERE state='pending'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(pending, 4, "member.create ×2, moderation.hold, ping");
    // A list's webhooks go with the list; the site hook hears of it.
    db.lists().delete(&OTHER.parse().unwrap()).await.unwrap();
    assert!(matches!(
        repo.get(other_lists.id).await,
        Err(listmngr_core::Error::NotFound(_))
    ));
    assert_eq!(repo.list(None).await.unwrap().len(), 3);
    assert!(
        events(db, site_all.id)
            .await
            .iter()
            .any(|(event, _)| event == "list.delete"),
    );
}

#[tokio::test]
async fn events_fan_out_to_the_webhooks_that_subscribe_in_the_writes_transaction() {
    let db = fixture().await;
    scenario(&db).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_webhooks_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webhooks")
        .await
        .unwrap();
    let db = fixture_on(
        Database::connect(&schema.url, 2)
            .await
            .unwrap()
            .with_webhooks(Some(KEY), false),
    )
    .await;
    scenario(&db).await;
    db.pool().close().await;
    schema.drop().await.unwrap();
}
