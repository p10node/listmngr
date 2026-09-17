//! The archive's browsing pages: an overview with figures, recent and
//! active threads, top posters and months; thread lists by latest
//! activity and by month with a signed-in reader's unread marks; the
//! thread page that clears them; sender pages keyed by an address digest,
//! never by the address; the search page's result count and highlights;
//! and the Atom and RSS feeds, all behind the archive's policy.
use super::{call, login_as, member, seeded_fixture_configured, text, user};
use axum::http::StatusCode;
use listmngr_core::{Config, MemberRole};
use listmngr_db::Database;
use listmngr_db::mail_queue::{NewMessage, Queue};

#[tokio::test]
async fn archive_overview_threads_senders_search_and_feeds() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    matrix(db).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_archive_ui_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_archive_ui")
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

fn before(html: &str, first: &str, second: &str) {
    let (a, b) = (html.find(first), html.find(second));
    assert!(
        a.is_some() && b.is_some() && a < b,
        "expected {first:?} before {second:?} in: {html}"
    );
}

async fn page(app: &axum::Router, path: &str, cookie: &str) -> String {
    let response = call(app, "GET", path, cookie, "").await;
    assert_eq!(response.status(), StatusCode::OK, "{path}");
    text(response).await
}

async fn status(app: &axum::Router, path: &str, cookie: &str) -> StatusCode {
    call(app, "GET", path, cookie, "").await.status()
}

const BASE: &str = "/web/lists/public.example.com/archive";

fn days_ago(days: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc2822()
}

fn digest(email: &str) -> String {
    use sha2::Digest as _;
    format!("{:x}", sha2::Sha256::digest(email))
}

/// One post through the archive runner; returns its Message-ID-Hash.
async fn archived(db: &Database, list: &str, post: &Post<'_>) -> String {
    let reply = post
        .parent
        .map(|parent| format!("In-Reply-To: {parent}\r\n"))
        .unwrap_or_default();
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: format!("Message-ID: {}\r\nFrom: {}\r\nDate: {}\r\n{reply}Subject: {}\r\nContent-Type: text/plain\r\n\r\n{}\r\n", post.id, post.from, post.date, post.subject, post.body).into_bytes(),
                external_id: post.id.into(),
                context: serde_json::json!({"version":1,"list_id":list}).to_string(),
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
    listmngr_mail::message_id_hash(post.id).unwrap()
}

struct Post<'a> {
    id: &'a str,
    from: &'a str,
    date: String,
    subject: &'a str,
    body: &'a str,
    parent: Option<&'a str>,
}

/// Five posts on the public list: one thread from January 2024, one three
/// posts long from this week, one single post from two days ago; and one
/// post on the private list.
async fn seed(db: &Database) -> String {
    for list in ["public.example.com", "private.example.com"] {
        db.lists()
            .update(
                &list.parse().unwrap(),
                &serde_json::json!({"archive_policy": "public"}),
            )
            .await
            .unwrap();
    }
    let posts = [
        Post {
            id: "<old1@example.org>",
            from: "Olivia <olivia@example.org>",
            date: "Mon, 01 Jan 2024 10:00:00 +0000".into(),
            subject: "Old plan",
            body: "Archive from last year.",
            parent: None,
        },
        Post {
            id: "<a1@example.org>",
            from: "Alice <alice@example.org>",
            date: days_ago(5),
            subject: "Release plan",
            body: "The release ships on Friday with the installer.",
            parent: None,
        },
        Post {
            id: "<a2@example.org>",
            from: "Bob <bob@example.org>",
            date: days_ago(4),
            subject: "Re: Release plan",
            body: "Friday works; the installer needs one more test.",
            parent: Some("<a1@example.org>"),
        },
        Post {
            id: "<a3@example.org>",
            from: "Alice <alice@example.org>",
            date: days_ago(3),
            subject: "Re: Release plan",
            body: "Installer <b>bold</b> done.",
            parent: Some("<a1@example.org>"),
        },
        Post {
            id: "<b1@example.org>",
            from: "Carol <carol@example.org>",
            date: days_ago(2),
            subject: "Lunch",
            body: "Sandwiches at noon.",
            parent: None,
        },
    ];
    let mut release = String::new();
    for post in &posts {
        let hash = archived(db, "public.example.com", post).await;
        if post.id == "<a1@example.org>" {
            release = hash;
        }
    }
    archived(
        db,
        "private.example.com",
        &Post {
            id: "<p1@example.org>",
            from: "Priya <priya@example.org>",
            date: days_ago(1),
            subject: "Ops mirrors",
            body: "Ops copies the release to the mirrors.",
            parent: None,
        },
    )
    .await;
    release
}

async fn matrix(db: Database) {
    let dir = tempfile::tempdir().unwrap();
    let index_path = dir.path().join("index");
    let configured = index_path.to_string_lossy().into_owned();
    let (db, app) = seeded_fixture_configured(db, |config: &mut Config| {
        config.archive.index_path = configured;
    })
    .await;
    let release = seed(&db).await;
    overview(&app).await;
    user(&db, "reader@example.com", false).await;
    member(&db, "reader@example.com", MemberRole::Member).await;
    let reader = login_as(&app, "reader@example.com").await;
    threads_and_unread(&app, &reader, &release).await;
    senders(&app, &reader).await;
    let index = listmngr_archive::search::SearchIndex::open(&index_path).unwrap();
    assert_eq!(listmngr_archive::reindex(&db, &index).await.unwrap(), 6);
    drop(index);
    search_ui(&app).await;
    feeds(&db, &app).await;
}

/// Figures, months, the latest threads, the active threads of the last
/// thirty days and the senders who posted most.
async fn overview(app: &axum::Router) {
    let response = call(app, "GET", &format!("{BASE}/overview"), "", "").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers().contains_key("content-security-policy"),
        "the shell's CSP"
    );
    let html = text(response).await;
    has(&html, "Archive overview: public.example.com");
    for (label, figure) in [
        ("Posts", "<dd>5</dd>"),
        ("Threads", "<dd>3</dd>"),
        ("Participants", "<dd>4</dd>"),
    ] {
        has(&html, label);
        has(&html, figure);
    }
    has(&html, &format!("{BASE}/threads/2024/01"));
    has(&html, "2024-01");
    let recent = html.split("Recent activity").nth(1).unwrap_or_default();
    before(recent, "Lunch", "Release plan");
    before(recent, "Release plan", "Old plan");
    let active = html.split("Most active threads").nth(1).unwrap_or_default();
    before(active, "Release plan", "Lunch");
    lacks(
        active.split("Top posters").next().unwrap_or_default(),
        "Old plan",
    );
    let posters = html.split("Top posters").nth(1).unwrap_or_default();
    before(posters, "Alice", "Bob");
    has(
        posters,
        &format!("{BASE}/senders/{}", digest("alice@example.org")),
    );
    lacks(&html, "alice@example.org");
    has(&html, &format!("{BASE}/feed.atom"));
    has(&html, &format!("{BASE}/threads\""));
}

/// Thread lists by latest activity and by month; a signed-in reader's
/// unread marks clear when the thread page is opened; a visitor sees none.
async fn threads_and_unread(app: &axum::Router, reader: &str, release: &str) {
    let html = page(app, &format!("{BASE}/threads"), "").await;
    has(&html, "Latest threads");
    before(&html, "Lunch", "Release plan");
    before(&html, "Release plan", "Old plan");
    has(&html, &format!("{BASE}/thread/{release}"));
    assert_eq!(html.matches("class=\"badge new\"").count(), 0, "a visitor");
    let html = page(app, &format!("{BASE}/threads"), reader).await;
    assert_eq!(html.matches("class=\"badge new\"").count(), 3, "{html}");
    let thread = page(app, &format!("{BASE}/thread/{release}"), reader).await;
    assert_eq!(thread.matches("<article ").count(), 3);
    has(&thread, "depth-1");
    let html = page(app, &format!("{BASE}/threads"), reader).await;
    assert_eq!(html.matches("class=\"badge new\"").count(), 2, "{html}");
    let month = page(app, &format!("{BASE}/threads/2024/01"), "").await;
    has(&month, "Old plan");
    lacks(&month, "Lunch");
    has(&month, "2024-01");
    assert_eq!(
        status(app, &format!("{BASE}/threads/2024/13"), "").await,
        StatusCode::BAD_REQUEST
    );
    has(
        &page(app, &format!("{BASE}/threads?page=2"), "").await,
        "No threads",
    );
    assert_eq!(
        status(app, &format!("{BASE}/thread/{}", "x".repeat(201)), "").await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        status(app, &format!("{BASE}/thread/absent"), "").await,
        StatusCode::NOT_FOUND
    );
}

/// A sender page is reached by the address digest each post links to; the
/// address itself shows only to a signed-in reader.
async fn senders(app: &axum::Router, reader: &str) {
    let alice = format!("{BASE}/senders/{}", digest("alice@example.org"));
    let html = page(app, &alice, "").await;
    has(&html, "Posts by Alice");
    has(&html, "2 posts");
    has(&html, "alice at example.org");
    lacks(&html, "alice@example.org");
    assert_eq!(html.matches("<article ").count(), 2);
    lacks(&html, "Lunch");
    let html = page(app, &alice, reader).await;
    has(&html, "alice@example.org");
    let post = page(app, BASE, "").await;
    has(&post, &alice);
    assert_eq!(
        status(
            app,
            &format!("{BASE}/senders/{}", digest("nobody@example.org")),
            ""
        )
        .await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        status(app, &format!("{BASE}/senders/not-a-digest"), "").await,
        StatusCode::BAD_REQUEST
    );
}

/// With the index, the search page counts its results and marks the words
/// it matched, in the subject and the body, never inside a tag or an
/// entity.
async fn search_ui(app: &axum::Router) {
    let html = page(app, &format!("{BASE}?q=friday"), "").await;
    has(&html, "2 results for");
    assert!(html.matches("<mark>Friday</mark>").count() >= 2, "{html}");
    let html = page(app, &format!("{BASE}?q=release"), "").await;
    has(&html, "Re: <mark>Release</mark> plan</h2>");
    has(&html, "3 results for");
    let html = page(app, &format!("{BASE}?q=bold+lt"), "").await;
    has(&html, "No messages found");
    let html = page(app, &format!("{BASE}?q=bold"), "").await;
    has(&html, "&lt;b&gt;<mark>bold</mark>&lt;/b&gt;");
    lacks(&html, "<b>");
    let html = page(app, BASE, "").await;
    lacks(&html, "results for");
    lacks(&html, "<mark>");
}

/// Atom and RSS carry the latest posts with escaped text, and a private
/// list's feed is refused to a visitor.
async fn feeds(db: &Database, app: &axum::Router) {
    let response = call(app, "GET", &format!("{BASE}/feed.atom"), "", "").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-type"],
        "application/atom+xml; charset=utf-8"
    );
    let atom = text(response).await;
    has(&atom, "<feed xmlns=\"http://www.w3.org/2005/Atom\">");
    assert_eq!(atom.matches("<entry>").count(), 5);
    before(&atom, "[public] Lunch</title>", "[public] Old plan</title>");
    has(&atom, "&lt;b&gt;bold&lt;/b&gt;");
    has(&atom, "<name>Alice</name>");
    lacks(&atom, "alice@example.org");
    has(
        &atom,
        "http://localhost/web/lists/public.example.com/archive?message=",
    );
    let response = call(app, "GET", &format!("{BASE}/feed.rss"), "", "").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-type"],
        "application/rss+xml; charset=utf-8"
    );
    let rss = text(response).await;
    has(&rss, "<rss version=\"2.0\" xmlns:dc=");
    assert_eq!(rss.matches("<item>").count(), 5);
    has(&rss, "<pubDate>");
    let private = "/web/lists/private.example.com/archive";
    assert_eq!(
        status(app, &format!("{private}/feed.atom"), "").await,
        StatusCode::OK
    );
    db.lists()
        .update(
            &"private.example.com".parse().unwrap(),
            &serde_json::json!({"archive_policy": "private"}),
        )
        .await
        .unwrap();
    for path in ["/feed.atom", "/overview", "/threads"] {
        assert_eq!(
            status(app, &format!("{private}{path}"), "").await,
            StatusCode::FORBIDDEN,
            "{path}"
        );
    }
    db.lists()
        .update(
            &"private.example.com".parse().unwrap(),
            &serde_json::json!({"archive_policy": "never"}),
        )
        .await
        .unwrap();
    assert_eq!(
        status(app, &format!("{private}/overview"), "").await,
        StatusCode::NOT_FOUND
    );
}
