//! The moderator's queues: held posts with a rendered preview, single and
//! bulk decisions with a reason and a forward, the sender's moderation and
//! ban from the post, the header-rule shortcut, keyboard shortcuts from a
//! first-party script, and the subscription requests queue.
use super::{call, csrf, fixture, held, login_as, member, seeded_fixture, text, user};
use axum::http::StatusCode;
use listmngr_core::MemberRole;
use listmngr_db::Database;
use listmngr_db::workflows::AdminSubscription;

const HELD: &str = "/web/lists/public.example.com/held";
const REQUESTS: &str = "/web/lists/public.example.com/requests";

#[tokio::test]
async fn held_queue_and_requests_for_moderators() {
    let (db, app) = fixture().await;
    matrix(db, app).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_held_queue_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_held_queue")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 3).await.unwrap();
    db.migrate().await.unwrap();
    let (db, app) = seeded_fixture(db).await;
    matrix(db, app).await;
    schema.drop().await.unwrap();
}

async fn count(db: &Database, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(db.pool()).await.unwrap()
}

fn encode(fields: &[(&str, &str)]) -> String {
    serde_urlencoded::to_string(fields).unwrap()
}

async fn page(app: &axum::Router, path: &str, cookie: &str) -> String {
    let response = call(app, "GET", path, cookie, "").await;
    assert_eq!(response.status(), StatusCode::OK, "{path}");
    text(response).await
}

async fn matrix(db: Database, app: axum::Router) {
    user(&db, "queue-mod@example.com", false).await;
    member(&db, "queue-mod@example.com", MemberRole::Moderator).await;
    member(&db, "queue-reader@example.com", MemberRole::Member).await;
    let moderator = login_as(&app, "queue-mod@example.com").await;
    user(&db, "queue-outsider@example.com", false).await;
    member(&db, "queue-outsider@example.com", MemberRole::Member).await;
    let outsider = login_as(&app, "queue-outsider@example.com").await;

    preview(&db, &app, &moderator, &outsider).await;
    single_decisions(&db, &app, &moderator).await;
    bulk(&db, &app, &moderator).await;
    sender_actions(&db, &app, &moderator).await;
    requests(&db, &app, &moderator, &outsider).await;
    index_counts(&db, &app, &moderator).await;
}

/// The queue renders each post: decoded headers, the text body, the raw
/// source, the forms and the script; outsiders are refused.
async fn preview(db: &Database, app: &axum::Router, moderator: &str, outsider: &str) {
    let id = held(db).await;
    let html = page(app, HELD, moderator).await;
    assert!(html.contains("&lt;script&gt;held&lt;/script&gt;"), "{html}");
    assert!(!html.contains("<script>held"));
    assert!(
        html.contains("Untrusted &lt;b&gt;body&lt;/b&gt;"),
        "rendered body: {html}"
    );
    assert!(html.contains("class=\"preview\""), "{html}");
    assert!(html.contains("sender@example.com"), "{html}");
    assert!(html.contains("name=\"forward_to\""), "{html}");
    assert!(html.contains("name=\"held\""), "bulk selection: {html}");
    assert!(html.contains(&format!("{HELD}/{}/sender", id.0)), "{html}");
    assert!(html.contains(&format!("{HELD}/{}/ban", id.0)), "{html}");
    assert!(
        html.contains(
            "/web/lists/public.example.com/settings/header-matches?header=From&amp;pattern="
        ),
        "header-rule shortcut: {html}"
    );
    assert!(
        html.contains("/web/moderation.js"),
        "keyboard shortcuts: {html}"
    );
    assert!(html.contains("data-held=\""), "{html}");
    assert_eq!(
        call(app, "GET", HELD, outsider, "").await.status(),
        StatusCode::FORBIDDEN
    );
    let script = call(app, "GET", "/web/moderation.js", "", "").await;
    assert_eq!(script.status(), StatusCode::OK);
    assert!(
        script.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/javascript")
    );
    // Leave it held for the next step.
    let _ = id;
}

/// One decision with a reason and a forward; a rejection carries its reason.
#[allow(clippy::too_many_lines, clippy::cognitive_complexity)] // One ordered contract per page.
async fn single_decisions(db: &Database, app: &axum::Router, moderator: &str) {
    let html = page(app, HELD, moderator).await;
    let token = csrf(&html);
    let first: (String,) = sqlx::query_as(
        "SELECT id FROM held_messages WHERE list_id='public.example.com' AND disposition IS NULL ORDER BY hold_date,id LIMIT 1",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    let jobs_before = count(db, "SELECT COUNT(*) FROM queue_jobs").await;
    let done = call(
        app,
        "POST",
        &format!("{HELD}/{}", first.0),
        moderator,
        &encode(&[
            ("csrf", &token),
            ("action", "defer"),
            ("comment", "still thinking"),
            ("forward_to", "reviewer@example.org"),
        ]),
    )
    .await;
    assert_eq!(done.status(), StatusCode::SEE_OTHER, "{}", text(done).await);
    let forwarded: (String, Option<String>) = sqlx::query_as(
        "SELECT action, forward_to FROM moderation_log WHERE held_id=$1 ORDER BY at DESC LIMIT 1",
    )
    .bind(&first.0)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        forwarded,
        ("defer".into(), Some("reviewer@example.org".into()))
    );
    assert_eq!(
        count(db, "SELECT COUNT(*) FROM queue_jobs").await,
        jobs_before + 1,
        "the forward is queued"
    );
    assert!(
        db.moderation()
            .get(listmngr_db::moderation::HeldId(first.0.parse().unwrap()))
            .await
            .unwrap()
            .disposition
            .is_none()
    );
    // A forward to the list itself is refused inline and changes nothing.
    let refused = call(
        app,
        "POST",
        &format!("{HELD}/{}", first.0),
        moderator,
        &encode(&[
            ("csrf", &token),
            ("action", "defer"),
            ("comment", ""),
            ("forward_to", "public@example.com"),
        ]),
    )
    .await;
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    assert!(text(refused).await.contains("id=\"forward_to-error\""));
    // Rejected with a reason.
    let rejected = call(
        app,
        "POST",
        &format!("{HELD}/{}", first.0),
        moderator,
        &encode(&[
            ("csrf", &token),
            ("action", "reject"),
            ("comment", "Off topic"),
            ("forward_to", ""),
        ]),
    )
    .await;
    assert_eq!(rejected.status(), StatusCode::SEE_OTHER);
    let logged: (String, String) = sqlx::query_as(
        "SELECT action, reason FROM moderation_log WHERE held_id=$1 ORDER BY at DESC LIMIT 1",
    )
    .bind(&first.0)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(logged, ("rejected".into(), "Off topic".into()));
    let landing = page(app, &format!("{HELD}?done=1"), moderator).await;
    assert!(landing.contains("1 decided"), "{landing}");
}

/// Several posts at once: the selected ones get the decision, an already
/// decided one is skipped and counted.
#[allow(clippy::too_many_lines, clippy::cognitive_complexity)] // One ordered contract per page.
async fn bulk(db: &Database, app: &axum::Router, moderator: &str) {
    let a = held(db).await;
    let b = held(db).await;
    let c = held(db).await;
    let token = csrf(&page(app, HELD, moderator).await);
    let body = format!(
        "csrf={token}&held={}&held={}&action=accept&comment=&forward_to=",
        a.0, b.0
    );
    let response = call(app, "POST", HELD, moderator, &body).await;
    assert_eq!(
        response.status(),
        StatusCode::SEE_OTHER,
        "{}",
        text(response).await
    );
    let location = response.headers()["location"].to_str().unwrap().to_owned();
    assert!(location.contains("done=2"), "{location}");
    assert!(db.moderation().get(a).await.unwrap().disposition.is_some());
    assert!(db.moderation().get(b).await.unwrap().disposition.is_some());
    assert!(db.moderation().get(c).await.unwrap().disposition.is_none());
    // A repeated selection is skipped, not an error.
    let body = format!(
        "csrf={token}&held={}&held={}&action=discard&comment=&forward_to=",
        a.0, c.0
    );
    let response = call(app, "POST", HELD, moderator, &body).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let location = response.headers()["location"].to_str().unwrap().to_owned();
    assert!(
        location.contains("done=1") && location.contains("skipped=1"),
        "{location}"
    );
    let landing = page(app, &location.replace("http://localhost", ""), moderator).await;
    assert!(landing.contains("1 decided"), "{landing}");
    assert!(landing.contains("1 skipped"), "{landing}");
    assert_eq!(
        count(db, "SELECT COUNT(*) FROM audit_log WHERE action IN ('moderation.accept','moderation.discarded')").await,
        3
    );
    // Nothing selected is a refusal.
    let none = call(
        app,
        "POST",
        HELD,
        moderator,
        &format!("csrf={token}&action=accept&comment=&forward_to="),
    )
    .await;
    assert_eq!(none.status(), StatusCode::BAD_REQUEST);
}

/// From a held post: moderate its sender (a nonmember row when the sender
/// is not a member), and ban the sender.
#[allow(clippy::too_many_lines, clippy::cognitive_complexity)] // One ordered contract per page.
async fn sender_actions(db: &Database, app: &axum::Router, moderator: &str) {
    let id = held(db).await;
    let token = csrf(&page(app, HELD, moderator).await);
    assert_eq!(
        count(db, "SELECT COUNT(*) FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id='public.example.com' AND a.email='sender@example.com'").await,
        0
    );
    let moderated = call(
        app,
        "POST",
        &format!("{HELD}/{}/sender", id.0),
        moderator,
        &encode(&[("csrf", &token), ("action", "discard")]),
    )
    .await;
    assert_eq!(
        moderated.status(),
        StatusCode::SEE_OTHER,
        "{}",
        text(moderated).await
    );
    let row: (String, Option<String>) = sqlx::query_as(
        "SELECT m.role, m.moderation_action FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id='public.example.com' AND a.email='sender@example.com'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(row, ("nonmember".into(), Some("discard".into())));
    // Again with another action updates the same row.
    call(
        app,
        "POST",
        &format!("{HELD}/{}/sender", id.0),
        moderator,
        &encode(&[("csrf", &token), ("action", "hold")]),
    )
    .await;
    let row: (Option<String>,) = sqlx::query_as(
        "SELECT m.moderation_action FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id='public.example.com' AND a.email='sender@example.com'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(row.0.as_deref(), Some("hold"));
    assert_eq!(
        count(db, "SELECT COUNT(*) FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id='public.example.com' AND a.email='sender@example.com'").await,
        1
    );
    let html = page(app, HELD, moderator).await;
    assert!(
        html.contains("value=\"hold\" selected"),
        "the sender's current action is shown: {html}"
    );
    // Ban.
    let banned = call(
        app,
        "POST",
        &format!("{HELD}/{}/ban", id.0),
        moderator,
        &encode(&[("csrf", &token)]),
    )
    .await;
    assert_eq!(banned.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        count(db, "SELECT COUNT(*) FROM bans WHERE list_id='public.example.com' AND email_or_regex='sender@example.com'").await,
        1
    );
    let html = page(app, HELD, moderator).await;
    assert!(html.contains("already banned"), "{html}");
    // The post itself is still held; discard it to keep the queue clean.
    call(
        app,
        "POST",
        &format!("{HELD}/{}", id.0),
        moderator,
        &encode(&[
            ("csrf", &token),
            ("action", "discard"),
            ("comment", ""),
            ("forward_to", ""),
        ]),
    )
    .await;
}

/// The requests queue: a moderated request accepted, one rejected with a
/// reason, an invitation shown as waiting for the address.
#[allow(clippy::too_many_lines, clippy::cognitive_complexity)] // One ordered contract per page.
async fn requests(db: &Database, app: &axum::Router, moderator: &str, outsider: &str) {
    let list: listmngr_core::ListId = "public.example.com".parse().unwrap();
    db.lists()
        .update(
            &list,
            &serde_json::json!({"subscription_policy": "moderate"}),
        )
        .await
        .unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    for email in ["wants-in@example.org", "also-wants@example.org"] {
        db.workflows()
            .subscribe(
                &AdminSubscription {
                    list: &list,
                    email,
                    display_name: "Wants In",
                    pre_verified: true,
                    pre_confirmed: true,
                    pre_approved: false,
                    invitation: false,
                },
                &listmngr_db::AuditContext::system(),
                now,
            )
            .await
            .unwrap();
    }
    db.workflows()
        .subscribe(
            &AdminSubscription {
                list: &list,
                email: "invited@example.org",
                display_name: "",
                pre_verified: false,
                pre_confirmed: false,
                pre_approved: false,
                invitation: true,
            },
            &listmngr_db::AuditContext::system(),
            now,
        )
        .await
        .unwrap();
    let html = page(app, REQUESTS, moderator).await;
    assert!(html.contains("wants-in@example.org"), "{html}");
    assert!(html.contains("invited@example.org"), "{html}");
    assert!(html.contains("waiting for the address"), "{html}");
    assert_eq!(
        call(app, "GET", REQUESTS, outsider, "").await.status(),
        StatusCode::FORBIDDEN
    );
    let token = csrf(&html);
    let ids: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, original_email FROM subscription_workflows WHERE list_id='public.example.com' AND state='pending_moderation' ORDER BY created_at,id",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(ids.len(), 2);
    let accepted = call(
        app,
        "POST",
        &format!("{REQUESTS}/{}", ids[0].0),
        moderator,
        &encode(&[("csrf", &token), ("decision", "accept"), ("reason", "")]),
    )
    .await;
    assert_eq!(
        accepted.status(),
        StatusCode::SEE_OTHER,
        "{}",
        text(accepted).await
    );
    assert_eq!(
        count(db, "SELECT COUNT(*) FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id='public.example.com' AND a.email='wants-in@example.org' AND m.role='member'").await,
        1
    );
    let rejected = call(
        app,
        "POST",
        &format!("{REQUESTS}/{}", ids[1].0),
        moderator,
        &encode(&[
            ("csrf", &token),
            ("decision", "reject"),
            ("reason", "Not this list"),
        ]),
    )
    .await;
    assert_eq!(rejected.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        count(db, "SELECT COUNT(*) FROM audit_log WHERE action='subscription.reject' AND diff LIKE '%Not this list%'").await,
        1
    );
    // A request of another list is not reachable here.
    let foreign: listmngr_core::ListId = "private.example.com".parse().unwrap();
    db.lists()
        .update(
            &foreign,
            &serde_json::json!({"subscription_policy": "moderate"}),
        )
        .await
        .unwrap();
    db.workflows()
        .subscribe(
            &AdminSubscription {
                list: &foreign,
                email: "elsewhere@example.org",
                display_name: "",
                pre_verified: true,
                pre_confirmed: true,
                pre_approved: false,
                invitation: false,
            },
            &listmngr_db::AuditContext::system(),
            now,
        )
        .await
        .unwrap();
    let other: (String,) =
        sqlx::query_as("SELECT id FROM subscription_workflows WHERE list_id='private.example.com'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        call(
            app,
            "POST",
            &format!("{REQUESTS}/{}", other.0),
            moderator,
            &encode(&[("csrf", &token), ("decision", "accept"), ("reason", "")])
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    let html = page(app, REQUESTS, moderator).await;
    assert!(!html.contains("wants-in@example.org"), "{html}");
    assert!(html.contains("invited@example.org"), "{html}");
    db.lists()
        .update(&list, &serde_json::json!({"subscription_policy": "open"}))
        .await
        .unwrap();
}

/// The moderation index counts what waits on each list.
async fn index_counts(db: &Database, app: &axum::Router, moderator: &str) {
    held(db).await;
    let html = page(app, "/web/moderation", moderator).await;
    assert!(html.contains(HELD), "{html}");
    assert!(html.contains(REQUESTS), "{html}");
    assert!(html.contains("1 held"), "{html}");
    assert!(html.contains("1 request"), "{html}");
}
