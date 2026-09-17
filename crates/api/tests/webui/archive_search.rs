//! The archive page searches the tantivy index when one exists — ranked,
//! scoped to the list, every word required — and the database's substring
//! match otherwise; hits are read back through the archive's own policy.
use super::{call, login_as, member, seeded_fixture_configured, text, user};
use axum::http::StatusCode;
use listmngr_core::{Config, MemberRole};
use listmngr_db::Database;
use listmngr_db::mail_queue::{NewMessage, Queue};

#[tokio::test]
async fn archive_search_uses_the_index_when_present() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    matrix(db).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_archive_search_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_archive_search")
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

async fn archived(db: &Database, list: &str, id: &str, subject: &str, body: &str) {
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: format!("Message-ID: {id}\r\nFrom: Poster <poster@example.org>\r\nDate: Mon, 01 Jan 2024 10:00:00 +0000\r\nSubject: {subject}\r\nContent-Type: text/plain\r\n\r\n{body}\r\n").into_bytes(),
                external_id: id.into(),
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
}

async fn matrix(db: Database) {
    let dir = tempfile::tempdir().unwrap();
    let index_path = dir.path().join("index");
    let configured = index_path.to_string_lossy().into_owned();
    let (db, app) = seeded_fixture_configured(db, |config: &mut Config| {
        config.archive.index_path = configured;
    })
    .await;
    seed(&db).await;
    fallback(&app).await;
    assert!(!index_path.exists());
    let index = listmngr_archive::search::SearchIndex::open(&index_path).unwrap();
    assert_eq!(listmngr_archive::reindex(&db, &index).await.unwrap(), 4);
    drop(index);
    ranked(&app).await;
    private_policy(&db, &app).await;
}

/// Four posts on two public lists, indexed through the runner path.
async fn seed(db: &Database) {
    for list in ["public.example.com", "private.example.com"] {
        db.lists()
            .update(
                &list.parse().unwrap(),
                &serde_json::json!({"archive_policy": "public"}),
            )
            .await
            .unwrap();
    }
    for (list, id, subject, body) in [
        (
            "public.example.com",
            "<r1@example.org>",
            "Release plan",
            "The release ships on Friday with the installer.",
        ),
        (
            "public.example.com",
            "<r2@example.org>",
            "Re: Release plan",
            "Friday works; the installer needs one more test.",
        ),
        (
            "public.example.com",
            "<l1@example.org>",
            "Lunch",
            "Sandwiches at noon <b>bold</b>.",
        ),
        (
            "private.example.com",
            "<o1@example.org>",
            "Release plan",
            "Ops copies the release to the mirrors on Friday.",
        ),
    ] {
        archived(db, list, id, subject, body).await;
    }
}

const BASE: &str = "/web/lists/public.example.com/archive";

/// Without an index the substring search answers, as before.
async fn fallback(app: &axum::Router) {
    let html = page(app, &format!("{BASE}?q=installer"), "").await;
    assert_eq!(html.matches("<article ").count(), 2);
    let html = page(app, &format!("{BASE}?q=release+installer+test"), "").await;
    assert_eq!(
        html.matches("<article ").count(),
        0,
        "a substring, not words, without the index"
    );
}

/// With the index the page ranks: every word required, the list scoped,
/// the sender searchable, the body still escaped, bounds kept.
async fn ranked(app: &axum::Router) {
    let html = page(app, &format!("{BASE}?q=release+installer+test"), "").await;
    assert_eq!(
        html.matches("<article ").count(),
        1,
        "every word required: {html}"
    );
    has(&html, "Re: <mark>Release</mark> plan");
    for (query, articles) in [("installer", 2), ("mirrors", 0), ("poster", 3)] {
        let html = page(app, &format!("{BASE}?q={query}"), "").await;
        assert_eq!(html.matches("<article ").count(), articles, "{query}");
    }
    has(
        &page(app, &format!("{BASE}?q=mirrors"), "").await,
        "No messages found",
    );
    let html = page(app, &format!("{BASE}?q=sandwiches"), "").await;
    has(
        &html,
        "<mark>Sandwiches</mark> at noon &lt;b&gt;bold&lt;/b&gt;",
    );
    lacks(&html, "<b>bold</b>");
    assert_eq!(
        call(app, "GET", &format!("{BASE}?q={}", "x".repeat(201)), "", "")
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
}

/// A private list's hits are read back through its policy: a visitor
/// gets nothing, a verified member the post.
async fn private_policy(db: &Database, app: &axum::Router) {
    db.lists()
        .update(
            &"private.example.com".parse().unwrap(),
            &serde_json::json!({"archive_policy": "private"}),
        )
        .await
        .unwrap();
    let private = "/web/lists/private.example.com/archive?q=mirrors";
    assert_eq!(
        call(app, "GET", private, "", "").await.status(),
        StatusCode::FORBIDDEN
    );
    user(db, "member@example.com", false).await;
    db.members()
        .create(listmngr_db::NewMember {
            list_id: "private.example.com".parse().unwrap(),
            email: "member@example.com".into(),
            role: MemberRole::Member,
            subscription_mode: listmngr_core::SubscriptionMode::AsAddress,
            display_name: String::new(),
        })
        .await
        .unwrap();
    member(db, "member@example.com", MemberRole::Member).await;
    let reader = login_as(app, "member@example.com").await;
    let html = page(app, private, &reader).await;
    assert_eq!(html.matches("<article ").count(), 1);
    has(&html, "mirrors");
}
