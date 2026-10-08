//! Posting from the web: a signed-in member with a verified address on
//! the list writes a new thread or a reply from the archive; the message
//! is composed from that address and injected into the `in` queue with a
//! context that names the web origin, so the posting chain admits it the
//! way an `Approved:` post is admitted. Visitors, non-members, unverified
//! and banned addresses are refused; the form is validated inline.
use super::{call, csrf, login_as, member, seeded_fixture, text, user};
use axum::http::StatusCode;
use listmngr_core::MemberRole;
use listmngr_db::Database;
use listmngr_db::mail_queue::{NewMessage, Queue};

#[tokio::test]
async fn web_posts_are_composed_and_injected_for_verified_members() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    matrix(db).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_archive_post_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_archive_post")
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

async fn status(app: &axum::Router, path: &str, cookie: &str) -> StatusCode {
    call(app, "GET", path, cookie, "").await.status()
}

/// A POST with the session's CSRF token; returns the status, the
/// `Location` header and the body.
async fn post(
    app: &axum::Router,
    path: &str,
    cookie: &str,
    body: &str,
) -> (StatusCode, String, String) {
    let token = csrf(&page(app, "/web/account", cookie).await);
    let response = call(app, "POST", path, cookie, &format!("csrf={token}&{body}")).await;
    let location = response
        .headers()
        .get("location")
        .map(|v| v.to_str().unwrap().to_owned())
        .unwrap_or_default();
    let status = response.status();
    (status, location, text(response).await)
}

const BASE: &str = "/web/lists/public.example.com/archive";
const POST: &str = "/web/lists/public.example.com/archive/post";

async fn archived(db: &Database, id: &str, subject: &str, body: &str) -> String {
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: format!("Message-ID: {id}\r\nFrom: Alice <alice@example.org>\r\nDate: Mon, 01 Jan 2024 10:00:00 +0000\r\nSubject: {subject}\r\nContent-Type: text/plain\r\n\r\n{body}\r\n").into_bytes(),
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
    listmngr_mail::message_id_hash(id).unwrap()
}

/// The next `in` job: its raw message and its context. The lease outlives
/// the test, so a job read here is never handed back to a later call: with
/// a one-second lease, a slow run let the reply assertion read the first
/// web post again.
async fn injected(db: &Database) -> (String, serde_json::Value) {
    let lease = db
        .mail_queue()
        .claim(
            Queue::In,
            "test",
            chrono::Utc::now().timestamp_millis(),
            600_000,
        )
        .await
        .unwrap()
        .expect("a job in the in queue");
    let message = db
        .mail_queue()
        .live()
        .message(lease.job.message_id)
        .await
        .unwrap();
    (
        String::from_utf8(message.raw).unwrap(),
        serde_json::from_str(&message.context).unwrap(),
    )
}

async fn matrix(db: Database) {
    let (db, app) = seeded_fixture(db).await;
    db.lists()
        .update(
            &"public.example.com".parse().unwrap(),
            &serde_json::json!({"archive_policy": "public"}),
        )
        .await
        .unwrap();
    let parent = archived(
        &db,
        "<a1@example.org>",
        "Release plan",
        "The release ships on Friday.\r\nWith the installer.",
    )
    .await;
    let poster = user(&db, "poster@example.com", false).await;
    member(&db, "poster@example.com", MemberRole::Member).await;
    let poster_cookie = login_as(&app, "poster@example.com").await;
    admission(&db, &app, &poster_cookie).await;
    let form = page(&app, POST, &poster_cookie).await;
    has(&form, "New thread");
    has(&form, "poster@example.com");
    has(&form, "name=\"subject\"");
    has(&form, "name=\"body\"");
    reply_form(&app, &poster_cookie, &parent).await;
    new_thread(&db, &app, &poster_cookie, &poster.id.to_string()).await;
    reply(&db, &app, &poster_cookie, &parent).await;
    validation(&app, &poster_cookie).await;
    refusals(&db, &app, &poster_cookie).await;
}

/// Who may open the form: a signed-in member with a verified address on
/// the list; nobody else.
async fn admission(db: &Database, app: &axum::Router, poster: &str) {
    assert_eq!(status(app, POST, "").await, StatusCode::UNAUTHORIZED);
    user(db, "stranger@example.com", false).await;
    let stranger = login_as(app, "stranger@example.com").await;
    let response = call(app, "GET", POST, &stranger, "").await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    user(db, "unverified@example.com", false).await;
    member(db, "unverified@example.com", MemberRole::Member).await;
    let unverified = login_as(app, "unverified@example.com").await;
    db.addresses()
        .verify("unverified@example.com", false)
        .await
        .unwrap();
    assert_eq!(status(app, POST, &unverified).await, StatusCode::FORBIDDEN);
    // The archive page offers the member the entry points.
    let archive = page(app, BASE, poster).await;
    has(&archive, &format!("{POST}\""));
    has(&archive, &format!("{POST}?reply="));
    lacks(&page(app, BASE, "").await, "archive/post");
}

/// A reply form quotes the parent and keeps its subject.
async fn reply_form(app: &axum::Router, poster: &str, parent: &str) {
    let form = page(app, &format!("{POST}?reply={parent}"), poster).await;
    has(&form, "value=\"Re: Release plan\"");
    has(
        &form,
        "&gt; The release ships on Friday.\n&gt; With the installer.",
    );
    has(&form, &format!("name=\"reply\" value=\"{parent}\""));
    assert_eq!(
        status(app, &format!("{POST}?reply=absent"), poster).await,
        StatusCode::NOT_FOUND
    );
}

/// A new thread is composed from the member's address and injected with
/// the web-post context.
async fn new_thread(db: &Database, app: &axum::Router, poster: &str, user_id: &str) {
    let (status, location, _) = post(
        app,
        POST,
        poster,
        "subject=Hello+list&body=First+web+post.%0A%0ASecond+paragraph+with+%C3%A9.",
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location, format!("{BASE}?saved=posted"));
    has(
        &page(app, &location, poster).await,
        "Your post is on its way to the list",
    );
    let (raw, context) = injected(db).await;
    has(&raw, "From: <poster@example.com>\r\n");
    has(&raw, "To: <public@example.com>\r\n");
    has(&raw, "Subject: Hello list\r\n");
    has(&raw, "Message-ID: <");
    has(&raw, "Date: ");
    has(&raw, "User-Agent: listmngr-web\r\n");
    lacks(&raw, "In-Reply-To");
    has(&raw, "First web post.");
    has(&raw, "Second paragraph with");
    assert_eq!(context["list_id"], "public.example.com");
    assert_eq!(context["envelope_sender"], "poster@example.com");
    assert_eq!(context["web_post"]["address"], "poster@example.com");
    assert_eq!(context["web_post"]["user_id"], user_id);
    let id = listmngr_mail::parse_message_id(raw.as_bytes()).unwrap();
    assert_eq!(
        context["message_id_hash"],
        listmngr_mail::message_id_hash(&id).unwrap()
    );
}

/// A reply threads under its parent and returns to the thread page.
async fn reply(db: &Database, app: &axum::Router, poster: &str, parent: &str) {
    let (status, location, _) = post(
        app,
        POST,
        poster,
        &format!("reply={parent}&subject=Re%3A+Release+plan&body=%3E+quoted%0AAgreed."),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location, format!("{BASE}/thread/{parent}?saved=posted"));
    let (raw, context) = injected(db).await;
    has(&raw, "In-Reply-To: <a1@example.org>\r\n");
    has(&raw, "References: <a1@example.org>\r\n");
    has(&raw, "Subject: Re: Release plan\r\n");
    assert_eq!(context["web_post"]["reply"], parent);
    // An absent parent is refused before anything is queued.
    let (status, _, _) = post(app, POST, poster, "reply=absent&subject=x&body=y").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// The form is validated inline: subject and body required and bounded.
async fn validation(app: &axum::Router, poster: &str) {
    for (body, expected) in [
        ("subject=&body=text", "A subject is required."),
        ("subject=Hi&body=", "A message body is required."),
        (
            "subject=Bad%0Aheader&body=text",
            "The subject cannot contain line breaks.",
        ),
        (
            &format!("subject={}&body=text", "s".repeat(201)),
            "The subject is too long.",
        ),
        (
            &format!("subject=Hi&body={}", "b".repeat(70_000)),
            "The message is too long.",
        ),
    ] {
        let (status, _, html) = post(app, POST, poster, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{expected}");
        has(&html, expected);
        has(&html, "name=\"subject\"");
    }
}

/// A banned address may not post; a list without an archive has no form.
async fn refusals(db: &Database, app: &axum::Router, poster: &str) {
    let list: listmngr_core::ListId = "public.example.com".parse().unwrap();
    db.bans()
        .create(
            &list,
            "poster@example.com",
            &listmngr_db::AuditContext::system(),
        )
        .await
        .unwrap();
    assert_eq!(status(app, POST, poster).await, StatusCode::FORBIDDEN);
    let (status, _, _) = post(app, POST, poster, "subject=x&body=y").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    db.bans()
        .delete(
            &list,
            "poster@example.com",
            &listmngr_db::AuditContext::system(),
        )
        .await
        .unwrap();
    db.lists()
        .update(&list, &serde_json::json!({"archive_policy": "never"}))
        .await
        .unwrap();
    assert_eq!(
        call(app, "GET", POST, poster, "").await.status(),
        StatusCode::NOT_FOUND
    );
}
