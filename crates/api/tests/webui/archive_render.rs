//! The archive as readers see it: sender and date on each post, a thread
//! laid out as a tree, stored attachments served as downloads, quoted
//! runs folded, Markdown rendered through the safe subset, addresses
//! obfuscated for visitors, the owner's reattach form, and the opt-in
//! avatar proxy.
use super::{call, csrf, login_as, member, seeded_fixture_configured, text, user};
use axum::http::StatusCode;
use listmngr_core::{Config, ListId, MemberRole};
use listmngr_db::Database;
use listmngr_db::mail_queue::{NewMessage, Queue};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[tokio::test]
async fn archive_rendering_threads_attachments_and_avatars() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    matrix(db).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_archive_render_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_archive_render")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 3).await.unwrap();
    db.migrate().await.unwrap();
    matrix(db).await;
    schema.drop().await.unwrap();
}

fn has(html: &str, needle: &str) {
    assert!(html.contains(needle), "expected {needle:?} in: {html}");
}

fn lacks(html: &str, needle: &str) {
    assert!(!html.contains(needle), "unexpected {needle:?} in: {html}");
}

async fn page(app: &axum::Router, path: &str, cookie: &str) -> String {
    let response = call(app, "GET", path, cookie, "").await;
    assert_eq!(response.status(), StatusCode::OK, "{path}");
    text(response).await
}

const LIST: &str = "public.example.com";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\nfake";

/// A stand-in for gravatar.com: one PNG for every hash, counting hits.
async fn gravatar_mock() -> (String, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let app = axum::Router::new().route(
        "/avatar/{hash}",
        axum::routing::get(move || {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                ([(axum::http::header::CONTENT_TYPE, "image/png")], PNG)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://127.0.0.1:{port}/avatar/"), hits)
}

/// Index one message through the archive runner, as the pipeline would.
async fn archived(db: &Database, id: &str, raw: &str) -> String {
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: raw.replace('\n', "\r\n").into_bytes(),
                external_id: id.into(),
                context: serde_json::json!({"version":1,"list_id":LIST}).to_string(),
                queue: Queue::Archive,
                max_attempts: 3,
            },
            100,
        )
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::Archive, "archive", 100, 1000)
        .await
        .unwrap()
        .unwrap();
    listmngr_archive::process(db, &lease, 101).await.unwrap();
    listmngr_mail::message_id_hash(id).unwrap()
}

struct Hashes {
    a: String,
    b: String,
    d: String,
    e: String,
}

async fn seed(db: &Database) -> Hashes {
    let list: ListId = LIST.parse().unwrap();
    db.lists()
        .update(&list, &serde_json::json!({"archive_policy": "public"}))
        .await
        .unwrap();
    let a = archived(db, "<a@example.org>", "Message-ID: <a@example.org>\nFrom: Alice <alice@example.org>\nDate: Mon, 01 Jan 2024 10:00:00 +0000\nSubject: Kickoff\nContent-Type: text/plain\n\nLet us begin at https://example.org/x.\n> quoted one\n> quoted two\nWrite to bob@example.org.\n").await;
    let b = archived(db, "<b@example.org>", "Message-ID: <b@example.org>\nIn-Reply-To: <a@example.org>\nReferences: <a@example.org>\nFrom: Bob <bob@example.org>\nDate: Mon, 01 Jan 2024 11:00:00 +0000\nSubject: Re: Kickoff\nContent-Type: multipart/mixed; boundary=\"b1\"\n\n--b1\nContent-Type: text/plain\n\nAgreed, see the notes.\n--b1\nContent-Type: text/plain; name=\"notes.txt\"\nContent-Disposition: attachment; filename=\"notes.txt\"\n\nhello notes\n--b1\nContent-Type: text/html; name=\"page.html\"\nContent-Disposition: attachment; filename=\"page.html\"\n\n<b>page</b>\n--b1--\n").await;
    let _c = archived(db, "<c@example.org>", "Message-ID: <c@example.org>\nReferences: <a@example.org> <b@example.org>\nFrom: Carol <carol@example.org>\nDate: Mon, 01 Jan 2024 12:00:00 +0000\nSubject: Re: Kickoff\nContent-Type: text/plain\n\nReferences only.\n").await;
    let d = archived(db, "<d@example.org>", "Message-ID: <d@example.org>\nIn-Reply-To: <a@example.org>\nFrom: Dave <dave@example.org>\nDate: Mon, 01 Jan 2024 09:30:00 +0000\nSubject: Re: Kickoff\nContent-Type: text/plain\n\nDated before Bob.\n").await;
    let e = archived(db, "<e@example.org>", "Message-ID: <e@example.org>\nFrom: Erin <erin@example.org>\nDate: Mon, 01 Jan 2024 13:00:00 +0000\nSubject: Markdown post\nContent-Type: text/plain\n\n**bold** <script>alert(1)</script> [bad](javascript:alert(1))\n").await;
    Hashes { a, b, d, e }
}

async fn matrix(db: Database) {
    let (gravatar_url, hits) = gravatar_mock().await;
    let (db, app) = seeded_fixture_configured(db, |config: &mut Config| {
        config.archive.gravatar = true;
        config.archive.gravatar_url = gravatar_url;
    })
    .await;
    let hashes = seed(&db).await;
    user(&db, "owner@example.com", false).await;
    member(&db, "owner@example.com", MemberRole::Owner).await;
    user(&db, "member@example.com", false).await;
    member(&db, "member@example.com", MemberRole::Member).await;
    let owner = login_as(&app, "owner@example.com").await;
    let reader = login_as(&app, "member@example.com").await;

    senders_and_bodies(&app, &reader).await;
    thread_tree(&app, &hashes).await;
    attachments(&app, &hashes).await;
    markdown(&db, &app, &hashes).await;
    reattach(&db, &app, &owner, &reader, &hashes).await;
    avatars(&db, &app, &hits).await;
}

/// The list view names each sender and date; a visitor sees addresses
/// obfuscated, a signed-in reader as written; quotes fold, URLs link.
async fn senders_and_bodies(app: &axum::Router, reader: &str) {
    let path = format!("/web/lists/{LIST}/archive");
    let html = page(app, &path, "").await;
    has(
        &html,
        "class=\"sender-name\" href=\"/web/lists/public.example.com/archive/senders/",
    );
    has(&html, ">Alice</a>");
    has(&html, "alice at example.org");
    lacks(&html, "alice@example.org");
    has(&html, "<time>2024-01-01 10:00 UTC</time>");
    has(
        &html,
        "<details class=\"quote\"><summary>2 quoted lines</summary>",
    );
    has(
        &html,
        "<a href=\"https://example.org/x\" rel=\"nofollow noopener\">",
    );
    has(&html, "bob at example.org");
    has(&html, "In reply to");
    assert_eq!(html.matches("<article class=\"post depth-0\"").count(), 5);
    let html = page(app, &path, reader).await;
    has(&html, "alice@example.org");
    has(&html, "bob@example.org");
}

/// One thread as a tree: replies under their parents, siblings by date.
async fn thread_tree(app: &axum::Router, hashes: &Hashes) {
    let html = page(
        app,
        &format!("/web/lists/{LIST}/archive?thread={}", hashes.a),
        "",
    )
    .await;
    let position = |hash: &str| html.find(&format!("id=\"m-{hash}\"")).expect(hash);
    let c = listmngr_mail::message_id_hash("<c@example.org>").unwrap();
    assert!(position(&hashes.a) < position(&hashes.d), "Dave before Bob");
    assert!(position(&hashes.d) < position(&hashes.b));
    assert!(position(&hashes.b) < position(&c));
    has(
        &html,
        &format!("class=\"post depth-0\" id=\"m-{}\"", hashes.a),
    );
    has(
        &html,
        &format!("class=\"post depth-1\" id=\"m-{}\"", hashes.d),
    );
    has(
        &html,
        &format!("class=\"post depth-1\" id=\"m-{}\"", hashes.b),
    );
    has(&html, &format!("class=\"post depth-2\" id=\"m-{c}\""));
    lacks(&html, &format!("id=\"m-{}\"", hashes.e));
    lacks(&html, "Next");
}

/// Stored attachments download from their own path, never as a document.
async fn attachments(app: &axum::Router, hashes: &Hashes) {
    let html = page(
        app,
        &format!("/web/lists/{LIST}/archive?message={}", hashes.b),
        "",
    )
    .await;
    let base = format!("/web/lists/{LIST}/archive/attachments/{}", hashes.b);
    has(&html, &format!("{base}/0"));
    has(&html, "Download attachment: notes.txt");
    let response = call(app, "GET", &format!("{base}/0"), "", "").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/plain");
    assert_eq!(
        response.headers()["content-disposition"],
        "attachment; filename=\"notes.txt\""
    );
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(text(response).await.trim(), "hello notes");
    let response = call(app, "GET", &format!("{base}/1"), "", "").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-type"],
        "application/octet-stream",
        "HTML is never served as a document"
    );
    assert_eq!(
        call(app, "GET", &format!("{base}/5"), "", "")
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            app,
            "GET",
            &format!("/web/lists/{LIST}/archive/attachments/nope/0"),
            "",
            ""
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
}

/// A list rendering Markdown gets the safe subset.
async fn markdown(db: &Database, app: &axum::Router, hashes: &Hashes) {
    let path = format!("/web/lists/{LIST}/archive?message={}", hashes.e);
    let html = page(app, &path, "").await;
    has(&html, "**bold**");
    db.lists()
        .update(
            &LIST.parse().unwrap(),
            &serde_json::json!({"archive_rendering_mode": "markdown"}),
        )
        .await
        .unwrap();
    let html = page(app, &path, "").await;
    has(&html, "<strong>bold</strong>");
    has(&html, "&lt;script&gt;alert(1)&lt;/script&gt;");
    lacks(&html, "<script>");
    lacks(&html, "javascript:");
}

/// The owner moves a post under another and its replies follow; a cycle
/// and a stranger are refused.
async fn reattach(db: &Database, app: &axum::Router, owner: &str, reader: &str, hashes: &Hashes) {
    let path = format!("/web/lists/{LIST}/archive?message={}", hashes.e);
    lacks(&page(app, &path, reader).await, "name=\"parent\"");
    let html = page(app, &path, owner).await;
    has(&html, "name=\"parent\"");
    let token = csrf(&html);
    let action = format!("/web/lists/{LIST}/archive/reattach");
    let response = call(
        app,
        "POST",
        &action,
        owner,
        &serde_urlencoded::to_string([
            ("csrf", token.as_str()),
            ("message", hashes.e.as_str()),
            ("parent", hashes.a.as_str()),
        ])
        .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert!(
        response.headers()["location"]
            .to_str()
            .unwrap()
            .ends_with("&saved=reattached")
    );
    let html = page(
        app,
        &format!("/web/lists/{LIST}/archive?thread={}", hashes.a),
        "",
    )
    .await;
    has(
        &html,
        &format!("class=\"post depth-1\" id=\"m-{}\"", hashes.e),
    );
    let audited: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='archive.reattach'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(audited, 1);
    // Bob (with Carol under him) becomes a root: both leave Alice's thread.
    let response = call(
        app,
        "POST",
        &action,
        owner,
        &serde_urlencoded::to_string([
            ("csrf", token.as_str()),
            ("message", hashes.b.as_str()),
            ("parent", ""),
        ])
        .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let html = page(
        app,
        &format!("/web/lists/{LIST}/archive?thread={}", hashes.b),
        "",
    )
    .await;
    has(
        &html,
        &format!("class=\"post depth-0\" id=\"m-{}\"", hashes.b),
    );
    let c = listmngr_mail::message_id_hash("<c@example.org>").unwrap();
    has(&html, &format!("class=\"post depth-1\" id=\"m-{c}\""));
    lacks(&html, &format!("id=\"m-{}\"", hashes.a));
    reattach_refusals(app, owner, reader, hashes).await;
}

/// A cycle, a stranger and an unknown parent are refused.
async fn reattach_refusals(app: &axum::Router, owner: &str, reader: &str, hashes: &Hashes) {
    let path = format!("/web/lists/{LIST}/archive?message={}", hashes.e);
    let action = format!("/web/lists/{LIST}/archive/reattach");
    let token = csrf(&page(app, &path, owner).await);
    // A cycle: Alice under Erin, who is under Alice.
    let response = call(
        app,
        "POST",
        &action,
        owner,
        &serde_urlencoded::to_string([
            ("csrf", token.as_str()),
            ("message", hashes.a.as_str()),
            ("parent", hashes.e.as_str()),
        ])
        .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert!(
        response.headers()["location"]
            .to_str()
            .unwrap()
            .ends_with("&saved=cycle")
    );
    has(
        &page(app, &format!("{path}&saved=cycle"), owner).await,
        "cannot become its own ancestor",
    );
    for (cookie, expected) in [
        (reader, StatusCode::FORBIDDEN),
        (owner, StatusCode::NOT_FOUND),
    ] {
        // The account page carries a CSRF token for every signed-in reader.
        let token = csrf(&page(app, "/web/account", cookie).await);
        let parent = if expected == StatusCode::NOT_FOUND {
            "nowhere"
        } else {
            hashes.a.as_str()
        };
        assert_eq!(
            call(
                app,
                "POST",
                &action,
                cookie,
                &serde_urlencoded::to_string([
                    ("csrf", token.as_str()),
                    ("message", hashes.d.as_str()),
                    ("parent", parent)
                ])
                .unwrap()
            )
            .await
            .status(),
            expected
        );
    }
}

/// Avatars come through this server, cached, only when switched on.
async fn avatars(db: &Database, app: &axum::Router, hits: &AtomicUsize) {
    let response = call(app, "GET", &format!("/web/lists/{LIST}/archive"), "", "").await;
    has(
        response.headers()["content-security-policy"]
            .to_str()
            .unwrap(),
        "img-src 'self'",
    );
    let html = text(response).await;
    let start = html
        .find("<img class=\"avatar\" src=\"/web/gravatar/")
        .expect("an avatar");
    let src: String = html[start + "<img class=\"avatar\" src=\"".len()..]
        .chars()
        .take_while(|c| *c != '"')
        .collect();
    let response = call(app, "GET", &src, "", "").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "image/png");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        call(app, "GET", &src, "", "").await.status(),
        StatusCode::OK
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "the second request is served from the cache"
    );
    assert_eq!(
        call(app, "GET", "/web/gravatar/not-a-hash", "", "")
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    let (_, off) = seeded_fixture_configured_off(db.clone());
    let response = call(&off, "GET", &format!("/web/lists/{LIST}/archive"), "", "").await;
    lacks(
        response.headers()["content-security-policy"]
            .to_str()
            .unwrap(),
        "img-src",
    );
    lacks(&text(response).await, "class=\"avatar\"");
    assert_eq!(
        call(&off, "GET", &src, "", "").await.status(),
        StatusCode::NOT_FOUND
    );
}

/// The same database behind a router with avatars off.
fn seeded_fixture_configured_off(db: Database) -> (Database, axum::Router) {
    let mut config = Config::default();
    config.site.base_url = "http://localhost".into();
    config.security.rate_limit.login = "1000/min".into();
    config.security.require_2fa_for = Vec::new();
    let app = listmngr_api::router(db.clone(), config);
    (db, app)
}
