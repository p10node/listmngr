//! `/web/moderation` as one queue across every list the reader moderates:
//! the per-list counts, every held post and every undecided request with
//! their decision forms, each decision returning to this page.
use super::{call, csrf, fixture, login_as, member, seeded_fixture, text, user};
use axum::http::StatusCode;
use listmngr_core::{ListId, MemberRole};
use listmngr_db::Database;
use listmngr_db::mail_queue::{NewMessage, Queue};
use listmngr_db::moderation::HeldId;

#[tokio::test]
async fn cross_list_moderation() {
    let (db, app) = fixture().await;
    matrix(db, app).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_moderation_cross_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_moderation_cross")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 3).await.unwrap();
    db.migrate().await.unwrap();
    let (db, app) = seeded_fixture(db).await;
    matrix(db, app).await;
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

/// A post held on `list` with `subject`.
async fn hold(db: &Database, list: &str, subject: &str) -> HeldId {
    let id: ListId = list.parse().unwrap();
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: format!("From: sender@example.com\r\nSubject: {subject}\r\n\r\nbody\r\n")
                    .into_bytes(),
                external_id: uuid::Uuid::now_v7().to_string(),
                context: serde_json::json!({"version":1,"list_id":list,"envelope_sender":"sender@example.com"}).to_string(),
                queue: Queue::In,
                max_attempts: 5,
            },
            1000,
        )
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::In, "test-web", 1000, 10000)
        .await
        .unwrap()
        .unwrap();
    db.moderation()
        .hold(
            &lease,
            &id,
            "sender@example.com",
            subject,
            "nonmember",
            1000,
        )
        .await
        .unwrap()
        .id
}

async fn matrix(db: Database, app: axum::Router) {
    let (public_a, public_b, private_a) = seed(&db).await;
    let moderator = login_as(&app, "mod@example.com").await;
    let plain = login_as(&app, "plain@example.com").await;
    visitor(&app, &plain).await;
    let token = aggregate(&app, &moderator).await;
    held_decisions(&db, &app, &moderator, &token, public_a, private_a).await;
    request_decision(&db, &app, &moderator, &token).await;
    guards(&app, &plain, &moderator, public_b).await;
}

/// A moderator of the public list who owns the private one, a plain member,
/// two posts held on the public list, one on the private, and a moderated
/// join request on the private list.
async fn seed(db: &Database) -> (HeldId, HeldId, HeldId) {
    user(db, "mod@example.com", false).await;
    member(db, "mod@example.com", MemberRole::Moderator).await;
    db.members()
        .create(listmngr_db::NewMember {
            list_id: "private.example.com".parse().unwrap(),
            email: "mod@example.com".into(),
            role: MemberRole::Owner,
            subscription_mode: listmngr_core::SubscriptionMode::AsAddress,
            display_name: String::new(),
        })
        .await
        .unwrap();
    user(db, "plain@example.com", false).await;
    member(db, "plain@example.com", MemberRole::Member).await;
    let public_a = hold(db, "public.example.com", "Public one").await;
    let public_b = hold(db, "public.example.com", "Public two").await;
    let private_a = hold(db, "private.example.com", "Private one <b>").await;
    let private: ListId = "private.example.com".parse().unwrap();
    db.lists()
        .update(
            &private,
            &serde_json::json!({"subscription_policy": "moderate"}),
        )
        .await
        .unwrap();
    db.workflows()
        .subscribe(
            &listmngr_db::workflows::AdminSubscription {
                list: &private,
                email: "pending@example.org",
                display_name: "Pending",
                pre_verified: true,
                pre_confirmed: true,
                pre_approved: false,
                invitation: false,
            },
            &listmngr_db::AuditContext::system(),
            1000,
        )
        .await
        .unwrap();
    (public_a, public_b, private_a)
}

/// A plain member moderates nothing and sees both empty states.
async fn visitor(app: &axum::Router, plain: &str) {
    let html = page(app, "/web/moderation", plain).await;
    has(&html, "No post waits on your lists");
    has(&html, "No request waits on your lists");
    assert_eq!(html.matches("<article data-held=").count(), 0);
}

/// The moderator's page: the counts, every held post and request across
/// both lists, each naming its list, the forms returning here, no bulk form.
async fn aggregate(app: &axum::Router, moderator: &str) -> String {
    let response = call(app, "GET", "/web/moderation", moderator, "").await;
    assert_eq!(response.status(), StatusCode::OK);
    let csp = response.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .to_owned();
    has(&csp, "script-src 'self'");
    let html = text(response).await;
    for needle in [
        "/web/moderation.js",
        "2 held",
        "1 held",
        "Private one &lt;b&gt;",
        "Public one",
        "href=\"/web/lists/private.example.com/held\">private.example.com</a>",
        "pending@example.org",
        "name=\"back\" value=\"moderation\"",
    ] {
        has(&html, needle);
    }
    assert_eq!(html.matches("<article data-held=").count(), 3);
    lacks(&html, "form=\"bulk\"");
    csrf(&html)
}

/// A decision with `back=moderation` returns here and takes effect; one
/// without returns to the list's queue as before.
async fn held_decisions(
    db: &Database,
    app: &axum::Router,
    moderator: &str,
    token: &str,
    public_a: HeldId,
    private_a: HeldId,
) {
    let response = call(
        app,
        "POST",
        &format!("/web/lists/public.example.com/held/{}", public_a.0),
        moderator,
        &serde_urlencoded::to_string([
            ("csrf", token),
            ("action", "accept"),
            ("comment", ""),
            ("forward_to", ""),
            ("back", "moderation"),
        ])
        .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()["location"], "/web/moderation?done=1");
    assert_eq!(
        db.moderation().get(public_a).await.unwrap().disposition,
        Some(listmngr_db::moderation::Disposition::Accepted)
    );
    let html = page(app, "/web/moderation?done=1", moderator).await;
    has(&html, "1 decided");
    assert_eq!(html.matches("<article data-held=").count(), 2);
    let response = call(
        app,
        "POST",
        &format!("/web/lists/private.example.com/held/{}", private_a.0),
        moderator,
        &serde_urlencoded::to_string([
            ("csrf", token),
            ("action", "discard"),
            ("comment", ""),
            ("forward_to", ""),
        ])
        .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()["location"],
        "/web/lists/private.example.com/held?done=1"
    );
}

/// The request accepted from here returns here and seats the member.
async fn request_decision(db: &Database, app: &axum::Router, moderator: &str, token: &str) {
    let request_id: String = sqlx::query_scalar(
        "SELECT id FROM subscription_workflows WHERE list_id='private.example.com'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    let response = call(
        app,
        "POST",
        &format!("/web/lists/private.example.com/requests/{request_id}"),
        moderator,
        &serde_urlencoded::to_string([
            ("csrf", token),
            ("decision", "accept"),
            ("reason", ""),
            ("back", "moderation"),
        ])
        .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()["location"], "/web/moderation?saved=1");
    let html = page(app, "/web/moderation?saved=1", moderator).await;
    has(&html, "decision was recorded");
    has(&html, "No request waits on your lists");
    assert_eq!(html.matches("<article data-held=").count(), 1);
    let accepted: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id='private.example.com' AND a.email='pending@example.org' AND m.role='member'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(accepted, 1);
}

/// `back` grants nothing: authority stays the list's; a page past the limit
/// is refused.
async fn guards(app: &axum::Router, plain: &str, moderator: &str, public_b: HeldId) {
    assert_eq!(
        call(
            app,
            "POST",
            &format!("/web/lists/public.example.com/held/{}", public_b.0),
            plain,
            "action=accept&back=moderation"
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(app, "GET", "/web/moderation?page=10001", moderator, "")
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
}
