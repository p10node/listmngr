//! The browser's mbox export: the whole archive, one thread or one month,
//! plain or gzipped, streamed under the archive's policy for the reader;
//! linked from the archive pages.
use super::{call, login_as, member, seeded_fixture, text, user};
use axum::http::StatusCode;
use listmngr_core::MemberRole;
use listmngr_db::Database;
use listmngr_db::mail_queue::{NewMessage, Queue};
use std::io::Read as _;

#[tokio::test]
async fn archive_export_streams_mbox_under_policy() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    matrix(db).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_archive_export_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_archive_export")
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

const BASE: &str = "/web/lists/public.example.com/archive";

async fn archived(db: &Database, id: &str, date: &str, subject: &str, parent: Option<&str>) {
    let reply = parent
        .map(|parent| format!("In-Reply-To: {parent}\r\n"))
        .unwrap_or_default();
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: format!("Message-ID: {id}\r\nFrom: Alice <alice@example.org>\r\nDate: {date}\r\n{reply}Subject: {subject}\r\nContent-Type: text/plain\r\n\r\nFrom the body of {subject}.\r\n").into_bytes(),
                external_id: id.into(),
                context: serde_json::json!({"version":1,"list_id":"public.example.com"}).to_string(),
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

async fn body(
    app: &axum::Router,
    path: &str,
    cookie: &str,
) -> (StatusCode, String, String, Vec<u8>) {
    let response = call(app, "GET", path, cookie, "").await;
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap().to_owned())
        .unwrap_or_default();
    let disposition = response
        .headers()
        .get("content-disposition")
        .map(|v| v.to_str().unwrap().to_owned())
        .unwrap_or_default();
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024 * 1024)
        .await
        .unwrap()
        .to_vec();
    (status, content_type, disposition, bytes)
}

fn count(bytes: &[u8]) -> usize {
    listmngr_archive::mbox::Reader::new(std::io::Cursor::new(bytes)).count()
}

async fn matrix(db: Database) {
    let (db, app) = seeded_fixture(db).await;
    let list: listmngr_core::ListId = "public.example.com".parse().unwrap();
    db.lists()
        .update(&list, &serde_json::json!({"archive_policy": "public"}))
        .await
        .unwrap();
    archived(
        &db,
        "<a1@example.org>",
        "Mon, 01 Jan 2024 10:00:00 +0000",
        "Kickoff",
        None,
    )
    .await;
    archived(
        &db,
        "<a2@example.org>",
        "Mon, 01 Jan 2024 11:00:00 +0000",
        "Re: Kickoff",
        Some("<a1@example.org>"),
    )
    .await;
    archived(
        &db,
        "<b1@example.org>",
        "Thu, 15 Feb 2024 09:00:00 +0000",
        "Notes",
        None,
    )
    .await;
    let root = listmngr_mail::message_id_hash("<a1@example.org>").unwrap();
    whole_and_selections(&app, &root).await;
    gzip_and_bounds(&app, &root).await;
    links(&app, &root).await;
    anonymised(&db, &app).await;
    policy(&db, &app).await;
}

async fn whole_and_selections(app: &axum::Router, root: &str) {
    let (status, content_type, disposition, bytes) =
        body(app, &format!("{BASE}/export.mbox"), "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type, "application/mbox");
    assert_eq!(
        disposition,
        "attachment; filename=\"public.example.com-all.mbox\""
    );
    assert_eq!(count(&bytes), 3);
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("\n>From the body of Kickoff.\r\n"), "{text}");
    let (_, _, disposition, bytes) =
        body(app, &format!("{BASE}/export.mbox?thread={root}"), "").await;
    assert_eq!(count(&bytes), 2);
    assert!(disposition.contains(&format!("thread-{root}")));
    let (_, _, disposition, bytes) =
        body(app, &format!("{BASE}/export.mbox?month=2024-02"), "").await;
    assert_eq!(count(&bytes), 1);
    // The archive's own bytes: the cooked copy carries the list's prefix.
    assert!(String::from_utf8_lossy(&bytes).contains("Subject: [public] Notes"));
    assert_eq!(
        disposition,
        "attachment; filename=\"public.example.com-2024-02.mbox\""
    );
    let (_, _, _, bytes) = body(app, &format!("{BASE}/export.mbox?month=2024-03"), "").await;
    assert_eq!(count(&bytes), 0, "an empty month is an empty file");
}

async fn gzip_and_bounds(app: &axum::Router, root: &str) {
    let (status, content_type, disposition, bytes) =
        body(app, &format!("{BASE}/export.mbox.gz?thread={root}"), "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type, "application/gzip");
    assert!(disposition.ends_with(".mbox.gz\""), "{disposition}");
    let mut plain = Vec::new();
    flate2::read::GzDecoder::new(bytes.as_slice())
        .read_to_end(&mut plain)
        .unwrap();
    let (_, _, _, expected) = body(app, &format!("{BASE}/export.mbox?thread={root}"), "").await;
    assert_eq!(plain, expected, "gzip carries the same bytes");
    for path in [
        format!("{BASE}/export.mbox?month=2024-13"),
        format!("{BASE}/export.mbox?month=nope"),
        format!("{BASE}/export.mbox?thread={root}&month=2024-01"),
        format!("{BASE}/export.mbox?thread={}", "x".repeat(201)),
        format!("{BASE}/export.mbox?other=1"),
    ] {
        assert_eq!(
            call(app, "GET", &path, "", "").await.status(),
            StatusCode::BAD_REQUEST,
            "{path}"
        );
    }
}

async fn links(app: &axum::Router, root: &str) {
    let page = text(call(app, "GET", BASE, "", "").await).await;
    has(&page, &format!("{BASE}/export.mbox\""));
    has(&page, &format!("{BASE}/export.mbox.gz\""));
    let thread = text(call(app, "GET", &format!("{BASE}/thread/{root}"), "", "").await).await;
    has(&thread, &format!("{BASE}/export.mbox?thread={root}"));
    let month = text(call(app, "GET", &format!("{BASE}/threads/2024/01"), "", "").await).await;
    has(&month, &format!("{BASE}/export.mbox?month=2024-01"));
}

/// An anonymous list hides its authors on the archive pages and in the
/// page's own mbox download; the streamed export must project the stored
/// copy the same way rather than hand out the raw author.
async fn anonymised(db: &Database, app: &axum::Router) {
    let list: listmngr_core::ListId = "public.example.com".parse().unwrap();
    db.lists()
        .update(&list, &serde_json::json!({"anonymous_list": true}))
        .await
        .unwrap();
    let (_, _, _, bytes) = body(app, &format!("{BASE}/export.mbox"), "").await;
    let exported = String::from_utf8_lossy(&bytes);
    assert!(
        !exported.contains("alice@example.org"),
        "the export published the author of an anonymous list: {exported}"
    );
    assert_eq!(count(&bytes), 3);
    db.lists()
        .update(&list, &serde_json::json!({"anonymous_list": false}))
        .await
        .unwrap();
}

async fn policy(db: &Database, app: &axum::Router) {
    let list: listmngr_core::ListId = "public.example.com".parse().unwrap();
    db.lists()
        .update(&list, &serde_json::json!({"archive_policy": "private"}))
        .await
        .unwrap();
    assert_eq!(
        call(app, "GET", &format!("{BASE}/export.mbox"), "", "")
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    user(db, "member@example.com", false).await;
    member(db, "member@example.com", MemberRole::Member).await;
    let reader = login_as(app, "member@example.com").await;
    let (status, _, _, bytes) = body(app, &format!("{BASE}/export.mbox"), &reader).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(count(&bytes), 3);
    db.lists()
        .update(&list, &serde_json::json!({"archive_policy": "never"}))
        .await
        .unwrap();
    assert_eq!(
        call(app, "GET", &format!("{BASE}/export.mbox.gz"), &reader, "")
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
}
