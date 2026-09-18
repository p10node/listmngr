//! Readers interact with the archive: a signed-in reader votes a post up
//! or down once (and takes it back), tags a thread (tags normalised,
//! removable by the tagger or an owner), an owner files a thread under one
//! of the list's categories, and a reader keeps favourite threads — each
//! with its page, each behind the archive's policy, votes, tags and
//! categories audited in the same transaction.
use super::{call, csrf, login_as, member, seeded_fixture, text, user};
use axum::http::StatusCode;
use listmngr_core::MemberRole;
use listmngr_db::Database;
use listmngr_db::mail_queue::{NewMessage, Queue};

#[tokio::test]
async fn archive_votes_tags_categories_and_favorites() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    matrix(db).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_archive_interactions_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_archive_interact")
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

/// A POST with the session's CSRF token; returns the status and the
/// `Location` header.
async fn post(app: &axum::Router, path: &str, cookie: &str, body: &str) -> (StatusCode, String) {
    let token = csrf(&page(app, "/web/account", cookie).await);
    let response = call(app, "POST", path, cookie, &format!("csrf={token}&{body}")).await;
    let location = response
        .headers()
        .get("location")
        .map(|v| v.to_str().unwrap().to_owned())
        .unwrap_or_default();
    (response.status(), location)
}

async fn audit_count(db: &Database, action: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action=$1")
        .bind(action)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

const BASE: &str = "/web/lists/public.example.com/archive";

async fn archived(db: &Database, id: &str, subject: &str, parent: Option<&str>) -> String {
    let reply = parent
        .map(|parent| format!("In-Reply-To: {parent}\r\n"))
        .unwrap_or_default();
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: format!("Message-ID: {id}\r\nFrom: Alice <alice@example.org>\r\nDate: Mon, 01 Jan 2024 10:00:00 +0000\r\n{reply}Subject: {subject}\r\nContent-Type: text/plain\r\n\r\nBody of {subject}.\r\n").into_bytes(),
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

struct Seed {
    release: String,
    reply: String,
    lunch: String,
}

async fn seed(db: &Database) -> Seed {
    db.lists()
        .update(
            &"public.example.com".parse().unwrap(),
            &serde_json::json!({"archive_policy": "public"}),
        )
        .await
        .unwrap();
    let release = archived(db, "<a1@example.org>", "Release plan", None).await;
    let reply = archived(
        db,
        "<a2@example.org>",
        "Re: Release plan",
        Some("<a1@example.org>"),
    )
    .await;
    let lunch = archived(db, "<b1@example.org>", "Lunch", None).await;
    for name in ["planning", "social"] {
        sqlx::query("INSERT INTO archive_categories(list_id, name) VALUES($1, $2)")
            .bind("public.example.com")
            .bind(name)
            .execute(db.pool())
            .await
            .unwrap();
    }
    Seed {
        release,
        reply,
        lunch,
    }
}

struct People {
    reader: String,
    other: String,
    owner: String,
}

async fn people(db: &Database, app: &axum::Router) -> People {
    for (email, role) in [
        ("reader@example.com", MemberRole::Member),
        ("other@example.com", MemberRole::Member),
        ("owner@example.com", MemberRole::Owner),
    ] {
        user(db, email, false).await;
        member(db, email, role).await;
    }
    People {
        reader: login_as(app, "reader@example.com").await,
        other: login_as(app, "other@example.com").await,
        owner: login_as(app, "owner@example.com").await,
    }
}

async fn matrix(db: Database) {
    let (db, app) = seeded_fixture(db).await;
    let seed = seed(&db).await;
    let people = people(&db, &app).await;
    votes(&db, &app, &seed, &people).await;
    tags(&db, &app, &seed, &people).await;
    categories(&db, &app, &seed, &people).await;
    favorites(&app, &seed, &people).await;
    policy(&db, &app, &seed, &people).await;
}

/// One vote per reader per post, up or down, taken back with a zero;
/// visitors see the score and no form.
async fn votes(db: &Database, app: &axum::Router, seed: &Seed, people: &People) {
    let permalink = format!("{BASE}?message={}", seed.release);
    let html = page(app, &permalink, "").await;
    has(&html, "class=\"score\">0<");
    lacks(&html, "name=\"value\"");
    assert_eq!(
        call(app, "POST", &format!("{BASE}/vote"), "", "hash=x&value=1")
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let (status, location) = post(
        app,
        &format!("{BASE}/vote"),
        &people.reader,
        &format!("hash={}&value=1", seed.release),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location, format!("{permalink}&saved=voted"));
    let html = page(app, &location, &people.reader).await;
    has(&html, "class=\"score\">1<");
    has(&html, "Your vote was recorded.");
    has(&html, "value=\"0\" aria-pressed=\"true\"");
    // The same reader again changes nothing but the direction.
    post(
        app,
        &format!("{BASE}/vote"),
        &people.reader,
        &format!("hash={}&value=-1", seed.release),
    )
    .await;
    has(
        &page(app, &permalink, &people.reader).await,
        "class=\"score\">-1<",
    );
    post(
        app,
        &format!("{BASE}/vote"),
        &people.owner,
        &format!("hash={}&value=1", seed.release),
    )
    .await;
    has(&page(app, &permalink, "").await, "class=\"score\">0<");
    post(
        app,
        &format!("{BASE}/vote"),
        &people.reader,
        &format!("hash={}&value=0", seed.release),
    )
    .await;
    has(&page(app, &permalink, "").await, "class=\"score\">1<");
    for (body, status) in [
        (
            format!("hash={}&value=5", seed.release),
            StatusCode::BAD_REQUEST,
        ),
        ("hash=absent&value=1".to_owned(), StatusCode::NOT_FOUND),
    ] {
        assert_eq!(
            post(app, &format!("{BASE}/vote"), &people.reader, &body)
                .await
                .0,
            status,
            "{body}"
        );
    }
    assert_eq!(audit_count(db, "archive.vote").await, 4);
    // The thread page carries every post's score.
    let thread = page(
        app,
        &format!("{BASE}/thread/{}", seed.release),
        &people.reader,
    )
    .await;
    assert_eq!(thread.matches("class=\"score\">").count(), 2);
}

/// Tags are normalised, listed on the thread page with a page of their
/// own, and removed by their tagger or an owner only.
async fn tags(db: &Database, app: &axum::Router, seed: &Seed, people: &People) {
    let thread = format!("{BASE}/thread/{}", seed.release);
    let action = format!("{BASE}/tags");
    let (status, location) = post(
        app,
        &action,
        &people.reader,
        &format!("thread={}&tag=Release+Notes&op=add", seed.release),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location, format!("{thread}?saved=tagged"));
    let html = page(app, &location, &people.reader).await;
    has(&html, "The tag was added.");
    has(&html, &format!("{BASE}/tags/release-notes\""));
    has(&html, ">release-notes</a>");
    has(&html, "Remove tag release-notes");
    let listing = page(app, &format!("{BASE}/tags/release-notes"), "").await;
    has(&listing, "Threads tagged release-notes");
    has(&listing, "Release plan");
    lacks(&listing, "Lunch");
    has(
        &page(app, &format!("{BASE}/threads"), "").await,
        ">release-notes</a>",
    );
    for (cookie, tag, status) in [
        (&people.reader, "!!!", StatusCode::SEE_OTHER),
        (&people.reader, &"x".repeat(41), StatusCode::SEE_OTHER),
        (&people.other, "release-notes", StatusCode::FORBIDDEN),
    ] {
        let op = if tag == "release-notes" {
            "remove"
        } else {
            "add"
        };
        let (got, location) = post(
            app,
            &action,
            cookie,
            &format!("thread={}&tag={tag}&op={op}", seed.release),
        )
        .await;
        assert_eq!(got, status, "{tag}");
        if op == "add" {
            assert_eq!(location, format!("{thread}?saved=tag-refused"));
        }
    }
    has(
        &page(app, &format!("{thread}?saved=tag-refused"), &people.reader).await,
        "Tags are one to forty letters, digits or hyphens.",
    );
    // A visitor sees the tag but no forms; the owner may remove it.
    let html = page(app, &thread, "").await;
    has(&html, ">release-notes</a>");
    lacks(&html, "Remove tag");
    let (status, location) = post(
        app,
        &action,
        &people.owner,
        &format!("thread={}&tag=release-notes&op=remove", seed.release),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location, format!("{thread}?saved=untagged"));
    lacks(&page(app, &thread, "").await, ">release-notes</a>");
    assert_eq!(
        call(app, "GET", &format!("{BASE}/tags/release-notes"), "", "")
            .await
            .status(),
        StatusCode::OK,
        "an empty tag page is a page"
    );
    assert_eq!(audit_count(db, "archive.tag").await, 2);
}

/// An owner files a thread under one of the list's categories.
async fn categories(db: &Database, app: &axum::Router, seed: &Seed, people: &People) {
    let thread = format!("{BASE}/thread/{}", seed.lunch);
    let action = format!("{BASE}/category");
    let html = page(app, &thread, &people.owner).await;
    has(&html, "<option value=\"planning\">");
    has(&html, "<option value=\"social\">");
    lacks(
        &page(app, &thread, &people.reader).await,
        "<option value=\"planning\">",
    );
    assert_eq!(
        post(
            app,
            &action,
            &people.reader,
            &format!("thread={}&category=social", seed.lunch)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, location) = post(
        app,
        &action,
        &people.owner,
        &format!("thread={}&category=social", seed.lunch),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location, format!("{thread}?saved=categorized"));
    let html = page(app, &location, &people.owner).await;
    has(&html, "The category was set.");
    has(&html, "<option value=\"social\" selected>");
    has(&html, &format!("{BASE}/categories/social\""));
    let listing = page(app, &format!("{BASE}/categories/social"), "").await;
    has(&listing, "Threads in social");
    has(&listing, "Lunch");
    lacks(&listing, "Release plan");
    has(
        &page(app, &format!("{BASE}/threads"), "").await,
        &format!("class=\"badge category\" href=\"{BASE}/categories/social\">social</a>"),
    );
    assert_eq!(
        post(
            app,
            &action,
            &people.owner,
            &format!("thread={}&category=absent", seed.lunch)
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    post(
        app,
        &action,
        &people.owner,
        &format!("thread={}&category=", seed.lunch),
    )
    .await;
    lacks(&page(app, &thread, "").await, "badge category");
    assert_eq!(audit_count(db, "archive.category").await, 2);
    assert_eq!(
        call(app, "GET", &format!("{BASE}/categories/absent"), "", "")
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
}

/// A reader's favourites are theirs alone and have their own page.
async fn favorites(app: &axum::Router, seed: &Seed, people: &People) {
    let thread = format!("{BASE}/thread/{}", seed.release);
    let action = format!("{BASE}/favorite");
    assert_eq!(
        call(app, "GET", &format!("{BASE}/favorites"), "", "")
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    has(
        &page(app, &format!("{BASE}/favorites"), &people.reader).await,
        "No threads on this page.",
    );
    has(
        &page(app, &thread, &people.reader).await,
        "name=\"on\" value=\"1\"",
    );
    let (status, location) = post(
        app,
        &action,
        &people.reader,
        &format!("thread={}&on=1", seed.release),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location, format!("{thread}?saved=favorited"));
    let html = page(app, &location, &people.reader).await;
    has(&html, "Added to your favourites.");
    has(&html, "name=\"on\" value=\"0\"");
    let mine = page(app, &format!("{BASE}/favorites"), &people.reader).await;
    has(&mine, "Your favourite threads");
    has(&mine, "Release plan");
    lacks(&mine, "Lunch");
    has(
        &page(app, &format!("{BASE}/favorites"), &people.other).await,
        "No threads on this page.",
    );
    post(
        app,
        &action,
        &people.reader,
        &format!("thread={}&on=0", seed.release),
    )
    .await;
    lacks(
        &page(app, &format!("{BASE}/favorites"), &people.reader).await,
        "Release plan",
    );
    assert_eq!(
        post(app, &action, &people.reader, "thread=absent&on=1")
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let _ = &seed.reply;
}

/// A private archive takes interactions from its verified members only.
async fn policy(db: &Database, app: &axum::Router, seed: &Seed, people: &People) {
    db.lists()
        .update(
            &"public.example.com".parse().unwrap(),
            &serde_json::json!({"archive_policy": "private"}),
        )
        .await
        .unwrap();
    user(db, "stranger@example.com", false).await;
    let stranger = login_as(app, "stranger@example.com").await;
    for (path, body) in [
        ("vote", format!("hash={}&value=1", seed.reply)),
        ("tags", format!("thread={}&tag=x&op=add", seed.release)),
        ("favorite", format!("thread={}&on=1", seed.release)),
    ] {
        assert_eq!(
            post(app, &format!("{BASE}/{path}"), &stranger, &body)
                .await
                .0,
            StatusCode::FORBIDDEN,
            "{path}"
        );
    }
    assert_eq!(
        call(app, "GET", &format!("{BASE}/favorites"), &stranger, "")
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post(
            app,
            &format!("{BASE}/vote"),
            &people.reader,
            &format!("hash={}&value=1", seed.reply)
        )
        .await
        .0,
        StatusCode::SEE_OTHER,
        "a verified member still votes"
    );
}
