//! The webhook runner against a target of the test's own: a delivery is
//! posted once, signed, and recorded as the target answered; refusals
//! back off and are given up; what the site may not reach is refused
//! before a connection.
use axum::{Router, extract::State, http::HeaderMap, http::StatusCode, routing::post};
use hmac::{Hmac, Mac};
use listmngr_core::WebhooksConfig;
use listmngr_db::webhooks::Outcome;
use listmngr_db::{AuditContext, Database, DeliveryState, NewWebhook, WebhookPatch};
use listmngr_runners::webhooks::{deliver_due, signature};
use sha2::Sha256;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const KEY: &str = "0123456789abcdef0123456789abcdef";

/// What the target received: headers and body, one entry per request.
type Received = Arc<Mutex<Vec<(HeaderMap, String)>>>;

async fn target(status: StatusCode) -> (u16, Received) {
    let received: Received = Arc::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let state = received.clone();
    let app = Router::new()
        .route(
            "/hook",
            post(
                move |State(received): State<Received>, headers: HeaderMap, body: String| async move {
                    received.lock().unwrap().push((headers, body));
                    status
                },
            ),
        )
        .with_state(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (port, received)
}

fn config() -> WebhooksConfig {
    WebhooksConfig {
        enabled: true,
        signing_key: Some(KEY.into()),
        allow_http: true,
        allow_private_targets: true,
        max_attempts: 2,
        timeout_secs: 2,
        ..WebhooksConfig::default()
    }
}

async fn fixture_on(db: Database) -> Database {
    db.migrate().await.unwrap();
    db
}

async fn fixture() -> Database {
    fixture_on(
        Database::connect("sqlite::memory:", 1)
            .await
            .unwrap()
            .with_webhooks(Some(KEY), true),
    )
    .await
}

async fn hook(db: &Database, port: u16) -> listmngr_db::Webhook {
    db.webhooks()
        .create_with_context(
            NewWebhook {
                url: format!("http://127.0.0.1:{port}/hook"),
                // Not `*`: that would hear of its own creation first.
                events: vec!["domain.*".into()],
                list_id: None,
                description: String::new(),
            },
            &AuditContext::system(),
        )
        .await
        .unwrap()
        .0
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers.get(name).unwrap().to_str().unwrap()
}

#[tokio::test]
async fn a_due_delivery_is_posted_once_signed_and_marked_delivered() {
    let db = fixture().await;
    let (port, received) = target(StatusCode::NO_CONTENT).await;
    let webhook = hook(&db, port).await;
    let ping = db
        .webhooks()
        .ping_with_context(webhook.id, &AuditContext::system())
        .await
        .unwrap();
    let before = listmngr_core::metrics::global()
        .webhook_deliveries
        .get("delivered");
    let (delivery, outcome) = deliver_due(&db, &config()).await.unwrap().unwrap();
    assert_eq!(outcome, Outcome::Delivered { status: 204 });
    assert_eq!(delivery.id, ping.id);
    assert_eq!(delivery.state, DeliveryState::Delivered);
    assert_eq!(delivery.attempts, 1);
    assert_eq!(delivery.last_status, Some(204));
    assert!(delivery.finished_at.is_some());
    assert!(
        listmngr_core::metrics::global()
            .webhook_deliveries
            .get("delivered")
            > before
    );
    // What the target got: the payload as JSON, named, dated and signed
    // so the receiver can check it with the secret it was shown.
    let requests = received.lock().unwrap().clone();
    assert_eq!(requests.len(), 1);
    let (headers, body) = &requests[0];
    assert_eq!(header(headers, "content-type"), "application/json");
    assert_eq!(header(headers, "user-agent"), "listmngr");
    assert_eq!(header(headers, "x-listmngr-event"), "ping");
    assert_eq!(header(headers, "x-listmngr-delivery"), ping.id);
    assert_eq!(
        header(headers, "x-listmngr-webhook"),
        webhook.id.to_string()
    );
    let sent: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(sent, ping.payload);
    let timestamp: i64 = header(headers, "x-listmngr-timestamp").parse().unwrap();
    assert!((chrono::Utc::now().timestamp() - timestamp).abs() < 60);
    let secret = db.webhooks().secret(webhook.id).await.unwrap();
    assert_eq!(
        header(headers, "x-listmngr-signature"),
        signature(&secret, timestamp, body)
    );
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(format!("{timestamp}.{body}").as_bytes());
    let expected = mac
        .finalize()
        .into_bytes()
        .iter()
        .fold(String::new(), |mut out, byte| {
            use std::fmt::Write as _;
            let _ = write!(out, "{byte:02x}");
            out
        });
    assert_eq!(
        header(headers, "x-listmngr-signature"),
        format!("sha256={expected}")
    );
    // Nothing else is due; nothing was posted twice.
    assert!(deliver_due(&db, &config()).await.unwrap().is_none());
    assert_eq!(received.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_refusal_backs_off_and_is_given_up_after_the_attempts_allowed() {
    let db = fixture().await;
    let (port, received) = target(StatusCode::INTERNAL_SERVER_ERROR).await;
    let webhook = hook(&db, port).await;
    let ping = db
        .webhooks()
        .ping_with_context(webhook.id, &AuditContext::system())
        .await
        .unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    let (delivery, outcome) = deliver_due(&db, &config()).await.unwrap().unwrap();
    let Outcome::Retry {
        status,
        next_attempt_at,
        ..
    } = outcome
    else {
        panic!("{outcome:?}");
    };
    assert_eq!(status, Some(500));
    assert_eq!(delivery.state, DeliveryState::Pending);
    assert_eq!(delivery.attempts, 1);
    assert_eq!(delivery.last_status, Some(500));
    assert_eq!(delivery.last_error.as_deref(), Some("HTTP 500"));
    // Ten seconds, jittered by up to a fifth.
    assert!(
        (now + 7_000..=now + 13_000).contains(&next_attempt_at),
        "{next_attempt_at} vs {now}"
    );
    assert!(
        deliver_due(&db, &config()).await.unwrap().is_none(),
        "not due yet"
    );
    sqlx::query("UPDATE webhook_deliveries SET next_attempt_at=0 WHERE id=$1")
        .bind(&ping.id)
        .execute(db.pool())
        .await
        .unwrap();
    let (delivery, outcome) = deliver_due(&db, &config()).await.unwrap().unwrap();
    assert_eq!(
        outcome,
        Outcome::Failed {
            status: Some(500),
            error: "HTTP 500".into()
        }
    );
    assert_eq!(delivery.state, DeliveryState::Failed);
    assert_eq!(delivery.attempts, 2);
    assert!(delivery.finished_at.is_some());
    assert_eq!(received.lock().unwrap().len(), 2);
    assert!(deliver_due(&db, &config()).await.unwrap().is_none());
    // A redirect is not followed: the target answered, and not with 2xx.
    let (port, received) = target(StatusCode::FOUND).await;
    let webhook = hook(&db, port).await;
    db.webhooks()
        .ping_with_context(webhook.id, &AuditContext::system())
        .await
        .unwrap();
    let (delivery, _) = deliver_due(&db, &config()).await.unwrap().unwrap();
    assert_eq!(delivery.last_status, Some(302));
    assert_eq!(delivery.state, DeliveryState::Pending);
    assert_eq!(received.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn what_the_site_may_not_reach_is_refused_before_a_connection() {
    let db = fixture().await;
    let (port, received) = target(StatusCode::OK).await;
    let webhook = hook(&db, port).await;
    db.webhooks()
        .ping_with_context(webhook.id, &AuditContext::system())
        .await
        .unwrap();
    // A loopback target, with private targets not allowed: given up at
    // once, nothing connected.
    let strict = WebhooksConfig {
        allow_private_targets: false,
        ..config()
    };
    let (delivery, outcome) = deliver_due(&db, &strict).await.unwrap().unwrap();
    assert!(
        matches!(&outcome, Outcome::Failed { status: None, error } if error.contains("private")),
        "{outcome:?}"
    );
    assert_eq!(delivery.state, DeliveryState::Failed);
    assert!(received.lock().unwrap().is_empty());
    // An `http://` target once `allow_http` is off: the same.
    db.webhooks()
        .ping_with_context(webhook.id, &AuditContext::system())
        .await
        .unwrap();
    let https_only = WebhooksConfig {
        allow_http: false,
        ..config()
    };
    let (_, outcome) = deliver_due(&db, &https_only).await.unwrap().unwrap();
    assert!(
        matches!(&outcome, Outcome::Failed { error, .. } if error.contains("scheme")),
        "{outcome:?}"
    );
    assert!(received.lock().unwrap().is_empty());
    // A name nobody resolves: transient, tried again later.
    let (unresolvable, _) = db
        .webhooks()
        .create_with_context(
            NewWebhook {
                url: "https://hooks.nowhere.invalid/hook".into(),
                events: vec!["domain.*".into()],
                list_id: None,
                description: String::new(),
            },
            &AuditContext::system(),
        )
        .await
        .unwrap();
    db.webhooks()
        .ping_with_context(unresolvable.id, &AuditContext::system())
        .await
        .unwrap();
    let (delivery, outcome) = deliver_due(&db, &config()).await.unwrap().unwrap();
    assert!(
        matches!(&outcome, Outcome::Retry { status: None, error, .. } if error.contains("resolve")),
        "{outcome:?}"
    );
    assert_eq!(delivery.state, DeliveryState::Pending);
}

#[tokio::test]
async fn a_disabled_webhook_keeps_its_deliveries_until_it_is_enabled_again() {
    let db = fixture().await;
    let (port, received) = target(StatusCode::OK).await;
    let webhook = hook(&db, port).await;
    db.webhooks()
        .ping_with_context(webhook.id, &AuditContext::system())
        .await
        .unwrap();
    let patch = |enabled| WebhookPatch {
        enabled: Some(enabled),
        ..WebhookPatch::default()
    };
    db.webhooks()
        .update_with_context(webhook.id, patch(false), &AuditContext::system())
        .await
        .unwrap();
    assert!(deliver_due(&db, &config()).await.unwrap().is_none());
    assert!(received.lock().unwrap().is_empty());
    db.webhooks()
        .update_with_context(webhook.id, patch(true), &AuditContext::system())
        .await
        .unwrap();
    let (delivery, _) = deliver_due(&db, &config()).await.unwrap().unwrap();
    assert_eq!(delivery.state, DeliveryState::Delivered);
    assert_eq!(received.lock().unwrap().len(), 1);
}

/// The runner itself: started with the service, it posts what becomes
/// due and stops when told.
#[tokio::test]
async fn the_webhook_runner_posts_on_its_own_until_shutdown() {
    let db = fixture().await;
    let (port, received) = target(StatusCode::OK).await;
    let webhook = hook(&db, port).await;
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let runner = tokio::spawn(listmngr_runners::webhooks::run(
        db.clone(),
        config(),
        shutdown_rx,
    ));
    // An audited write is an event: the domain the site gains.
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let posted = received.lock().unwrap().len();
        if posted >= 1 {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "nothing posted");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (headers, _) = received.lock().unwrap()[0].clone();
    assert_eq!(header(&headers, "x-listmngr-event"), "domain.create");
    shutdown_tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), runner)
        .await
        .expect("the runner stops on shutdown")
        .unwrap();
    // The runner recorded what it posted before it stopped.
    let deliveries = db.webhooks().deliveries(webhook.id, 10).await.unwrap();
    assert!(
        deliveries
            .iter()
            .any(|delivery| delivery.event == "domain.create"
                && delivery.state == DeliveryState::Delivered),
        "{deliveries:?}"
    );
}

/// Two claims on PostgreSQL, where `FOR UPDATE SKIP LOCKED` keeps two
/// runners off one delivery.
#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_webhook_claim_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webhook_claim")
        .await
        .unwrap();
    let db = fixture_on(
        Database::connect(&schema.url, 4)
            .await
            .unwrap()
            .with_webhooks(Some(KEY), true),
    )
    .await;
    let (port, received) = target(StatusCode::OK).await;
    let webhook = hook(&db, port).await;
    for _ in 0..2 {
        db.webhooks()
            .ping_with_context(webhook.id, &AuditContext::system())
            .await
            .unwrap();
    }
    let now = chrono::Utc::now().timestamp_millis();
    let repo = db.webhooks();
    let (left, right) = tokio::join!(repo.claim_due(now, 60_000), repo.claim_due(now, 60_000));
    let (left, _) = left.unwrap().unwrap();
    let (right, _) = right.unwrap().unwrap();
    assert_ne!(left.id, right.id, "each runner its own delivery");
    assert!(
        db.webhooks()
            .claim_due(now, 60_000)
            .await
            .unwrap()
            .is_none()
    );
    for claimed in [&left, &right] {
        db.webhooks()
            .record(&claimed.id, &Outcome::Delivered { status: 200 }, now)
            .await
            .unwrap();
    }
    let _ = received;
    db.pool().close().await;
    schema.drop().await.unwrap();
}
