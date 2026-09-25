//! Webhooks from the browser: a list owner's page under the list's
//! settings and a server owner's under admin; a secret shown once, on
//! creation and rotation; deliveries on the webhook's own page.
use super::{call, csrf, login_as, member, seeded_fixture_configured, text, user};
use axum::http::StatusCode;
use listmngr_core::MemberRole;
use listmngr_db::Database;

const KEY: &str = "0123456789abcdef0123456789abcdef";
const ROOT: &str = "/web/lists/public.example.com/settings/webhooks";
const SITE: &str = "/web/admin/webhooks";

async fn keyed_fixture(db: Database) -> (Database, axum::Router) {
    seeded_fixture_configured(db, |config| {
        config.webhooks.signing_key = Some(KEY.into());
    })
    .await
}

fn encode(fields: &[(&str, &str)]) -> String {
    serde_urlencoded::to_string(fields).unwrap()
}

async fn page(app: &axum::Router, path: &str, cookie: &str) -> String {
    let response = call(app, "GET", path, cookie, "").await;
    assert_eq!(response.status(), StatusCode::OK, "{path}");
    text(response).await
}

async fn post(
    app: &axum::Router,
    path: &str,
    cookie: &str,
    body: &str,
) -> axum::response::Response {
    call(app, "POST", path, cookie, body).await
}

async fn count(db: &Database, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(db.pool()).await.unwrap()
}

fn shown_secret(html: &str) -> String {
    html.split("<code id=\"webhook-secret\">")
        .nth(1)
        .expect("the secret is shown")
        .split('<')
        .next()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn webhooks_from_the_browser() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let (db, app) = keyed_fixture(db).await;
    scenario(db, app).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_webhooks_web_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_webhooks")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 3).await.unwrap();
    db.migrate().await.unwrap();
    let (db, app) = keyed_fixture(db).await;
    scenario(db, app).await;
    schema.drop().await.unwrap();
}

#[allow(clippy::too_many_lines, clippy::cognitive_complexity)] // One ordered contract per backend.
async fn scenario(db: Database, app: axum::Router) {
    user(&db, "hooks-owner@example.com", false).await;
    member(&db, "hooks-owner@example.com", MemberRole::Owner).await;
    let owner = login_as(&app, "hooks-owner@example.com").await;
    user(&db, "hooks-member@example.com", false).await;
    member(&db, "hooks-member@example.com", MemberRole::Member).await;
    let outsider = login_as(&app, "hooks-member@example.com").await;
    // The page, in the settings navigation, empty; an owner's only.
    let html = page(&app, ROOT, &owner).await;
    assert!(
        html.contains(
            r#"href="/web/lists/public.example.com/settings/webhooks" aria-current="page""#
        ),
        "{html}"
    );
    assert!(html.contains("No webhook yet"), "{html}");
    assert_eq!(
        call(&app, "GET", ROOT, &outsider, "").await.status(),
        StatusCode::FORBIDDEN
    );
    let token = csrf(&html);
    // An `http://` target is refused inline, and nothing is made.
    let refused = post(
        &app,
        &format!("{ROOT}/add"),
        &owner,
        &encode(&[
            ("csrf", &token),
            ("url", "http://hooks.example.invalid/x"),
            ("events", "*"),
        ]),
    )
    .await;
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let refused = text(refused).await;
    assert!(refused.contains("id=\"url-error\""), "{refused}");
    assert!(
        refused.contains("value=\"http://hooks.example.invalid/x\""),
        "the draft is kept"
    );
    assert_eq!(count(&db, "SELECT COUNT(*) FROM webhooks").await, 0);
    // Added: the secret shown this once.
    let added = post(
        &app,
        &format!("{ROOT}/add"),
        &owner,
        &encode(&[
            ("csrf", &token),
            ("url", "https://hooks.example.invalid/dev"),
            ("events", "member.*, list.config"),
            ("description", "ops"),
        ]),
    )
    .await;
    assert_eq!(added.status(), StatusCode::OK);
    let html = text(added).await;
    let secret = shown_secret(&html);
    assert_eq!(secret.len(), 64, "{secret}");
    assert!(
        html.contains("member.*, list.config") && html.contains("ops"),
        "{html}"
    );
    let id: String = sqlx::query_scalar("SELECT id FROM webhooks")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let html = page(&app, ROOT, &owner).await;
    assert!(
        !html.contains(&secret) && !html.contains("webhook-secret"),
        "the secret is shown once"
    );
    assert!(html.contains("hooks.example.invalid/dev"), "{html}");
    assert!(html.contains("public.example.com — enabled"), "{html}");
    // Disabled, then enabled again.
    let enable = post(
        &app,
        &format!("{ROOT}/{id}/enable"),
        &owner,
        &encode(&[("csrf", &token), ("enabled", "false")]),
    )
    .await;
    assert_eq!(enable.status(), StatusCode::SEE_OTHER);
    assert!(page(&app, ROOT, &owner).await.contains("— disabled"));
    assert_eq!(
        post(
            &app,
            &format!("{ROOT}/{id}/enable"),
            &owner,
            &encode(&[("csrf", &token), ("enabled", "true")]),
        )
        .await
        .status(),
        StatusCode::SEE_OTHER
    );
    // Pinged: the delivery on the webhook's own page.
    let pinged = post(
        &app,
        &format!("{ROOT}/{id}/ping"),
        &owner,
        &encode(&[("csrf", &token)]),
    )
    .await;
    assert_eq!(pinged.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        pinged.headers()["location"].to_str().unwrap(),
        format!("{ROOT}/{id}?pinged=1")
    );
    let html = page(&app, &format!("{ROOT}/{id}?pinged=1"), &owner).await;
    assert!(html.contains("A ping is queued"), "{html}");
    assert!(
        html.contains("<code>ping</code>") && html.contains("<td>pending</td>"),
        "{html}"
    );
    // Rotated: a new secret, shown this once.
    let rotated = post(
        &app,
        &format!("{ROOT}/{id}/rotate"),
        &owner,
        &encode(&[("csrf", &token)]),
    )
    .await;
    assert_eq!(rotated.status(), StatusCode::OK);
    let rotated = shown_secret(&text(rotated).await);
    assert_ne!(rotated, secret);
    assert_eq!(rotated.len(), 64);
    // The site page: a server owner's; the list owner is refused; every
    // webhook is listed with its list, and a site-wide one is made there.
    assert_eq!(
        call(&app, "GET", SITE, &owner, "").await.status(),
        StatusCode::FORBIDDEN
    );
    user(&db, "hooks-site@example.com", true).await;
    let site = login_as(&app, "hooks-site@example.com").await;
    let html = page(&app, SITE, &site).await;
    assert!(
        html.contains("hooks.example.invalid/dev") && html.contains("public.example.com"),
        "{html}"
    );
    let site_token = csrf(&html);
    let added = post(
        &app,
        &format!("{SITE}/add"),
        &site,
        &encode(&[
            ("csrf", &site_token),
            ("url", "https://hooks.example.invalid/site"),
            ("events", "*"),
        ]),
    )
    .await;
    assert_eq!(added.status(), StatusCode::OK);
    let html = text(added).await;
    assert_eq!(shown_secret(&html).len(), 64);
    assert!(html.contains("— site-wide"), "{html}");
    let site_id: String = sqlx::query_scalar("SELECT id FROM webhooks WHERE list_id IS NULL")
        .fetch_one(db.pool())
        .await
        .unwrap();
    // A site-wide webhook is not the list owner's to see.
    assert_eq!(
        call(&app, "GET", &format!("{ROOT}/{site_id}"), &owner, "")
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        post(
            &app,
            &format!("{ROOT}/{site_id}/remove"),
            &owner,
            &encode(&[("csrf", &token)]),
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    // Without the session's CSRF token nothing is written.
    let forged = post(
        &app,
        &format!("{ROOT}/{id}/remove"),
        &owner,
        &encode(&[("csrf", "forged")]),
    )
    .await;
    assert_ne!(forged.status(), StatusCode::SEE_OTHER);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM webhooks").await, 2);
    // Removed, with its deliveries; the site's stays.
    assert_eq!(
        post(
            &app,
            &format!("{ROOT}/{id}/remove"),
            &owner,
            &encode(&[("csrf", &token)]),
        )
        .await
        .status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(count(&db, "SELECT COUNT(*) FROM webhooks").await, 1);
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM webhook_deliveries WHERE event='ping'"
        )
        .await,
        0
    );
    assert!(page(&app, ROOT, &owner).await.contains("No webhook yet"));
    // Every change was audited with the signed-in user as its actor, and
    // the admin index links the site page.
    assert!(
        count(
            &db,
            "SELECT COUNT(*) FROM audit_log WHERE action LIKE 'webhook.%' AND actor_user_id IS NOT NULL"
        )
        .await
            >= 7
    );
    assert!(
        page(&app, "/web/admin", &site)
            .await
            .contains("href=\"/web/admin/webhooks\"")
    );
}
