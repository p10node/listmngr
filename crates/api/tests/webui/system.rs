//! The server owner's system page — versions, redacted configuration, queue
//! and runner status, MTA map status — and the audit log viewer with its
//! filters and paging.
use super::{call, login_as, seeded_fixture_configured, text, user};
use axum::http::StatusCode;
use listmngr_core::Config;
use listmngr_db::Database;
use listmngr_db::mail_queue::{NewMessage, Queue};

#[tokio::test]
async fn system_page_and_audit_log() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    matrix(db).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_system_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_system")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 3).await.unwrap();
    db.migrate().await.unwrap();
    matrix(db).await;
    schema.drop().await.unwrap();
}

async fn page(app: &axum::Router, path: &str, cookie: &str) -> String {
    let response = call(app, "GET", path, cookie, "").await;
    assert_eq!(response.status(), StatusCode::OK, "{path}");
    text(response).await
}

fn has(html: &str, needle: &str) {
    assert!(html.contains(needle), "expected {needle:?} in: {html}");
}

fn lacks(html: &str, needle: &str) {
    assert!(!html.contains(needle), "unexpected {needle:?} in: {html}");
}

async fn matrix(db: Database) {
    let maps = tempfile::tempdir().unwrap();
    let map_directory = maps.path().to_string_lossy().into_owned();
    let (db, app) = seeded_fixture_configured(db, |config: &mut Config| {
        config.mta.incoming = "postfix".into();
        config.mta.map_directory = map_directory;
        config.mta.smtp_auth_password = Some("hunter2-not-for-pages".into());
    })
    .await;
    let root = user(&db, "root@example.com", true).await;
    user(&db, "plain@example.com", false).await;
    let root_cookie = login_as(&app, "root@example.com").await;
    let plain = login_as(&app, "plain@example.com").await;

    for path in ["/web/admin/system", "/web/admin/system/audit"] {
        assert_eq!(
            call(&app, "GET", path, "", "").await.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(&app, "GET", path, &plain, "").await.status(),
            StatusCode::FORBIDDEN
        );
    }
    has(
        &page(&app, "/web/admin", &root_cookie).await,
        "/web/admin/system",
    );

    system(&db, &app, &root_cookie).await;
    audit(&db, &app, &root_cookie, &root.id.to_string()).await;
}

/// Versions, the redacted configuration, the queues with a seeded job, the
/// runner that leased it, and the MTA map status without a generation.
async fn system(db: &Database, app: &axum::Router, root: &str) {
    let html = page(app, "/web/admin/system", root).await;
    has(&html, env!("CARGO_PKG_VERSION"));
    has(&html, "<dd>sqlite</dd>");
    has(&html, "[REDACTED]");
    lacks(&html, "sqlite::memory:");
    lacks(&html, "hunter2");
    has(&html, "<code>mta.incoming</code></td><td>postfix</td>");
    has(&html, "No job is waiting");
    has(&html, "No runner holds a lease");
    has(&html, "No generation published yet");
    has(&html, "<code>in</code></td><td>0</td>");
    let job = db
        .mail_queue()
        .enqueue(
            NewMessage {
                raw: b"From: a@example.com\r\nSubject: s\r\n\r\nbody\r\n".to_vec(),
                external_id: "<system-seed@example.invalid>".into(),
                context: serde_json::json!({"version":1,"list_id":"public.example.com","envelope_sender":"a@example.com"}).to_string(),
                queue: Queue::In,
                max_attempts: 3,
            },
            chrono::Utc::now().timestamp_millis() - 30_000,
        )
        .await
        .unwrap();
    let html = page(app, "/web/admin/system", root).await;
    has(&html, "<code>in</code></td><td>1</td>");
    has(&html, "has waited");
    lacks(&html, "No job is waiting");
    let now = chrono::Utc::now().timestamp_millis();
    let leased = db
        .mail_queue()
        .claim(Queue::In, "in-runner-1", now, 60_000)
        .await
        .unwrap();
    assert_eq!(leased.map(|leased| leased.job.id), Some(job.id));
    let html = page(app, "/web/admin/system", root).await;
    has(&html, "<code>in</code></td><td>0</td><td>1</td>");
    has(&html, "<code>in-runner-1</code></dt><dd>1</dd>");
    lacks(&html, "No runner holds a lease");
}

/// The audit viewer filters by action prefix and target, escapes diffs,
/// pages newest first and refuses an oversized filter.
async fn audit(db: &Database, app: &axum::Router, root: &str, root_id: &str) {
    db.users()
        .update(
            root_id.parse().unwrap(),
            &serde_json::json!({"display_name": "Root <b>"}),
        )
        .await
        .unwrap();
    let html = page(app, "/web/admin/system/audit", root).await;
    has(&html, "<code>list.create</code>");
    has(&html, "<code>user.update</code>");
    has(&html, "Root &lt;b&gt;");
    lacks(&html, "Root <b>");
    has(&html, "<td>system</td>");
    let first = html.find("user.update").unwrap();
    let created = html.find("list.create").unwrap();
    assert!(first < created, "newest first");
    let html = page(app, "/web/admin/system/audit?action=list.", root).await;
    has(&html, "<code>list.create</code>");
    lacks(&html, "<code>user.update</code>");
    has(&html, "public.example.com");
    let html = page(
        app,
        "/web/admin/system/audit?action=list.&target=private",
        root,
    )
    .await;
    has(&html, "private.example.com");
    lacks(&html, "list public.example.com");
    let html = page(app, "/web/admin/system/audit?action=nothing.", root).await;
    has(&html, "No audit events match");
    for query in [
        format!("action={}", "x".repeat(101)),
        "page=10001".into(),
        "unknown=1".into(),
    ] {
        assert_eq!(
            call(
                app,
                "GET",
                &format!("/web/admin/system/audit?{query}"),
                root,
                ""
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST,
            "{query}"
        );
    }
    let html = page(app, "/web/admin/system/audit?action=user.", root).await;
    has(&html, root_id);
}
