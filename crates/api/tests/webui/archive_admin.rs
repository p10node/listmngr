//! The owner's archive administration: a page that manages the list's
//! categories and lists what is hidden, posts and threads hidden from the
//! reading pages and restored from there, and posts and threads deleted
//! outright with their attachments, votes, tags and marks — each owner
//! only, each audited in the transaction that makes the change.
use super::{call, csrf, login_as, member, seeded_fixture, text, user};
use axum::http::StatusCode;
use listmngr_core::MemberRole;
use listmngr_db::Database;
use listmngr_db::mail_queue::{NewMessage, Queue};

#[tokio::test]
async fn archive_admin_hides_deletes_and_manages_categories() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    matrix(db).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_archive_admin_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_archive_admin")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 3).await.unwrap();
    db.migrate().await.unwrap();
    matrix(db).await;
    schema.drop().await.unwrap();
}

const BASE: &str = "/web/lists/public.example.com/archive";

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

async fn count(db: &Database, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(db.pool()).await.unwrap()
}

async fn audit_count(db: &Database, action: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action=$1")
        .bind(action)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

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

/// A three-post thread (`root` → `middle` → `leaf`) and a single post.
struct Seed {
    root: String,
    middle: String,
    leaf: String,
    single: String,
}

async fn seed(db: &Database) -> Seed {
    db.lists()
        .update(
            &"public.example.com".parse().unwrap(),
            &serde_json::json!({"archive_policy": "public"}),
        )
        .await
        .unwrap();
    let root = archived(db, "<a1@example.org>", "Release plan", None).await;
    let middle = archived(
        db,
        "<a2@example.org>",
        "Re: Release plan",
        Some("<a1@example.org>"),
    )
    .await;
    let leaf = archived(
        db,
        "<a3@example.org>",
        "Re: Re: Release plan",
        Some("<a2@example.org>"),
    )
    .await;
    let single = archived(db, "<b1@example.org>", "Lunch", None).await;
    Seed {
        root,
        middle,
        leaf,
        single,
    }
}

struct People {
    member: String,
    owner: String,
}

async fn people(db: &Database, app: &axum::Router) -> People {
    for (email, role) in [
        ("reader@example.com", MemberRole::Member),
        ("owner@example.com", MemberRole::Owner),
    ] {
        user(db, email, false).await;
        member(db, email, role).await;
    }
    People {
        member: login_as(app, "reader@example.com").await,
        owner: login_as(app, "owner@example.com").await,
    }
}

async fn matrix(db: Database) {
    let (db, app) = seeded_fixture(db).await;
    let seed = seed(&db).await;
    let people = people(&db, &app).await;
    authority(&app, &seed, &people).await;
    categories(&db, &app, &seed, &people).await;
    hiding(&db, &app, &seed, &people).await;
    deleting(&db, &app, &seed, &people).await;
}

/// Every route needs a signed-in owner of the list.
async fn authority(app: &axum::Router, seed: &Seed, people: &People) {
    assert_eq!(
        call(app, "GET", &format!("{BASE}/admin"), "", "")
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(app, "GET", &format!("{BASE}/admin"), &people.member, "")
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    for (path, body) in [
        (
            format!("{BASE}/categories"),
            "op=add&name=releases".to_owned(),
        ),
        (
            format!("{BASE}/hide"),
            format!("scope=message&target={}&on=1", seed.single),
        ),
        (
            format!("{BASE}/delete"),
            format!("scope=message&target={}", seed.single),
        ),
    ] {
        assert_eq!(
            post(app, &path, &people.member, &body).await.0,
            StatusCode::FORBIDDEN,
            "{path}"
        );
    }
    // The reading pages offer the controls to the owner alone.
    let path = format!("{BASE}/thread/{}", seed.root);
    for cookie in ["", people.member.as_str()] {
        let html = page(app, &path, cookie).await;
        lacks(&html, &format!("{BASE}/hide"));
        lacks(&html, &format!("{BASE}/delete"));
        lacks(&html, &format!("{BASE}/admin"));
    }
    let owned = page(app, &path, &people.owner).await;
    has(&owned, &format!("action=\"{BASE}/hide\""));
    has(&owned, &format!("action=\"{BASE}/delete\""));
    has(&owned, &format!("href=\"{BASE}/admin\""));
    has(&owned, "value=\"message\"");
    has(&owned, "value=\"thread\"");
}

/// The list's categories are created, renamed and removed on the owner's
/// page, and a thread filed under a renamed one follows it.
async fn categories(db: &Database, app: &axum::Router, seed: &Seed, people: &People) {
    let admin = format!("{BASE}/admin");
    has(&page(app, &admin, &people.owner).await, "value=\"add\"");
    let (status, to) = post(
        app,
        &format!("{BASE}/categories"),
        &people.owner,
        "op=add&name=Release%20Notes",
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(to, format!("{admin}?saved=category-added"));
    let html = page(app, &format!("{admin}?saved=category-added"), &people.owner).await;
    has(&html, "release-notes");
    has(&html, "Category added");
    refused_categories(
        app,
        &people.owner,
        &["op=add&name=release-notes", "op=add&name=%21%21%21"],
    )
    .await;
    // File the single post's thread under it, then rename the category.
    let (_, _) = post(
        app,
        &format!("{BASE}/category"),
        &people.owner,
        &format!("thread={}&category=release-notes", seed.single),
    )
    .await;
    let (_, to) = post(
        app,
        &format!("{BASE}/categories"),
        &people.owner,
        "op=rename&name=release-notes&to=Shipping",
    )
    .await;
    assert_eq!(to, format!("{admin}?saved=category-renamed"));
    let filed: i64 = count(
        db,
        "SELECT COUNT(*) FROM archive_thread_categories WHERE category='shipping'",
    )
    .await;
    assert_eq!(filed, 1, "the thread followed the renamed category");
    has(
        &page(app, &format!("{BASE}/categories/shipping"), "").await,
        "Lunch",
    );
    // Removing a category unfiles its threads and leaves the posts alone.
    let (_, to) = post(
        app,
        &format!("{BASE}/categories"),
        &people.owner,
        "op=remove&name=shipping",
    )
    .await;
    assert_eq!(to, format!("{admin}?saved=category-removed"));
    assert_eq!(
        count(db, "SELECT COUNT(*) FROM archive_categories").await,
        0
    );
    assert_eq!(
        count(db, "SELECT COUNT(*) FROM archive_thread_categories").await,
        0
    );
    assert_eq!(
        call(app, "GET", &format!("{BASE}/categories/shipping"), "", "")
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        audit_count(db, "archive.category").await,
        4,
        "3 CRUD + 1 filing"
    );
    refused_categories(
        app,
        &people.owner,
        &["op=rename&name=absent&to=other", "op=remove&name=absent"],
    )
    .await;
}

/// Category changes that send the owner back with a refusal: a name the
/// list already has, one that normalises to nothing, and a rename or a
/// removal of one the list lacks.
async fn refused_categories(app: &axum::Router, owner: &str, bodies: &[&str]) {
    let admin = format!("{BASE}/admin");
    for body in bodies {
        let (_, to) = post(app, &format!("{BASE}/categories"), owner, body).await;
        assert_eq!(to, format!("{admin}?saved=category-refused"), "{body}");
    }
}

/// A hidden post leaves every reading surface and comes back from the
/// owner's page; hiding a thread hides all of its posts.
async fn hiding(db: &Database, app: &axum::Router, seed: &Seed, people: &People) {
    let permalink = format!("{BASE}?message={}", seed.middle);
    has(&page(app, &permalink, "").await, "Re: Release plan");
    let (status, to) = post(
        app,
        &format!("{BASE}/hide"),
        &people.owner,
        &format!("scope=message&target={}&on=1", seed.middle),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    // A hidden post cannot be read, so the owner lands where it is listed.
    assert_eq!(to, format!("{BASE}/admin?saved=hidden"));
    has(
        &page(app, &to, &people.owner).await,
        "hidden from the reading pages",
    );
    // Gone from the permalink, the thread, the lists, the overview, the
    // sender page, the search, the feed and the export — for everyone.
    assert_eq!(
        call(app, "GET", &permalink, &people.owner, "")
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    let post_id = format!("id=\"m-{}\"", seed.middle);
    let thread = page(app, &format!("{BASE}/thread/{}", seed.root), "").await;
    lacks(&thread, &post_id);
    has(&thread, &format!("id=\"m-{}\"", seed.leaf));
    for path in [
        format!("{BASE}/overview"),
        format!("{BASE}/threads"),
        format!("{BASE}?q=Release"),
        format!("{BASE}/feed.atom"),
    ] {
        lacks(&page(app, &path, "").await, &seed.middle);
    }
    let exported = call(app, "GET", &format!("{BASE}/export.mbox"), "", "").await;
    let bytes = axum::body::to_bytes(exported.into_body(), 1 << 20)
        .await
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&bytes).contains("Body of Re: Release plan."),
        "a hidden post stays out of the export"
    );
    // The owner's page lists it and puts it back.
    let admin = page(app, &format!("{BASE}/admin"), &people.owner).await;
    has(&admin, "Re: Release plan");
    has(&admin, &format!("value=\"{}\"", seed.middle));
    let (_, to) = post(
        app,
        &format!("{BASE}/hide"),
        &people.owner,
        &format!("scope=message&target={}&on=0", seed.middle),
    )
    .await;
    assert_eq!(to, format!("{permalink}&saved=shown"));
    has(
        &page(app, &to, &people.owner).await,
        "back on the reading pages",
    );
    has(&page(app, &permalink, "").await, "Re: Release plan");
    hiding_a_thread(db, app, seed, people).await;
}

/// A whole thread at once, and the bounds on a target.
async fn hiding_a_thread(db: &Database, app: &axum::Router, seed: &Seed, people: &People) {
    let (_, to) = post(
        app,
        &format!("{BASE}/hide"),
        &people.owner,
        &format!("scope=thread&target={}&on=1", seed.root),
    )
    .await;
    assert_eq!(to, format!("{BASE}/admin?saved=hidden"));
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM archive_messages WHERE hidden_at IS NOT NULL"
        )
        .await,
        3
    );
    assert_eq!(
        call(app, "GET", &format!("{BASE}/thread/{}", seed.root), "", "")
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    let (_, _) = post(
        app,
        &format!("{BASE}/hide"),
        &people.owner,
        &format!("scope=thread&target={}&on=0", seed.root),
    )
    .await;
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM archive_messages WHERE hidden_at IS NOT NULL"
        )
        .await,
        0
    );
    // An absent target is not found; an unknown scope is a bad request.
    assert_eq!(
        post(
            app,
            &format!("{BASE}/hide"),
            &people.owner,
            "scope=message&target=ABSENT&on=1"
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        post(
            app,
            &format!("{BASE}/hide"),
            &people.owner,
            &format!("scope=post&target={}&on=1", seed.single)
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(audit_count(db, "archive.hide").await, 2);
    assert_eq!(audit_count(db, "archive.unhide").await, 2);
}

/// Deleting a post takes its attachments and votes with it, splices its
/// replies onto its parent, and re-roots the thread when the root goes;
/// deleting a thread takes every post and every mark of it.
async fn deleting(db: &Database, app: &axum::Router, seed: &Seed, people: &People) {
    // A vote and a tag on the thread, to be taken with it.
    let _ = post(
        app,
        &format!("{BASE}/vote"),
        &people.member,
        &format!("hash={}&value=1", seed.leaf),
    )
    .await;
    let _ = post(
        app,
        &format!("{BASE}/tags"),
        &people.member,
        &format!("thread={}&op=add&tag=release", seed.root),
    )
    .await;
    // The leaf: gone, with its vote.
    let (status, to) = post(
        app,
        &format!("{BASE}/delete"),
        &people.owner,
        &format!("scope=message&target={}", seed.leaf),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(to, format!("{BASE}/threads?saved=deleted"));
    // The redirect lands on a page, with its notice.
    has(&page(app, &to, &people.owner).await, "posts are deleted");
    assert_eq!(
        count(db, "SELECT COUNT(*) FROM archive_messages").await,
        3,
        "the leaf is gone"
    );
    assert_eq!(count(db, "SELECT COUNT(*) FROM archive_votes").await, 0);
    // The root of a thread that still has a reply: the reply becomes the
    // new root and the thread's tag follows it.
    let (_, _) = post(
        app,
        &format!("{BASE}/delete"),
        &people.owner,
        &format!("scope=message&target={}", seed.root),
    )
    .await;
    assert_eq!(count(db, "SELECT COUNT(*) FROM archive_messages").await, 2);
    let rerooted: String = sqlx::query_scalar("SELECT thread FROM archive_messages WHERE hash=$1")
        .bind(&seed.middle)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(rerooted, seed.middle, "the reply became its own root");
    let parent: Option<String> =
        sqlx::query_scalar("SELECT parent_hash FROM archive_messages WHERE hash=$1")
            .bind(&seed.middle)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(parent, None, "its parent went with the deleted post");
    let tagged: String = sqlx::query_scalar("SELECT thread FROM archive_tags")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(
        tagged, seed.middle,
        "the thread's tag followed the new root"
    );
    has(
        &page(app, &format!("{BASE}/thread/{}", seed.middle), "").await,
        "Re: Release plan",
    );
    deleting_a_thread(db, app, seed, people).await;
}

/// A whole thread goes with every mark on it; the single post survives.
async fn deleting_a_thread(db: &Database, app: &axum::Router, seed: &Seed, people: &People) {
    let (_, to) = post(
        app,
        &format!("{BASE}/delete"),
        &people.owner,
        &format!("scope=thread&target={}", seed.middle),
    )
    .await;
    assert_eq!(to, format!("{BASE}/threads?saved=deleted"));
    assert_eq!(count(db, "SELECT COUNT(*) FROM archive_messages").await, 1);
    assert_eq!(count(db, "SELECT COUNT(*) FROM archive_tags").await, 0);
    assert_eq!(
        call(
            app,
            "GET",
            &format!("{BASE}/thread/{}", seed.middle),
            "",
            ""
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    // The single post survives all of it; an absent target is not found.
    has(
        &page(app, &format!("{BASE}/thread/{}", seed.single), "").await,
        "Lunch",
    );
    assert_eq!(
        post(
            app,
            &format!("{BASE}/delete"),
            &people.owner,
            "scope=thread&target=ABSENT"
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(audit_count(db, "archive.delete").await, 3);
}
