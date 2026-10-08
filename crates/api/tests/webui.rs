#[path = "webui/addresses.rs"]
mod addresses;
#[path = "webui/admin_pagination.rs"]
mod admin_pagination;
#[path = "webui/archive_admin.rs"]
mod archive_admin;
#[path = "webui/archive_export.rs"]
mod archive_export;
#[path = "webui/archive_interactions.rs"]
mod archive_interactions;
#[path = "webui/archive_post.rs"]
mod archive_post;
#[path = "webui/archive_render.rs"]
mod archive_render;
#[path = "webui/archive_search.rs"]
mod archive_search;
#[path = "webui/archive_ui.rs"]
mod archive_ui;
#[path = "webui/delete_account.rs"]
mod delete_account;
#[path = "webui/domains_users.rs"]
mod domains_users;
#[path = "webui/emergency.rs"]
mod emergency;
#[path = "webui/gdpr.rs"]
mod gdpr;
#[path = "webui/goodbye.rs"]
mod goodbye;
#[path = "webui/held_queue.rs"]
mod held_queue;
#[path = "webui/list_create_index.rs"]
mod list_create_index;
#[path = "webui/list_settings.rs"]
mod list_settings;
#[path = "webui/list_settings_groups.rs"]
mod list_settings_groups;
#[path = "webui/member_admin.rs"]
mod member_admin;
#[path = "webui/member_search.rs"]
mod member_search;
#[path = "webui/members_admin.rs"]
mod members_admin;
#[path = "webui/moderation_cross.rs"]
mod moderation_cross;
#[path = "webui/notices.rs"]
mod notices;
#[path = "webui/oidc.rs"]
mod oidc;
#[path = "webui/passkeys.rs"]
mod passkeys;
#[path = "webui/posting_limits.rs"]
mod posting_limits;
#[path = "webui/profile.rs"]
mod profile;
#[path = "webui/reset.rs"]
mod reset;
#[path = "webui/sessions.rs"]
mod sessions;
#[path = "webui/signup.rs"]
mod signup;
#[path = "webui/subject_prefix.rs"]
mod subject_prefix;
#[path = "webui/subject_prefix_controls.rs"]
mod subject_prefix_controls;
#[path = "webui/system.rs"]
mod system;
#[path = "webui/tokens.rs"]
mod tokens;
#[path = "webui/totp.rs"]
mod totp;
#[path = "webui/webhooks.rs"]
mod webhooks;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    response::Response,
};
use listmngr_core::Config;
use listmngr_db::{Database, NewList};
use tower::ServiceExt;
async fn fixture() -> (Database, axum::Router) {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    seeded_fixture(db).await
}
async fn seeded_fixture(db: Database) -> (Database, axum::Router) {
    seeded_fixture_configured(db, |_| {}).await
}
/// The shared fixture: two lists on one domain, and a router whose
/// production defaults of five password checks a minute and a mandatory
/// second factor for server owners are switched off, because many flows sign
/// in repeatedly and use privileged pages as a server owner; each default has
/// its own test.
async fn seeded_fixture_configured(
    db: Database,
    configure: impl FnOnce(&mut Config),
) -> (Database, axum::Router) {
    db.domains().create("example.com", "", None).await.unwrap();
    for (name, advertised) in [("public", true), ("private", false)] {
        let id = format!("{name}.example.com").parse().unwrap();
        db.lists()
            .create(NewList {
                list_id: id,
                display_name: "<script>alert(1)</script>".into(),
                style: "legacy-default".into(),
            })
            .await
            .unwrap();
        // Hold notices are covered by their own tests; keep the browser
        // moderation queue limited to the posts under test.
        db.lists()
            .update(
                &format!("{name}.example.com").parse().unwrap(),
                &serde_json::json!({"advertised":advertised, "respond_to_post_requests": false, "admin_immed_notify": false}),
            )
            .await
            .unwrap();
    }
    let mut config = Config::default();
    config.site.base_url = "http://localhost".into();
    config.security.rate_limit.login = "1000/min".into();
    config.security.require_2fa_for = Vec::new();
    configure(&mut config);
    let app = listmngr_api::router(db.clone(), config);
    (db, app)
}
/// The production login budget, for the tests that measure it.
async fn strict_login_fixture() -> (Database, axum::Router) {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    seeded_fixture_configured(db, |config| {
        config.security.rate_limit.login = "5/min".into();
    })
    .await
}
async fn call(app: &axum::Router, method: &str, path: &str, cookie: &str, body: &str) -> Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("host", "localhost")
                .header("origin", "http://localhost")
                .header("cookie", cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(body.to_owned()))
                .unwrap(),
        )
        .await
        .unwrap()
}
async fn text(r: Response) -> String {
    String::from_utf8(to_bytes(r.into_body(), 1_000_000).await.unwrap().to_vec()).unwrap()
}
#[tokio::test]
async fn directory_is_usable_public_only_and_escaped() {
    let (_, app) = fixture().await;
    let r = call(&app, "GET", "/web", "", "").await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()["cache-control"], "no-store");
    assert_eq!(r.headers()["referrer-policy"], "strict-origin");
    assert_eq!(
        r.headers()["permissions-policy"],
        "camera=(), microphone=(), geolocation=(), payment=(), usb=()"
    );
    let html = text(r).await;
    assert!(html.contains("public.example.com"));
    assert!(!html.contains("private.example.com"));
    assert!(html.contains("&lt;script&gt;"));
    assert!(!html.contains("<script>"));
    assert!(html.contains("/web/login"));
}

#[tokio::test]
async fn public_archive_browser_is_searchable_bounded_escaped_and_policy_gated() {
    use base64::Engine as _;
    let (db, app) = fixture().await;
    let list = "public.example.com".parse().unwrap();
    db.lists()
        .update(&list, &serde_json::json!({"archive_policy":"public"}))
        .await
        .unwrap();
    for i in 0..25 {
        let body = format!("message-{i:02} <script>alert(1)</script>");
        let raw = format!(
            "From: sender@example.com\r\nSubject: literal%_ archive\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{body}"
        );
        sqlx::query("INSERT INTO archive_messages(list_id,hash,thread,subject,body,raw_b64,created_at) VALUES($1,$2,$3,$4,$5,$6,$7)")
            .bind(list.as_str()).bind(format!("hash-{i:02}"))
            .bind(if i < 2 { "first&thread" } else { "other" })
            .bind("literal%_ archive").bind(body)
            .bind(base64::engine::general_purpose::STANDARD.encode(raw)).bind(i64::from(i))
            .execute(db.pool()).await.unwrap();
    }
    let url = "/web/lists/public.example.com/archive";
    let response = call(&app, "GET", url, "", "").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let html = text(response).await;
    assert_eq!(html.matches("<article ").count(), 20);
    assert!(html.contains("&lt;script&gt;"));
    assert!(!html.contains("<script>"));
    assert!(html.contains("Next"));
    let html = text(call(&app, "GET", &format!("{url}?page=2"), "", "").await).await;
    assert_eq!(html.matches("<article ").count(), 5);
    assert!(html.contains("message-24"));
    assert!(!html.contains("message-00"));
    let html = text(call(&app, "GET", &format!("{url}?q=message-24"), "", "").await).await;
    assert_eq!(html.matches("<article ").count(), 1);
    assert!(html.contains("message-24"));
    let html = text(call(&app, "GET", &format!("{url}?thread=first%26thread"), "", "").await).await;
    assert_eq!(html.matches("<article ").count(), 2);
    let html = text(call(&app, "GET", &format!("{url}?q=absent"), "", "").await).await;
    assert_eq!(html.matches("<article ").count(), 0);
    assert!(html.contains("No messages"));
    for query in [
        "page=0".to_owned(),
        "page=5002".into(),
        format!("q={}", "x".repeat(201)),
    ] {
        assert_eq!(
            call(&app, "GET", &format!("{url}?{query}"), "", "")
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    db.lists()
        .update(&list, &serde_json::json!({"archive_policy":"private"}))
        .await
        .unwrap();
    assert_eq!(
        call(&app, "GET", url, "", "").await.status(),
        StatusCode::FORBIDDEN
    );
    db.lists()
        .update(&list, &serde_json::json!({"archive_policy":"never"}))
        .await
        .unwrap();
    assert_eq!(
        call(&app, "GET", url, "", "").await.status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn public_archive_is_discoverable_only_when_enabled() {
    let (db, app) = fixture().await;
    let id = "public.example.com".parse().unwrap();
    for policy in ["public", "private", "never"] {
        db.lists()
            .update(&id, &serde_json::json!({"archive_policy":policy}))
            .await
            .unwrap();
        let html = text(call(&app, "GET", "/web/lists/public.example.com", "", "").await).await;
        assert_eq!(
            html.contains("href=\"/web/lists/public.example.com/archive\""),
            policy == "public"
        );
    }
}

#[tokio::test]
async fn archive_permalink_selects_exact_message_beyond_first_page_with_current_policy() {
    let (db, app) = fixture().await;
    verify_archive_permalink(db, app).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_archive_permalink_matrix() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_archive")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 1).await.unwrap();
    db.migrate().await.unwrap();
    let (db, app) = seeded_fixture(db).await;
    verify_archive_permalink(db, app).await;
    schema.drop().await.unwrap();
}

async fn verify_archive_permalink(db: Database, app: axum::Router) {
    seed_browser_archive(&db).await;
    // Unrelated earlier rows must not be decoded by an exact-message lookup.
    for i in 0..25 {
        sqlx::query("INSERT INTO archive_messages(list_id,hash,thread,subject,body,raw_b64,created_at) VALUES('public.example.com',$1,'other','other','other','invalid base64',0)")
            .bind(format!("earlier-{i:02}")).execute(db.pool()).await.unwrap();
    }
    let url = "/web/lists/public.example.com/archive?message=browser-archive";
    let response = call(&app, "GET", url, "", "").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let html = text(response).await;
    assert_eq!(html.matches("<article ").count(), 1);
    assert!(html.contains("Archived text &lt;script&gt;unsafe&lt;/script&gt;"));
    assert!(html.contains("Permanent link"));
    assert!(!html.contains("<script>"));
    assert!(!html.contains(">Next</a>"));
    assert_eq!(
        call(
            &app,
            "GET",
            "/web/lists/public.example.com/archive?message=missing",
            "",
            ""
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    let other = "private.example.com".parse().unwrap();
    db.lists()
        .update(&other, &serde_json::json!({"archive_policy":"public"}))
        .await
        .unwrap();
    assert_eq!(
        call(
            &app,
            "GET",
            "/web/lists/private.example.com/archive?message=browser-archive",
            "",
            ""
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    for query in [
        "message=".to_owned(),
        "message=browser-archive&page=2".into(),
        "message=browser-archive&q=other".into(),
        "message=browser-archive&thread=other".into(),
        format!("message={}", "a".repeat(201)),
    ] {
        assert_eq!(
            call(
                &app,
                "GET",
                &format!("/web/lists/public.example.com/archive?{query}"),
                "",
                ""
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
    }
    let list = "public.example.com".parse().unwrap();
    for (policy, expected) in [
        ("private", StatusCode::FORBIDDEN),
        ("never", StatusCode::NOT_FOUND),
    ] {
        db.lists()
            .update(&list, &serde_json::json!({"archive_policy":policy}))
            .await
            .unwrap();
        assert_eq!(call(&app, "GET", url, "", "").await.status(), expected);
    }
}

#[tokio::test]
async fn archive_browser_mbox_exports_only_selected_page_thread_or_message() {
    let (db, app) = fixture().await;
    let list = "public.example.com".parse().unwrap();
    db.lists()
        .update(&list, &serde_json::json!({"archive_policy":"public"}))
        .await
        .unwrap();
    for i in 0..25 {
        let raw = format!(
            "From: sender@example.com\r\nSubject: Export item-{i:02}\r\nContent-Type: text/plain\r\n\r\npayload-{i:02}\r\nFrom deceptive line\r\n"
        );
        sqlx::query("INSERT INTO archive_messages(list_id,hash,thread,subject,body,raw_b64,created_at) VALUES('public.example.com',$1,$2,$3,$4,$5,$6)")
            .bind(format!("export-{i:02}"))
            .bind(if i < 2 { "first&thread" } else { "other" })
            .bind(format!("Export item-{i:02}")).bind(format!("payload-{i:02}"))
            .bind(base64::Engine::encode(&base64::engine::general_purpose::STANDARD, raw))
            .bind(i64::from(i)).execute(db.pool()).await.unwrap();
    }
    let url = "/web/lists/public.example.com/archive";
    let r = call(&app, "GET", &format!("{url}?format=mbox"), "", "").await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()["content-type"], "application/mbox");
    assert_eq!(
        r.headers()["content-disposition"],
        "attachment; filename=archive.mbox"
    );
    assert_eq!(r.headers()["cache-control"], "no-store");
    let body = text(r).await;
    assert_eq!(body.matches("From archive@localhost ").count(), 20);
    assert!(body.contains(">From deceptive line"));
    assert!(!body.contains("payload-20"));
    for (filter, count) in [
        ("page=2", 5),
        ("thread=first%26thread", 2),
        ("q=payload-24", 1),
        ("message=export-24", 1),
        ("q=absent", 0),
    ] {
        let r = call(&app, "GET", &format!("{url}?format=mbox&{filter}"), "", "").await;
        assert_eq!(r.status(), StatusCode::OK);
        let body = text(r).await;
        assert_eq!(body.matches("From archive@localhost ").count(), count);
        if filter == "page=2" || filter.ends_with("24") {
            assert!(body.contains("payload-24"));
            assert!(!body.contains("payload-00"));
        }
        verify_download_link(&app, &format!("{url}?{filter}"), count).await;
    }
    // Export must not decode the HTML paginator's extra look-ahead row.
    sqlx::query("UPDATE archive_messages SET raw_b64='invalid base64' WHERE list_id='public.example.com' AND hash='export-20'")
        .execute(db.pool()).await.unwrap();
    assert_eq!(
        call(&app, "GET", &format!("{url}?format=mbox"), "", "")
            .await
            .status(),
        StatusCode::OK
    );
    let html = text(call(&app, "GET", &format!("{url}?message=export-24"), "", "").await).await;
    assert!(html.contains("Download this selection (mbox)"));
    assert!(html.contains("message=export-24"));
    assert_eq!(
        call(&app, "GET", &format!("{url}?format=raw"), "", "")
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    for (policy, expected) in [
        ("private", StatusCode::FORBIDDEN),
        ("never", StatusCode::NOT_FOUND),
    ] {
        db.lists()
            .update(&list, &serde_json::json!({"archive_policy":policy}))
            .await
            .unwrap();
        assert_eq!(
            call(
                &app,
                "GET",
                &format!("{url}?format=mbox&message=export-24"),
                "",
                ""
            )
            .await
            .status(),
            expected
        );
    }
}

#[tokio::test]
async fn archive_attachment_download_uses_current_policy_and_exact_selection() {
    let (db, app) = fixture().await;
    verify_archive_attachments(db, app).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_archive_attachment_matrix() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_private_archive")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 3).await.unwrap();
    db.migrate().await.unwrap();
    let (db, app) = seeded_fixture(db).await;
    verify_archive_attachments(db, app).await;
    schema.drop().await.unwrap();
}

async fn verify_archive_attachments(db: Database, app: axum::Router) {
    seed_browser_archive(&db).await;
    let raw = b"From: sender@example.com\r\nSubject: Attachments\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=parts\r\n\r\n--parts\r\nContent-Type: text/plain\r\n\r\nAttachment body\r\n--parts\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment; filename=\"<script>.bin\"\r\nContent-Transfer-Encoding: base64\r\n\r\nAP9BQkM=\r\n--parts\r\nContent-Type: text/html\r\nContent-Disposition: attachment; filename=\"evil.html\"\r\n\r\n<script>alert(1)</script>\r\n--parts--\r\n";
    sqlx::query("UPDATE archive_messages SET raw_b64=$1 WHERE list_id='public.example.com' AND hash='browser-archive'")
        .bind(base64::Engine::encode(&base64::engine::general_purpose::STANDARD, raw))
        .execute(db.pool()).await.unwrap();
    let url = "/web/lists/public.example.com/archive?message=browser-archive&attachment=0";
    let response = call(&app, "GET", url, "", "").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-type"],
        "application/octet-stream"
    );
    assert_eq!(
        response.headers()["content-disposition"],
        "attachment; filename=\"attachment-0.bin\""
    );
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert!(
        response.headers()["cache-control"]
            .to_str()
            .unwrap()
            .contains("no-store")
    );
    assert_eq!(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .as_ref(),
        b"\x00\xffABC"
    );
    verify_attachment_links(app.clone()).await;
    verify_attachment_charset(&db, &app).await;
    verify_attachment_policy(db, app).await;
}

async fn verify_attachment_charset(db: &Database, app: &axum::Router) {
    let raw = b"From: sender@example.com\r\nSubject: CSV\r\nContent-Type: text/csv; charset=windows-1252\r\nContent-Disposition: attachment; filename=data.csv\r\nContent-Transfer-Encoding: base64\r\n\r\n6QD/";
    sqlx::query("UPDATE archive_messages SET raw_b64=$1 WHERE list_id='public.example.com' AND hash='browser-archive'")
        .bind(base64::Engine::encode(&base64::engine::general_purpose::STANDARD, raw))
        .execute(db.pool()).await.unwrap();
    let response = call(
        app,
        "GET",
        "/web/lists/public.example.com/archive?message=browser-archive&attachment=0",
        "",
        "",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .as_ref(),
        b"\xe9\x00\xff"
    );
}

async fn verify_attachment_links(app: axum::Router) {
    let html = text(
        call(
            &app,
            "GET",
            "/web/lists/public.example.com/archive?message=browser-archive",
            "",
            "",
        )
        .await,
    )
    .await;
    assert!(html.contains("&lt;script&gt;.bin"));
    assert!(!html.contains("<script>"));
    let generated = html
        .split("href=\"")
        .find(|p| p.contains("attachment=1"))
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .replace("&amp;", "&");
    let response = call(&app, "GET", &generated, "", "").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()["content-disposition"]
            .to_str()
            .unwrap()
            .starts_with("attachment;")
    );
    assert_eq!(text(response).await.trim(), "<script>alert(1)</script>");
    for (query, status) in [
        ("?attachment=0", StatusCode::BAD_REQUEST),
        (
            "?message=browser-archive&attachment=0&format=mbox",
            StatusCode::BAD_REQUEST,
        ),
        (
            "?message=browser-archive&attachment=0&q=body",
            StatusCode::BAD_REQUEST,
        ),
        (
            "?message=browser-archive&attachment=99",
            StatusCode::NOT_FOUND,
        ),
        ("?message=missing&attachment=0", StatusCode::NOT_FOUND),
    ] {
        assert_eq!(
            call(
                &app,
                "GET",
                &format!("/web/lists/public.example.com/archive{query}"),
                "",
                ""
            )
            .await
            .status(),
            status
        );
    }
}

async fn verify_attachment_policy(db: Database, app: axum::Router) {
    let url = "/web/lists/public.example.com/archive?message=browser-archive&attachment=0";
    let list = "public.example.com".parse().unwrap();
    db.lists()
        .update(&list, &serde_json::json!({"archive_policy":"private"}))
        .await
        .unwrap();
    assert_eq!(
        call(&app, "GET", url, "", "").await.status(),
        StatusCode::FORBIDDEN
    );
    user(&db, "attachment-reader@example.com", false).await;
    member(
        &db,
        "attachment-reader@example.com",
        listmngr_core::MemberRole::Member,
    )
    .await;
    let cookie = login_as(&app, "attachment-reader@example.com").await;
    assert_eq!(
        call(&app, "GET", url, &cookie, "").await.status(),
        StatusCode::OK
    );
    db.addresses()
        .verify("attachment-reader@example.com", false)
        .await
        .unwrap();
    assert_eq!(
        call(&app, "GET", url, &cookie, "").await.status(),
        StatusCode::FORBIDDEN
    );
    db.lists()
        .update(&list, &serde_json::json!({"archive_policy":"never"}))
        .await
        .unwrap();
    assert_eq!(
        call(&app, "GET", url, &cookie, "").await.status(),
        StatusCode::NOT_FOUND
    );
}

async fn verify_download_link(app: &axum::Router, url: &str, expected: usize) {
    let html = text(call(app, "GET", url, "", "").await).await;
    let link = html
        .split("href=\"")
        .find(|part| part.contains("\">Download this selection (mbox)</a>"))
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .replace("&amp;", "&");
    let response = call(app, "GET", &link, "", "").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "application/mbox");
    assert_eq!(
        text(response)
            .await
            .matches("From archive@localhost ")
            .count(),
        expected
    );
}

#[tokio::test]
async fn private_archive_browser_requires_current_verified_membership_and_session() {
    let (db, app) = fixture().await;
    verify_private_archive_browser(db, app).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_private_archive_browser_matrix() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_private_archive")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 3).await.unwrap();
    db.migrate().await.unwrap();
    let (db, app) = seeded_fixture(db).await;
    verify_private_archive_browser(db, app).await;
    schema.drop().await.unwrap();
}

async fn verify_private_archive_browser(db: Database, app: axum::Router) {
    seed_browser_archive(&db).await;
    let list = "public.example.com".parse().unwrap();
    db.lists()
        .update(&list, &serde_json::json!({"archive_policy":"private"}))
        .await
        .unwrap();
    let alice = user(&db, "archive-reader@example.com", false).await;
    member(
        &db,
        "archive-reader@example.com",
        listmngr_core::MemberRole::Member,
    )
    .await;
    let cookie = login_as(&app, "archive-reader@example.com").await;
    private_archive_status(&app, "", StatusCode::FORBIDDEN).await;
    private_archive_status(&app, &cookie, StatusCode::OK).await;
    let account = text(call(&app, "GET", "/web/account", &cookie, "").await).await;
    assert!(account.contains("href=\"/web/lists/public.example.com/archive\">Read archive</a>"));
    user(&db, "archive-outsider@example.com", true).await;
    let outsider = login_as(&app, "archive-outsider@example.com").await;
    private_archive_status(&app, &outsider, StatusCode::FORBIDDEN).await;
    db.addresses()
        .verify("archive-reader@example.com", false)
        .await
        .unwrap();
    private_archive_status(&app, &cookie, StatusCode::FORBIDDEN).await;
    db.addresses()
        .verify("archive-reader@example.com", true)
        .await
        .unwrap();
    private_archive_status(&app, &cookie, StatusCode::OK).await;
    sqlx::query(
        "UPDATE user_credentials SET password_updated_at='archive-fixture-version' WHERE user_id=$1"
    )
    .bind(alice.id.to_string())
    .execute(db.pool())
    .await
    .unwrap();
    private_archive_status(&app, &cookie, StatusCode::FORBIDDEN).await;
    let cookie = login_as(&app, "archive-reader@example.com").await;
    private_archive_status(&app, &cookie, StatusCode::OK).await;
    sqlx::query("UPDATE web_sessions SET expires_at=0 WHERE user_id=$1")
        .bind(alice.id.to_string())
        .execute(db.pool())
        .await
        .unwrap();
    private_archive_status(&app, &cookie, StatusCode::FORBIDDEN).await;
}

async fn private_archive_status(app: &axum::Router, cookie: &str, expected: StatusCode) {
    for query in [
        "",
        "?q=Archived",
        "?thread=browser-thread",
        "?message=browser-archive",
        "?format=mbox",
        "?message=browser-archive&format=mbox",
    ] {
        let response = call(
            app,
            "GET",
            &format!("/web/lists/public.example.com/archive{query}"),
            cookie,
            "",
        )
        .await;
        assert_eq!(response.status(), expected, "projection {query}");
        let body = text(response).await;
        assert_eq!(body.contains("Archived text"), expected == StatusCode::OK);
    }
}

fn cookie(r: &Response) -> String {
    r.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}
fn csrf(html: &str) -> String {
    html.split("name=\"csrf\" value=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .to_owned()
}
async fn session(app: &axum::Router) -> (String, String) {
    let r = call(app, "GET", "/web/login", "", "").await;
    assert_eq!(r.status(), StatusCode::OK);
    let c = cookie(&r);
    let x = csrf(&text(r).await);
    (c, x)
}
#[tokio::test]
async fn login_rotates_persistent_hashed_session_requires_csrf_and_logout_revokes() {
    let (db, app) = fixture().await;
    db.users()
        .create(listmngr_db::NewUser {
            display_name: "Me".into(),
            email: "me@example.com".into(),
            password: "very secure password".into(),
            server_owner: false,
        })
        .await
        .unwrap();
    db.addresses().verify("me@example.com", true).await.unwrap();
    let (c, x) = session(&app).await;
    assert_eq!(
        call(
            &app,
            "POST",
            "/web/login",
            &c,
            "email=me%40example.com&password=very+secure+password"
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/web/login",
            &c,
            &format!("csrf={x}&email=me%40example.com&password=wrong")
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    let r = call(
        &app,
        "POST",
        "/web/login",
        &c,
        &format!("csrf={x}&email=me%40example.com&password=very+secure+password"),
    )
    .await;
    assert_eq!(r.status(), StatusCode::SEE_OTHER);
    assert_eq!(r.headers()["location"], "/web/account");
    let set = r.headers()["set-cookie"].to_str().unwrap();
    assert!(set.contains("HttpOnly"));
    assert!(set.contains("SameSite=Strict"));
    let auth = cookie(&r);
    assert_ne!(auth, c);
    let hash: String =
        sqlx::query_scalar("SELECT token_hash FROM web_sessions WHERE user_id IS NOT NULL")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(hash.len(), 64);
    assert!(!auth.contains(&hash));
    assert_eq!(
        call(&app, "GET", "/web/account", &c, "").await.status(),
        StatusCode::UNAUTHORIZED
    );
    let r = call(&app, "GET", "/web/account", &auth, "").await;
    assert_eq!(r.status(), StatusCode::OK);
    let x = csrf(&text(r).await);
    assert_eq!(
        call(&app, "POST", "/web/logout", &auth, &format!("csrf={x}"))
            .await
            .status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        call(&app, "GET", "/web/account", &auth, "").await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn browser_join_leave_notices_and_confirmation_are_durable_and_get_is_safe() {
    let (db, app) = fixture().await;
    let r = call(&app, "GET", "/web/lists/public.example.com", "", "").await;
    assert_eq!(r.status(), StatusCode::OK);
    let c = cookie(&r);
    let x = csrf(&text(r).await);
    for action in ["join", "leave"] {
        if action == "leave" {
            sqlx::query("UPDATE subscription_workflows SET created_at=0")
                .execute(db.pool())
                .await
                .unwrap();
        }
        let r = call(
            &app,
            "POST",
            "/web/lists/public.example.com/request",
            &c,
            &format!("csrf={x}&email=person%40example.com&action={action}"),
        )
        .await;
        assert_eq!(r.status(), StatusCode::ACCEPTED);
        let raws: Vec<Vec<u8>> = sqlx::query_scalar("SELECT raw FROM message_blobs")
            .fetch_all(db.pool())
            .await
            .unwrap();
        // The Mailman confirmation bodies name the request kind, not the
        // command word.
        let marker = if action == "join" {
            "registration request"
        } else {
            "unsubscription request"
        };
        let text = raws
            .iter()
            .map(|r| String::from_utf8(r.clone()).unwrap())
            .find(|r| r.contains(marker))
            .unwrap();
        let tok = text
            .split("Token: ")
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap();
        let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM members")
            .fetch_one(db.pool())
            .await
            .unwrap();
        let r = call(
            &app,
            "GET",
            &format!("/web/lists/public.example.com/confirm?token={tok}"),
            &c,
            "",
        )
        .await;
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM members")
                .fetch_one(db.pool())
                .await
                .unwrap(),
            before
        );
        let r = call(
            &app,
            "POST",
            "/web/lists/public.example.com/confirm",
            &c,
            &format!("csrf={x}&token={tok}"),
        )
        .await;
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM members")
                .fetch_one(db.pool())
                .await
                .unwrap(),
            i64::from(action == "join")
        );
        assert_eq!(
            call(
                &app,
                "POST",
                "/web/lists/public.example.com/confirm",
                &c,
                &format!("csrf={x}&token={tok}")
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
    }
}

async fn paged_members(db: &Database, role: listmngr_core::MemberRole) {
    user(db, "paged@example.com", false).await;
    for index in 0..21 {
        let id = format!("page{index:02}.example.com").parse().unwrap();
        db.lists()
            .create(NewList {
                list_id: id,
                display_name: format!("Page {index:02}"),
                style: "legacy-default".into(),
            })
            .await
            .unwrap();
        db.members()
            .create(listmngr_db::NewMember {
                list_id: format!("page{index:02}.example.com").parse().unwrap(),
                email: "paged@example.com".into(),
                role,
                subscription_mode: listmngr_core::SubscriptionMode::AsAddress,
                display_name: String::new(),
            })
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn account_pages_only_owned_member_subscriptions() {
    let (db, app) = fixture().await;
    paged_members(&db, listmngr_core::MemberRole::Member).await;
    member(
        &db,
        "paged@example.com",
        listmngr_core::MemberRole::Moderator,
    )
    .await;
    member(
        &db,
        "foreign@example.com",
        listmngr_core::MemberRole::Member,
    )
    .await;
    let c = login_as(&app, "paged@example.com").await;
    let first = text(call(&app, "GET", "/web/account", &c, "").await).await;
    assert_eq!(first.matches("<section>").count(), 20);
    assert!(first.contains("page00.example.com"));
    assert!(!first.contains("page20.example.com"));
    assert!(!first.contains("public.example.com"));
    assert!(first.contains("/web/account?page=1"));
    let second = text(call(&app, "GET", "/web/account?page=1", &c, "").await).await;
    assert_eq!(second.matches("<section>").count(), 1);
    assert!(second.contains("page20.example.com"));
    assert!(!second.contains("page00.example.com"));
    assert!(!second.contains("Next page"));
    assert!(second.contains("/web/account?page=0"));
    assert_eq!(
        call(&app, "GET", "/web/account?page=10001", &c, "")
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn moderation_pages_filter_authority_before_limiting() {
    let (db, app) = fixture().await;
    paged_members(&db, listmngr_core::MemberRole::Moderator).await;
    // An unauthorized row sorts before every authorized row. Applying LIMIT
    // before the permission filter would underfill page zero and skip a list.
    db.lists()
        .create(NewList {
            list_id: "aaa-unrelated.example.com".parse().unwrap(),
            display_name: "Not authorized".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    member(&db, "paged@example.com", listmngr_core::MemberRole::Member).await;
    let c = login_as(&app, "paged@example.com").await;
    let first = text(call(&app, "GET", "/web/moderation", &c, "").await).await;
    assert_eq!(first.matches(" — held messages</a>").count(), 20);
    assert!(first.contains("page00.example.com"));
    assert!(!first.contains("page20.example.com"));
    assert!(!first.contains("public.example.com"));
    assert!(!first.contains("private.example.com"));
    assert!(!first.contains("aaa-unrelated.example.com"));
    assert!(first.contains("/web/moderation?page=1"));
    let second = text(call(&app, "GET", "/web/moderation?page=1", &c, "").await).await;
    assert_eq!(second.matches(" — held messages</a>").count(), 1);
    assert!(second.contains("page20.example.com"));
    assert!(!second.contains("Next page"));
    assert_eq!(
        call(&app, "GET", "/web/moderation?page=10001", &c, "")
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    db.addresses()
        .verify("paged@example.com", false)
        .await
        .unwrap();
    let revoked = text(call(&app, "GET", "/web/moderation", &c, "").await).await;
    assert!(!revoked.contains(" — held messages</a>"));
    user(&db, "owner@example.com", true).await;
    let owner = login_as(&app, "owner@example.com").await;
    let second = text(call(&app, "GET", "/web/moderation?page=1", &owner, "").await).await;
    assert_eq!(second.matches(" — held messages</a>").count(), 21 + 3 - 20);
    assert!(second.contains("public.example.com"));
    assert!(second.contains("private.example.com"));
}

async fn user(db: &Database, email: &str, owner: bool) -> listmngr_core::User {
    let u = db
        .users()
        .create(listmngr_db::NewUser {
            display_name: email.into(),
            email: email.into(),
            password: "very secure password".into(),
            server_owner: owner,
        })
        .await
        .unwrap();
    db.addresses().verify(email, true).await.unwrap();
    u
}
async fn member(
    db: &Database,
    email: &str,
    role: listmngr_core::MemberRole,
) -> listmngr_core::Member {
    db.members()
        .create(listmngr_db::NewMember {
            list_id: "public.example.com".parse().unwrap(),
            email: email.into(),
            role,
            subscription_mode: listmngr_core::SubscriptionMode::AsAddress,
            display_name: email.into(),
        })
        .await
        .unwrap()
}
async fn login_as(app: &axum::Router, email: &str) -> String {
    let (c, x) = session(app).await;
    let form = serde_urlencoded::to_string([
        ("csrf", x.as_str()),
        ("email", email),
        ("password", "very secure password"),
    ])
    .unwrap();
    let r = call(app, "POST", "/web/login", &c, &form).await;
    assert_eq!(r.status(), StatusCode::SEE_OTHER);
    cookie(&r)
}
#[tokio::test]
async fn browser_password_change_revokes_sessions_and_audits_atomically() {
    let (db, app) = fixture().await;
    verify_password_change(db, app).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_browser_password_change() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_password")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 3).await.unwrap();
    db.migrate().await.unwrap();
    let (db, app) = seeded_fixture(db).await;
    verify_password_change(db, app).await;
    schema.drop().await.unwrap();
}

#[tokio::test]
async fn password_change_rejects_invalid_forms_without_mutation() {
    let (db, app) = fixture().await;
    let user = user(&db, "password-form@example.com", false).await;
    let cookie = login_as(&app, "password-form@example.com").await;
    let url = "/web/account/password";
    assert_eq!(
        call(&app, "GET", url, "", "").await.status(),
        StatusCode::UNAUTHORIZED
    );
    let token = csrf(&text(call(&app, "GET", url, &cookie, "").await).await);
    for (new, confirm) in [
        ("different".to_owned(), "confirmation".to_owned()),
        (String::new(), String::new()),
        ("x".repeat(1025), "x".repeat(1025)),
    ] {
        let form = serde_urlencoded::to_string([
            ("csrf", token.as_str()),
            ("current_password", "very secure password"),
            ("new_password", new.as_str()),
            ("confirm_password", confirm.as_str()),
        ])
        .unwrap();
        assert_eq!(
            call(&app, "POST", url, &cookie, &form).await.status(),
            StatusCode::BAD_REQUEST
        );
    }
    let form = serde_urlencoded::to_string([
        ("csrf", token.as_str()),
        ("current_password", "very secure password"),
        ("new_password", "new strong password phrase 2026!"),
        ("confirm_password", "new strong password phrase 2026!"),
    ])
    .unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(url)
                .header("host", "localhost")
                .header("origin", "https://foreign.invalid")
                .header("cookie", &cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(form))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(
        db.users()
            .verify_password(user.id, "very secure password")
            .await
            .unwrap()
    );
    assert_eq!(
        call(&app, "GET", "/web/account", &cookie, "")
            .await
            .status(),
        StatusCode::OK
    );
    let audits: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='user.password'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(audits, 0);
}

async fn verify_password_change(db: Database, app: axum::Router) {
    let other = user(&db, "unaffected@example.com", false).await;
    let survivor = db
        .create_web_session(Some(other.id), None, chrono::Utc::now().timestamp_millis())
        .await
        .unwrap();
    let user = user(&db, "password-reader@example.com", false).await;
    let first = login_as(&app, "password-reader@example.com").await;
    let second = login_as(&app, "password-reader@example.com").await;
    let url = "/web/account/password";
    let response = call(&app, "GET", url, &first, "").await;
    assert_eq!(response.status(), StatusCode::OK);
    let token = csrf(&text(response).await);
    let form = |current: &str, csrf: &str| {
        serde_urlencoded::to_string([
            ("csrf", csrf),
            ("current_password", current),
            ("new_password", "new strong password phrase 2026!"),
            ("confirm_password", "new strong password phrase 2026!"),
        ])
        .unwrap()
    };
    assert_eq!(
        call(
            &app,
            "POST",
            url,
            &first,
            &form("very secure password", "wrong-csrf")
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "POST", url, &first, &form("incorrect", &token))
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert!(
        db.users()
            .verify_password(user.id, "very secure password")
            .await
            .unwrap()
    );
    let response = call(
        &app,
        "POST",
        url,
        &first,
        &form("very secure password", &token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    assert!(text(response).await.contains("Password changed"));
    verify_password_effects(db, app, user, first, second, survivor).await;
}

async fn verify_password_effects(
    db: Database,
    app: axum::Router,
    user: listmngr_core::User,
    first: String,
    second: String,
    survivor: listmngr_db::web_sessions::WebSession,
) {
    assert!(
        db.web_session(&survivor.token, chrono::Utc::now().timestamp_millis())
            .await
            .is_ok()
    );
    assert!(
        !db.users()
            .verify_password(user.id, "very secure password")
            .await
            .unwrap()
    );
    assert!(
        db.users()
            .verify_password(user.id, "new strong password phrase 2026!")
            .await
            .unwrap()
    );
    for cookie in [&first, &second] {
        assert_eq!(
            call(&app, "GET", "/web/account", cookie, "").await.status(),
            StatusCode::UNAUTHORIZED
        );
    }
    let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM web_sessions WHERE user_id=$1")
        .bind(user.id.to_string())
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(sessions, 0);
    let audits: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log WHERE action='user.password' AND target_id=$1 AND actor_user_id=$1",
    )
    .bind(user.id.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(audits, 1);
}

#[tokio::test]
async fn browser_member_leave_works_for_hidden_lists_without_removing_other_roles() {
    let (db, app) = fixture().await;
    verify_member_leave(db, app, true).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_browser_member_leave() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_leave")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 3).await.unwrap();
    db.migrate().await.unwrap();
    let (db, app) = seeded_fixture(db).await;
    verify_member_leave(db, app, false).await;
    schema.drop().await.unwrap();
}

async fn verify_member_leave(db: Database, app: axum::Router, sqlite: bool) {
    let user = user(&db, "leaver@example.com", false).await;
    let own = member(&db, "leaver@example.com", listmngr_core::MemberRole::Member).await;
    let owner = member(&db, "leaver@example.com", listmngr_core::MemberRole::Owner).await;
    let other = member(
        &db,
        "other-member@example.com",
        listmngr_core::MemberRole::Member,
    )
    .await;
    db.lists()
        .update(&own.list_id, &serde_json::json!({"advertised":false}))
        .await
        .unwrap();
    let preferences: String = sqlx::query_scalar("SELECT preferences_id FROM members WHERE id=$1")
        .bind(own.id.to_string())
        .fetch_one(db.pool())
        .await
        .unwrap();
    let cookie = login_as(&app, "leaver@example.com").await;
    let url = format!("/web/members/{}/leave", own.id);
    let response = call(&app, "GET", &url, &cookie, "").await;
    assert_eq!(response.status(), StatusCode::OK);
    let confirmation = text(response).await;
    assert!(
        confirmation.contains("leaver@example.com"),
        "identify the exact address before destructive confirmation"
    );
    let token = csrf(&confirmation);
    assert!(
        db.members().get(own.id).await.is_ok(),
        "GET must not unsubscribe"
    );
    let account = text(call(&app, "GET", "/web/account", &cookie, "").await).await;
    assert!(account.contains(&format!("href=\"{url}\">Leave list</a>")));
    let form = serde_urlencoded::to_string([("csrf", token.as_str())]).unwrap();
    verify_leave_denials(
        app.clone(),
        url.clone(),
        cookie.clone(),
        form.clone(),
        [other.id, owner.id],
    )
    .await;
    verify_leave_rollback(&db, &app, &cookie, &form, &own, sqlite).await;
    let response = call(&app, "POST", &url, &cookie, &form).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()["location"], "/web/account");
    assert_eq!(
        call(&app, "POST", &url, &cookie, &form).await.status(),
        StatusCode::FORBIDDEN
    );
    verify_leave_effects(db, user, [own, other, owner], preferences).await;
}

async fn verify_leave_rollback(
    db: &Database,
    app: &axum::Router,
    cookie: &str,
    form: &str,
    member: &listmngr_core::Member,
    sqlite: bool,
) {
    sqlx::query(if sqlite { "CREATE TRIGGER reject_leave_audit BEFORE INSERT ON audit_log WHEN NEW.action='member.delete' BEGIN SELECT RAISE(ABORT,'owned audit failure'); END" } else { "ALTER TABLE audit_log ADD CONSTRAINT reject_leave_audit CHECK (action <> 'member.delete') NOT VALID" }).execute(db.pool()).await.unwrap();
    let url = format!("/web/members/{}/leave", member.id);
    let response = call(app, "POST", &url, cookie, form).await;
    assert!(!response.status().is_success() && !response.status().is_redirection());
    assert_eq!(
        serde_json::to_value(db.members().get(member.id).await.unwrap()).unwrap(),
        serde_json::to_value(member).unwrap()
    );
    assert!(db.preferences().get(member.preferences_id).await.is_ok());
    let audits: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log WHERE action='member.delete' AND target_id=$1",
    )
    .bind(member.id.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(audits, 0);
    sqlx::query(if sqlite {
        "DROP TRIGGER reject_leave_audit"
    } else {
        "ALTER TABLE audit_log DROP CONSTRAINT reject_leave_audit"
    })
    .execute(db.pool())
    .await
    .unwrap();
}

async fn verify_leave_denials(
    app: axum::Router,
    url: String,
    cookie: String,
    form: String,
    ids: [listmngr_core::MemberId; 2],
) {
    assert_eq!(
        call(&app, "POST", &url, &cookie, "csrf=wrong")
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    for id in ids {
        let denied = format!("/web/members/{id}/leave");
        assert_eq!(
            call(&app, "GET", &denied, &cookie, "").await.status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(&app, "POST", &denied, &cookie, &form).await.status(),
            StatusCode::FORBIDDEN
        );
    }
}

async fn verify_leave_effects(
    db: Database,
    user: listmngr_core::User,
    members: [listmngr_core::Member; 3],
    preferences: String,
) {
    let [own, other, owner] = members;
    assert!(db.members().get(own.id).await.is_err());
    assert!(db.members().get(other.id).await.is_ok());
    assert!(db.members().get(owner.id).await.is_ok());
    assert!(db.users().get(user.id).await.is_ok());
    assert_eq!(
        db.addresses()
            .get("leaver@example.com")
            .await
            .unwrap()
            .user_id,
        Some(user.id)
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM preferences WHERE id=$1")
        .bind(preferences)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    let audits: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='member.delete' AND target_id=$1 AND actor_user_id=$2").bind(own.id.to_string()).bind(user.id.to_string()).fetch_one(db.pool()).await.unwrap();
    assert_eq!(audits, 1);
}

#[tokio::test]
async fn own_preferences_are_persisted_and_other_members_are_forbidden() {
    let (db, app) = fixture().await;
    user(&db, "me@example.com", false).await;
    user(&db, "other@example.com", false).await;
    let own = member(&db, "me@example.com", listmngr_core::MemberRole::Member).await;
    let other = member(&db, "other@example.com", listmngr_core::MemberRole::Member).await;
    let c = login_as(&app, "me@example.com").await;
    let html = text(call(&app, "GET", "/web/account", &c, "").await).await;
    assert!(html.contains(&own.id.to_string()));
    assert!(!html.contains(&other.id.to_string()));
    let x = csrf(&html);
    let body = format!("csrf={x}&delivery_mode=mime_digests&delivery_status=by_user");
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("/web/members/{}/preferences", other.id),
            &c,
            &body
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("/web/members/{}/preferences", own.id),
            &c,
            &body
        )
        .await
        .status(),
        StatusCode::SEE_OTHER
    );
    let p = db.preferences().get(own.preferences_id).await.unwrap();
    assert_eq!(
        p.delivery_mode,
        Some(listmngr_core::DeliveryMode::MimeDigests)
    );
    assert_eq!(
        p.delivery_status,
        Some(listmngr_core::DeliveryStatus::ByUser)
    );
    db.addresses()
        .verify("me@example.com", false)
        .await
        .unwrap();
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("/web/members/{}/preferences", own.id),
            &c,
            &body
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
}

async fn held(db: &Database) -> listmngr_db::moderation::HeldId {
    held_on(db, "public.example.com").await
}
/// A nonmember post held on `list`.
async fn held_on(db: &Database, list: &str) -> listmngr_db::moderation::HeldId {
    use listmngr_db::mail_queue::{NewMessage, Queue};
    let id: listmngr_core::ListId = list.parse().unwrap();
    db.mail_queue().enqueue(NewMessage{raw:b"From: sender@example.com\r\nSubject: <script>held</script>\r\n\r\nUntrusted <b>body</b>".to_vec(),external_id:uuid::Uuid::now_v7().to_string(),context:serde_json::json!({"version":1,"list_id":list,"envelope_sender":"sender@example.com"}).to_string(),queue:Queue::In,max_attempts:5},1000).await.unwrap();
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
            "<script>held</script>",
            "nonmember",
            1000,
        )
        .await
        .unwrap()
        .id
}
#[tokio::test]
async fn moderator_review_is_scoped_verified_escaped_and_has_real_queue_effects() {
    let (db, app) = fixture().await;
    user(&db, "mod@example.com", false).await;
    member(&db, "mod@example.com", listmngr_core::MemberRole::Moderator).await;
    member(
        &db,
        "recipient@example.com",
        listmngr_core::MemberRole::Member,
    )
    .await;
    let c = login_as(&app, "mod@example.com").await;
    for action in ["accept", "reject", "discard"] {
        let id = held(&db).await;
        let r = call(&app, "GET", "/web/lists/public.example.com/held", &c, "").await;
        assert_eq!(r.status(), StatusCode::OK);
        let html = text(r).await;
        assert!(html.contains("&lt;script&gt;held"));
        assert!(!html.contains("<script>"));
        let x = csrf(&html);
        assert_eq!(
            call(
                &app,
                "POST",
                &format!("/web/lists/private.example.com/held/{}", id.0),
                &c,
                &format!("csrf={x}&action={action}&comment=reviewed")
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
        let path = format!("/web/lists/public.example.com/held/{}", id.0);
        assert_eq!(
            call(
                &app,
                "POST",
                &path,
                &c,
                &format!("csrf={x}&action={action}&comment=reviewed")
            )
            .await
            .status(),
            StatusCode::SEE_OTHER
        );
        assert!(db.moderation().get(id).await.unwrap().disposition.is_some());
        assert_eq!(
            call(
                &app,
                "POST",
                &path,
                &c,
                &format!("csrf={x}&action={action}&comment=again")
            )
            .await
            .status(),
            StatusCode::CONFLICT
        );
    }
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs WHERE queue='out'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
    let recipients: Vec<String> = sqlx::query_scalar("SELECT email FROM delivery_recipients")
        .fetch_all(db.pool())
        .await
        .unwrap();
    assert_eq!(recipients, vec!["recipient@example.com"]);
    db.addresses()
        .verify("mod@example.com", false)
        .await
        .unwrap();
    assert_eq!(
        call(&app, "GET", "/web/lists/public.example.com/held", &c, "")
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn password_attempts_have_a_separate_small_global_budget() {
    let (_, app) = strict_login_fixture().await;
    let (c, x) = session(&app).await;
    for _ in 0..5 {
        assert_eq!(
            call(
                &app,
                "POST",
                "/web/login",
                &c,
                &format!("csrf={x}&email=unknown%40example.com&password=wrong")
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        call(
            &app,
            "POST",
            "/web/login",
            &c,
            &format!("csrf={x}&email=unknown%40example.com&password=wrong")
        )
        .await
        .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn origin_csrf_expiry_password_change_and_cookie_transport_are_enforced() {
    let (db, app) = fixture().await;
    let u = user(&db, "owner@example.com", true).await;
    let c = login_as(&app, "owner@example.com").await;
    let html = text(call(&app, "GET", "/web/account", &c, "").await).await;
    let x = csrf(&html);
    for origin in [
        None,
        Some("null"),
        Some("http://evil.invalid"),
        Some("https://localhost"),
    ] {
        let mut request = Request::builder()
            .method("POST")
            .uri("/web/logout")
            .header("host", "evil.invalid")
            .header("cookie", &c)
            .header("content-type", "application/x-www-form-urlencoded");
        if let Some(o) = origin {
            request = request.header("origin", o);
        }
        let r = app
            .clone()
            .oneshot(request.body(Body::from(format!("csrf={x}"))).unwrap())
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
    }
    assert_eq!(
        call(&app, "POST", "/web/logout", &c, "csrf=wrong")
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    let (c2, x2) = session(&app).await;
    assert_ne!(c, c2);
    assert_eq!(
        call(&app, "POST", "/web/logout", &c, &format!("csrf={x2}"))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    // Rebuilding the router preserves database-backed login; no in-memory auth map.
    let mut config = Config::default();
    config.site.base_url = "https://lists.example.com".into();
    let restarted = listmngr_api::router(db.clone(), config);
    assert_eq!(
        call(&restarted, "GET", "/web/account", &c, "")
            .await
            .status(),
        StatusCode::OK
    );
    let r = call(&restarted, "GET", "/web/login", "", "").await;
    assert!(
        r.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .contains("; Secure")
    );
    db.users()
        .set_password(u.id, "another very secure password for this test")
        .await
        .unwrap();
    assert_eq!(
        call(&app, "GET", "/web/account", &c, "").await.status(),
        StatusCode::UNAUTHORIZED
    );
    sqlx::query("UPDATE web_sessions SET expires_at=0")
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(
        call(&app, "POST", "/web/logout", &c2, &format!("csrf={x2}"))
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let mut config = Config::default();
    config.site.base_url = "http://lists.example.com".into();
    let insecure = listmngr_api::router(db, config);
    assert_eq!(
        call(&insecure, "GET", "/web/login", "", "").await.status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn rate_limit_html_preserves_retry_after() {
    let (_, app) = strict_login_fixture().await;
    let (c, x) = session(&app).await;
    for _ in 0..5 {
        call(
            &app,
            "POST",
            "/web/login",
            &c,
            &format!("csrf={x}&email=missing%40example.com&password=wrong"),
        )
        .await;
    }
    let r = call(
        &app,
        "POST",
        "/web/login",
        &c,
        &format!("csrf={x}&email=missing%40example.com&password=wrong"),
    )
    .await;
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(r.headers().contains_key("retry-after"));
}

async fn wire(
    address: std::net::SocketAddr,
    method: &str,
    path: &str,
    cookie: &str,
    body: &str,
) -> Response {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nCookie: {cookie}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut bytes = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(20),
        stream.read_to_end(&mut bytes),
    )
    .await
    .unwrap()
    .unwrap();
    let response = String::from_utf8(bytes).unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    let mut lines = head.lines();
    let status = lines
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse::<u16>()
        .unwrap();
    let mut response = Response::builder().status(status);
    for line in lines {
        let (k, v) = line.split_once(':').unwrap();
        response = response.header(k, v.trim());
    }
    response.body(Body::from(body.to_owned())).unwrap()
}
#[tokio::test]
async fn real_tcp_browser_self_service_and_moderation_smoke() {
    let (db, _) = fixture().await;
    user(&db, "smoke@example.com", true).await;
    let m = member(&db, "smoke@example.com", listmngr_core::MemberRole::Member).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mut config = Config::default();
    config.site.base_url = format!("http://{address}");
    config.security.require_2fa_for = Vec::new();
    let app = listmngr_api::router(db.clone(), config);
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let r = wire(address, "GET", "/web", "", "").await;
    assert_eq!(r.status(), StatusCode::OK);
    let directory = text(r).await;
    assert!(directory.contains("public.example.com"));
    assert!(!directory.contains("private.example.com"));
    let r = wire(address, "GET", "/web/login", "", "").await;
    assert_eq!(r.status(), StatusCode::OK);
    let c = cookie(&r);
    let x = csrf(&text(r).await);
    let r = wire(
        address,
        "POST",
        "/web/login",
        &c,
        &format!("csrf={x}&email=smoke%40example.com&password=very+secure+password"),
    )
    .await;
    assert_eq!(r.status(), StatusCode::SEE_OTHER);
    let c = cookie(&r);
    let r = wire(address, "GET", "/web/account", &c, "").await;
    assert_eq!(r.status(), StatusCode::OK);
    let html = text(r).await;
    let x = csrf(&html);
    assert!(html.contains(&m.id.to_string()));
    assert_eq!(
        wire(
            address,
            "POST",
            &format!("/web/members/{}/preferences", m.id),
            &c,
            &format!("csrf={x}&delivery_mode=regular&delivery_status=enabled")
        )
        .await
        .status(),
        StatusCode::SEE_OTHER
    );
    wire_confirmation(&db, address, &c, &x).await;
    let id = held(&db).await;
    assert_eq!(
        wire(address, "GET", "/web/lists/public.example.com/held", &c, "")
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        wire(
            address,
            "POST",
            &format!("/web/lists/public.example.com/held/{}", id.0),
            &c,
            &format!("csrf={x}&action=accept&comment=TCP+review")
        )
        .await
        .status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        db.moderation().get(id).await.unwrap().disposition,
        Some(listmngr_db::moderation::Disposition::Accepted)
    );
    let recipients:Vec<String>=sqlx::query_scalar("SELECT email FROM delivery_recipients WHERE job_id IN (SELECT id FROM queue_jobs WHERE message_id=$1) ORDER BY email").bind(db.moderation().get(id).await.unwrap().message_id.0.to_string()).fetch_all(db.pool()).await.unwrap();
    assert_eq!(recipients, vec!["joined@example.com", "smoke@example.com"]);
    assert_eq!(
        wire(address, "POST", "/web/logout", &c, &format!("csrf={x}"))
            .await
            .status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        wire(address, "GET", "/web/account", &c, "").await.status(),
        StatusCode::UNAUTHORIZED
    );
    server.abort();
    println!(
        "REAL TCP PASS: directory 200; login 303; own preferences 303; join request 202; GET confirmation non-consuming; POST confirmation 200; held accept 303; exact 2-recipient snapshot; logout 303; revoked cookie 401. Isolated in-memory SQLite, ephemeral localhost port."
    );
}

#[tokio::test]
async fn browser_responses_are_correlated_and_pagination_is_bounded() {
    let (db, app) = fixture().await;
    for path in ["/web", "/web/account", "/missing"] {
        let r = call(&app, "GET", path, "", "").await;
        assert!(
            r.headers().contains_key("x-request-id"),
            "missing correlation: {path}"
        );
    }
    for n in 0..21 {
        db.lists()
            .create(NewList {
                list_id: format!("page{n:02}.example.com").parse().unwrap(),
                display_name: format!("Page {n}"),
                style: "legacy-default".into(),
            })
            .await
            .unwrap();
    }
    let html = text(call(&app, "GET", "/web", "", "").await).await;
    assert_eq!(html.matches("<li>").count(), 20);
    assert!(html.contains("?page=1"));
    let html = text(call(&app, "GET", "/web?page=1", "", "").await).await;
    assert_eq!(html.matches("<li>").count(), 2);
    assert!(html.contains("?page=0"));
    assert_eq!(
        call(&app, "GET", "/web?page=10001", "", "").await.status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn moderator_preview_is_bounded_and_queue_pages_are_reachable() {
    let (db, app) = fixture().await;
    user(&db, "mod@example.com", true).await;
    for _ in 0..21 {
        held(&db).await;
    }
    let c = login_as(&app, "mod@example.com").await;
    let path = "/web/lists/public.example.com/held";
    let html = text(call(&app, "GET", path, &c, "").await).await;
    assert_eq!(html.matches("<article data-held=").count(), 20);
    assert!(html.contains("?page=1"));
    let html = text(call(&app, "GET", &format!("{path}?page=1"), &c, "").await).await;
    assert_eq!(html.matches("<article data-held=").count(), 1);
}

fn private_browser_evidence_directory(output: &str) {
    std::fs::create_dir_all(output).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(output, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}

async fn seed_private_browser_archive(db: &Database) {
    db.members()
        .create(listmngr_db::NewMember {
            list_id: "private.example.com".parse().unwrap(),
            email: "browser@example.com".into(),
            role: listmngr_core::MemberRole::Member,
            subscription_mode: listmngr_core::SubscriptionMode::AsAddress,
            display_name: "Browser fixture".into(),
        })
        .await
        .unwrap();
    db.lists()
        .update(
            &"private.example.com".parse().unwrap(),
            &serde_json::json!({"archive_policy":"private"}),
        )
        .await
        .unwrap();
    let raw = b"From: sender@example.com\r\nSubject: Private browser archive\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=parts\r\n\r\n--parts\r\nContent-Type: text/plain\r\n\r\nPrivate archived body\r\n--parts\r\nContent-Type: text/csv; charset=windows-1252\r\nContent-Disposition: attachment; filename=private.csv\r\nContent-Transfer-Encoding: base64\r\n\r\n6QD/\r\n--parts--\r\n";
    sqlx::query("INSERT INTO archive_messages(list_id,hash,thread,subject,body,raw_b64,created_at) VALUES('private.example.com','private-browser-archive','private-thread','Private browser archive','Private archived body',$1,1)")
        .bind(base64::Engine::encode(&base64::engine::general_purpose::STANDARD, raw))
        .execute(db.pool()).await.unwrap();
}

async fn seed_browser_archive(db: &Database) {
    db.lists()
        .update(
            &"public.example.com".parse().unwrap(),
            &serde_json::json!({"archive_policy":"public"}),
        )
        .await
        .unwrap();
    let raw = b"From: sender@example.com\r\nSubject: Browser archive fixture\r\nContent-Type: text/plain\r\n\r\nArchived text <script>unsafe</script>";
    sqlx::query("INSERT INTO archive_messages(list_id,hash,thread,subject,body,raw_b64,created_at,sender_name,sender_email) VALUES('public.example.com','browser-archive','browser-thread','Browser archive fixture','Archived text',$1,1,'Sender','sender@example.com')")
        .bind(base64::Engine::encode(&base64::engine::general_purpose::STANDARD, raw))
        .execute(db.pool()).await.unwrap();
}

/// Opt-in real Chromium render against the production router, never a live database.
#[tokio::test]
#[ignore = "requires WEBUI_BROWSER_PYTHON and WEBUI_BROWSER_SCRIPT; disposable browser acceptance"]
#[allow(clippy::too_many_lines)] // One fixture, one browser run, its assertions.
async fn chromium_browser_acceptance() {
    let python = std::env::var("WEBUI_BROWSER_PYTHON").expect("browser Python");
    let script = std::env::var("WEBUI_BROWSER_SCRIPT").expect("browser script");
    let output =
        std::env::var("WEBUI_BROWSER_OUTPUT").expect("outside-repository evidence directory");
    private_browser_evidence_directory(&output);
    let token_path = std::path::Path::new(&output).join(".confirmation-token");
    let _ = std::fs::remove_file(&token_path);
    let (db, _) = fixture().await;
    user(&db, "browser@example.com", true).await;
    seed_browser_archive(&db).await;
    seed_private_browser_archive(&db).await;
    let own = member(
        &db,
        "browser@example.com",
        listmngr_core::MemberRole::Member,
    )
    .await;
    let held_id = held(&db).await;
    // Two more held posts for the bulk decision, and a moderated request on
    // the private list for the requests queue.
    let bulk_a = held(&db).await;
    let bulk_b = held(&db).await;
    let private: listmngr_core::ListId = "private.example.com".parse().unwrap();
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
                email: "pending-request@example.org",
                display_name: "Pending Person",
                pre_verified: true,
                pre_confirmed: true,
                pre_approved: false,
                invitation: false,
            },
            &listmngr_db::AuditContext::system(),
            chrono::Utc::now().timestamp_millis(),
        )
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    // Passkeys need a domain as the relying-party id; `localhost` resolves to
    // the bound loopback address and is a secure context for WebAuthn.
    let public = format!("http://localhost:{}", address.port());
    let mut config = Config::default();
    config.site.base_url = public.clone();
    // The journey adds a webhook, which needs the site's signing key.
    config.webhooks.signing_key = Some("0123456789abcdef0123456789abcdef".into());
    let app = listmngr_api::router(db.clone(), config);
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let child_token_path = token_path.clone();
    let mut child = tokio::task::spawn_blocking(move || {
        std::process::Command::new(python)
            .arg(script)
            .env("WEBUI_URL", public)
            .env("WEBUI_OUTPUT", output)
            .env("WEBUI_CONFIRMATION_FILE", child_token_path)
            .env("WEBUI_TEST_PASSWORD", "very secure password")
            .status()
            .expect("launch browser")
    });
    let bridge = async {
        loop {
            let raws: Vec<Vec<u8>> = sqlx::query_scalar("SELECT raw FROM message_blobs")
                .fetch_all(db.pool())
                .await
                .unwrap();
            if let Some(secret) = raws
                .iter()
                .filter_map(|raw| std::str::from_utf8(raw).ok())
                .find_map(|raw| {
                    raw.split("Token: ")
                        .nth(1)
                        .and_then(|s| s.split_whitespace().next())
                })
            {
                std::fs::write(&token_path, secret).unwrap();
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    };
    let result = tokio::time::timeout(std::time::Duration::from_secs(120), async {
        tokio::select! { result = &mut child => result, () = bridge => child.await }
    })
    .await
    .expect("browser acceptance deadline");
    let _ = std::fs::remove_file(token_path);
    server.abort();
    assert!(result.unwrap().success(), "browser assertions failed");
    assert_eq!(
        db.preferences()
            .get(own.preferences_id)
            .await
            .unwrap()
            .delivery_status,
        Some(listmngr_core::DeliveryStatus::ByUser)
    );
    assert_eq!(
        db.members()
            .find("browser-joined@example.com")
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        db.moderation().get(held_id).await.unwrap().disposition,
        Some(listmngr_db::moderation::Disposition::Accepted)
    );
    for bulk in [bulk_a, bulk_b] {
        assert_eq!(
            db.moderation().get(bulk).await.unwrap().disposition,
            Some(listmngr_db::moderation::Disposition::Discarded),
            "the bulk decision discarded both"
        );
    }
    let sender_row: (String, Option<String>) = sqlx::query_as("SELECT m.role, m.moderation_action FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id='public.example.com' AND a.email='sender@example.com'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(
        sender_row,
        ("nonmember".into(), Some("hold".into())),
        "moderate sender from the queue"
    );
    let accepted_request: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id='private.example.com' AND a.email='pending-request@example.org' AND m.role='member'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(
        accepted_request, 1,
        "the request was accepted from the queue"
    );
    let recipients: Vec<String> = sqlx::query_scalar("SELECT email FROM delivery_recipients WHERE job_id IN (SELECT id FROM queue_jobs WHERE message_id=$1)")
        .bind(db.moderation().get(held_id).await.unwrap().message_id.0.to_string()).fetch_all(db.pool()).await.unwrap();
    assert_eq!(recipients, vec!["browser-joined@example.com"]);
    let made: listmngr_core::ListId = "browser-made.example.com".parse().unwrap();
    let made_list = db.lists().get(&made).await.unwrap();
    assert_eq!(made_list.display_name, "Browser made");
    assert_eq!(made_list.style_name, "legacy-announce");
    assert!(made_list.advertised);
    let made_owners = db
        .members()
        .roster(&made, listmngr_core::MemberRole::Owner)
        .await
        .unwrap();
    assert_eq!(made_owners.len(), 1, "the creator seated as owner");
    assert_eq!(
        db.addresses().get("browser@example.com").await.unwrap().id,
        made_owners[0].address_id
    );
    browser_left_the_database_consistent(&db).await;
    println!(
        "BROWSER DB PASS: paused preference persisted; confirmed member created; held accepted, two more discarded in bulk, the sender moderated, a subscription request accepted; a list created with its creator as owner; exact enabled recipient queued; logout revoked persistent session; signup left an unverified account with one live token; the reset request left one live reset token."
    );
}

/// After the browser run: every signed-in session was revoked, and the
/// anonymous signup left one unverified account with one live token.
async fn browser_left_the_database_consistent(db: &Database) {
    for (sql, expected, why) in [
        (
            "SELECT COUNT(*) FROM web_sessions WHERE user_id IS NOT NULL",
            0,
            "logout revoked every signed-in session",
        ),
        (
            "SELECT COUNT(*) FROM addresses WHERE email='newcomer@example.com' AND verified_on IS NULL AND user_id IS NOT NULL",
            1,
            "the browser signup created an unverified account",
        ),
        (
            "SELECT COUNT(*) FROM account_tokens WHERE purpose='verify_address' AND consumed_at IS NULL",
            2,
            "one live verification token each for the signup and the added address",
        ),
        (
            "SELECT COUNT(*) FROM account_tokens WHERE purpose='password_reset' AND consumed_at IS NULL",
            1,
            "one live reset token for the verified browser account",
        ),
        (
            "SELECT COUNT(*) FROM addresses WHERE email='browser-second@example.com' AND verified_on IS NULL AND user_id IS NOT NULL",
            1,
            "the added address is linked and unverified",
        ),
        (
            "SELECT COUNT(*) FROM api_tokens WHERE name='browser token' AND list_id='public.example.com' AND revoked_at IS NOT NULL",
            1,
            "the browser-minted token is bound to the owned list and revoked",
        ),
    ] {
        let count: i64 = sqlx::query_scalar(sql).fetch_one(db.pool()).await.unwrap();
        assert_eq!(count, expected, "{why}");
    }
}

async fn wire_confirmation(db: &Database, address: std::net::SocketAddr, c: &str, x: &str) {
    assert_eq!(
        wire(
            address,
            "POST",
            "/web/lists/public.example.com/request",
            c,
            &format!("csrf={x}&email=joined%40example.com&action=join")
        )
        .await
        .status(),
        StatusCode::ACCEPTED
    );
    let raw: Vec<u8> = sqlx::query_scalar("SELECT raw FROM message_blobs")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let raw = String::from_utf8(raw).unwrap();
    let token = raw
        .split("Token: ")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    assert_eq!(
        wire(
            address,
            "GET",
            &format!("/web/lists/public.example.com/confirm?token={token}"),
            c,
            ""
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert!(
        db.members()
            .find("joined@example.com")
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        wire(
            address,
            "POST",
            "/web/lists/public.example.com/confirm",
            c,
            &format!("csrf={x}&token={token}")
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert_eq!(
        db.members().find("joined@example.com").await.unwrap().len(),
        1
    );
}

#[tokio::test]
async fn session_expiry_after_pool_wait_and_rotation_audit_rollback() {
    let (db, _) = fixture().await;
    let u = user(&db, "session@example.com", false).await;
    let now = chrono::Utc::now().timestamp_millis();
    let anonymous = db.create_web_session(None, None, now).await.unwrap();
    sqlx::query("CREATE TRIGGER web_login_audit_failure BEFORE INSERT ON audit_log WHEN NEW.action='web.login' BEGIN SELECT RAISE(ABORT,'test sabotage'); END")
        .execute(db.pool()).await.unwrap();
    assert!(
        db.create_web_session(Some(u.id), Some(&anonymous.token), now)
            .await
            .is_err()
    );
    assert!(db.web_session(&anonymous.token, now).await.is_ok());
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM web_sessions")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
    sqlx::query("DROP TRIGGER web_login_audit_failure")
        .execute(db.pool())
        .await
        .unwrap();
    let authenticated = db
        .create_web_session(Some(u.id), Some(&anonymous.token), now)
        .await
        .unwrap();
    assert!(db.web_session(&anonymous.token, now).await.is_err());
    let expires = chrono::Utc::now().timestamp_millis() + 100;
    sqlx::query("UPDATE web_sessions SET expires_at=$1")
        .bind(expires)
        .execute(db.pool())
        .await
        .unwrap();
    assert!(
        db.web_session(&authenticated.token, expires).await.is_err(),
        "exact expiry rejects"
    );
    let connection = db.pool().acquire().await.unwrap();
    let lookup = db.web_session(&authenticated.token, now);
    let release = async {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        drop(connection);
    };
    let (result, ()) = tokio::join!(lookup, release);
    assert!(matches!(result, Err(listmngr_core::Error::Authentication)));
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_browser_session_forms_and_bounded_preview() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_session")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 2).await.unwrap();
    db.migrate().await.unwrap();
    let (db, app) = seeded_fixture(db).await;
    user(&db, "postgres@example.com", true).await;
    let own = member(
        &db,
        "postgres@example.com",
        listmngr_core::MemberRole::Member,
    )
    .await;
    let c = login_as(&app, "postgres@example.com").await;
    let html = text(call(&app, "GET", "/web/account", &c, "").await).await;
    assert_eq!(html.matches("<section>").count(), 1);
    let x = csrf(&html);
    let index = call(&app, "GET", "/web/moderation", &c, "").await;
    assert_eq!(index.status(), StatusCode::OK);
    let index = text(index).await;
    assert!(index.contains("public.example.com"));
    assert!(index.contains("private.example.com"));
    assert_eq!(
        call(&app, "GET", "/web", "", "").await.status(),
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("/web/members/{}/preferences", own.id),
            &c,
            &format!("csrf={x}&delivery_mode=regular&delivery_status=by_user")
        )
        .await
        .status(),
        StatusCode::SEE_OTHER
    );
    let id = held(&db).await;
    let html = text(call(&app, "GET", "/web/lists/public.example.com/held", &c, "").await).await;
    assert!(html.contains("&lt;script&gt;held"));
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("/web/lists/public.example.com/held/{}", id.0),
            &c,
            &format!("csrf={x}&action=discard&comment=PostgreSQL")
        )
        .await
        .status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        call(&app, "POST", "/web/logout", &c, &format!("csrf={x}"))
            .await
            .status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        call(&app, "GET", "/web/account", &c, "").await.status(),
        StatusCode::UNAUTHORIZED
    );
    println!(
        "POSTGRES WEB PASS: migrations, hashed session/login, bounded directory/byte preview, persisted preference, discard, logout/revocation."
    );
    schema.drop().await.unwrap();
}

/// The blocker owns a real database write lock. The observer proves the HTTP
/// request reached the mutation lock (old code) or authorization lock (fixed
/// code) before invoking the actual revocation repository API. No scheduler
/// sleep is used to assume that the request got far enough.
#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_inflight_browser_revocation_barrier() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_revocation")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 5).await.unwrap();
    db.migrate().await.unwrap();
    let (db, app) = seeded_fixture(db).await;
    for revoke in [false, true] {
        let email = if revoke {
            "revoked@example.com"
        } else {
            "valid@example.com"
        };
        let u = user(&db, email, false).await;
        let m = member(&db, email, listmngr_core::MemberRole::Member).await;
        let c = login_as(&app, email).await;
        let x = csrf(&text(call(&app, "GET", "/web/account", &c, "").await).await);
        let mut blocker = db.pool().begin().await.unwrap();
        sqlx::query("UPDATE preferences SET delivery_status=delivery_status WHERE id=$1")
            .bind(m.preferences_id.to_string())
            .execute(&mut *blocker)
            .await
            .unwrap();
        let before: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='preferences.update'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        let task_app = app.clone();
        let task = tokio::spawn(async move {
            call(
                &task_app,
                "POST",
                &format!("/web/members/{}/preferences", m.id),
                &c,
                &format!("csrf={x}&delivery_mode=regular&delivery_status=by_user"),
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let waiting: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid() AND ((query LIKE 'UPDATE preferences SET acknowledge_posts=%' AND wait_event_type='Lock') OR query LIKE 'LOCK TABLE web_sessions,%')")
                    .fetch_one(db.pool()).await.unwrap();
                if waiting > 0 { break; }
                assert!(!task.is_finished(), "request bypassed the write barrier");
                tokio::task::yield_now().await;
            }
        }).await.expect("request must reach actual database lock barrier");
        if revoke {
            db.users()
                .set_password(u.id, "a different secure password")
                .await
                .unwrap();
        }
        blocker.commit().await.unwrap();
        let response = tokio::time::timeout(std::time::Duration::from_secs(10), task)
            .await
            .unwrap()
            .unwrap();
        let after: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='preferences.update'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(
            response.status(),
            if revoke {
                StatusCode::UNAUTHORIZED
            } else {
                StatusCode::SEE_OTHER
            },
            "revocation={revoke}"
        );
        assert_eq!(after - before, i64::from(!revoke));
        assert_eq!(
            db.preferences()
                .get(m.preferences_id)
                .await
                .unwrap()
                .delivery_status,
            if revoke {
                None
            } else {
                Some(listmngr_core::DeliveryStatus::ByUser)
            }
        );
        println!(
            "PG LOCK BARRIER PASS revoke={revoke}: status={}, preference/audit delta verified",
            response.status()
        );
    }
    schema.drop().await.unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_browser_authority_matrix_at_lock_barrier() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_matrix")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 5).await.unwrap();
    db.migrate().await.unwrap();
    let (db, app) = seeded_fixture(db).await;
    for review in [false, true] {
        for change in [
            "valid",
            "logout",
            "password",
            "unverify",
            "unlink",
            "role",
            "as_user",
            "delete_member",
            "expiry",
            "server_owner",
            "policy_member",
            "policy_address",
            "policy_user",
        ] {
            if (review && change.starts_with("policy")) || (!review && change == "server_owner") {
                continue;
            }
            pg_authority_case(&db, &app, review, change).await;
        }
    }
    schema.drop().await.unwrap();
}

async fn pg_authority_case(db: &Database, app: &axum::Router, review: bool, change: &str) {
    use listmngr_core::MemberRole;
    let email = format!("{review}-{change}@example.com");
    let u = user(db, &email, change == "server_owner").await;
    let m = member(
        db,
        &email,
        if review {
            MemberRole::Moderator
        } else {
            MemberRole::Member
        },
    )
    .await;
    if change == "server_owner" {
        db.members().delete(m.id).await.unwrap();
    }
    let h = held(db).await;
    let session = db
        .create_web_session(Some(u.id), None, chrono::Utc::now().timestamp_millis())
        .await
        .unwrap();
    let expiry = chrono::Utc::now().timestamp_millis() + 1000;
    if change == "expiry" {
        sqlx::query("UPDATE web_sessions SET expires_at=$1 WHERE user_id=$2")
            .bind(expiry)
            .bind(u.id.to_string())
            .execute(db.pool())
            .await
            .unwrap();
    }
    let mut blocker = db.pool().begin().await.unwrap();
    sqlx::query("UPDATE held_messages SET reason=reason WHERE id=$1")
        .bind(h.0.to_string())
        .execute(&mut *blocker)
        .await
        .unwrap();
    let task_app = app.clone();
    let c = format!("listmngr_session={}", session.token);
    let body = if review {
        format!("csrf={}&action=accept&comment=barrier", session.csrf)
    } else {
        format!(
            "csrf={}&delivery_mode=mime_digests&delivery_status=by_user",
            session.csrf
        )
    };
    let route = if review {
        format!("/web/lists/public.example.com/held/{}", h.0)
    } else {
        format!("/web/members/{}/preferences", m.id)
    };
    let task = tokio::spawn(async move { call(&task_app, "POST", &route, &c, &body).await });
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    let attempted: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid() AND query LIKE 'LOCK TABLE web_sessions,%'")
                        .fetch_one(db.pool()).await.unwrap();
                    if attempted>0 {break;}
                    assert!(!task.is_finished(),"request bypassed barrier review={review} change={change}");
                    tokio::task::yield_now().await;
                }
            }).await.unwrap();
    revoke_pg_authority(db, &session, &u, &m, change, review, expiry).await;
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='preferences.update' OR action LIKE 'moderation.%'").fetch_one(db.pool()).await.unwrap();
    blocker.commit().await.unwrap();
    let response = tokio::time::timeout(std::time::Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap();
    verify_pg_effects(db, &m, h, review, change, before, response).await;
}
async fn revoke_pg_authority(
    db: &Database,
    session: &listmngr_db::web_sessions::WebSession,
    u: &listmngr_core::User,
    m: &listmngr_core::Member,
    change: &str,
    review: bool,
    expiry: i64,
) {
    use listmngr_core::{DeliveryStatus, Preferences};
    let email = &format!("{review}-{change}@example.com");
    match change {
        "valid" => {}
        "logout" => db.delete_web_session(session).await.unwrap(),
        "password" => db
            .users()
            .set_password(u.id, "another secure password")
            .await
            .unwrap(),
        "unverify" => {
            db.addresses().verify(email, false).await.unwrap();
        }
        "unlink" => {
            db.addresses().link(email, None).await.unwrap();
        }
        "role" => {
            db.members()
                .update(
                    m.id,
                    &serde_json::json!({"role":if review {"member"}else{"moderator"}}),
                )
                .await
                .unwrap();
        }
        "as_user" => {
            db.members()
                .update(m.id, &serde_json::json!({"subscription_mode":"as_user"}))
                .await
                .unwrap();
            sqlx::query("UPDATE members SET user_id=NULL WHERE id=$1")
                .bind(m.id.to_string())
                .execute(db.pool())
                .await
                .unwrap();
        }
        "delete_member" => db.members().delete(m.id).await.unwrap(),
        "expiry" => {
            while chrono::Utc::now().timestamp_millis() <= expiry {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }
        "server_owner" => {
            sqlx::query("UPDATE users SET is_server_owner=0 WHERE id=$1")
                .bind(u.id.to_string())
                .execute(db.pool())
                .await
                .unwrap();
        }
        "policy_member" => db
            .preferences()
            .set_member(
                m.id,
                Preferences {
                    delivery_status: Some(DeliveryStatus::ByModerator),
                    ..Preferences::default()
                },
            )
            .await
            .unwrap(),
        "policy_address" => db
            .preferences()
            .set_address(
                email,
                Preferences {
                    delivery_status: Some(DeliveryStatus::ByBounces),
                    ..Preferences::default()
                },
            )
            .await
            .unwrap(),
        "policy_user" => db
            .preferences()
            .set_user(
                u.id,
                Preferences {
                    delivery_status: Some(DeliveryStatus::Unknown),
                    ..Preferences::default()
                },
            )
            .await
            .unwrap(),
        _ => unreachable!(),
    }
}
async fn verify_pg_effects(
    db: &Database,
    m: &listmngr_core::Member,
    h: listmngr_db::moderation::HeldId,
    review: bool,
    change: &str,
    before: i64,
    response: Response,
) {
    let valid = change == "valid";
    let expected = if valid {
        StatusCode::SEE_OTHER
    } else if matches!(change, "logout" | "password" | "expiry") {
        StatusCode::UNAUTHORIZED
    } else {
        StatusCode::FORBIDDEN
    };
    assert_eq!(
        response.status(),
        expected,
        "review={review},change={change}"
    );
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='preferences.update' OR action LIKE 'moderation.%'").fetch_one(db.pool()).await.unwrap();
    assert_eq!(
        after - before,
        i64::from(valid),
        "audit review={review} change={change}"
    );
    let item = db.moderation().get(h).await.unwrap();
    let children: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs WHERE message_id=$1 AND queue='out'")
            .bind(item.message_id.0.to_string())
            .fetch_one(db.pool())
            .await
            .unwrap();
    let logs: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM moderation_log WHERE held_id=$1 AND action='accept'",
    )
    .bind(h.0.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(children, i64::from(review && valid));
    assert_eq!(logs, i64::from(review && valid));
    assert_eq!(item.disposition.is_some(), review && valid);
    if !review && change != "delete_member" {
        assert_eq!(
            db.preferences()
                .get(m.preferences_id)
                .await
                .unwrap()
                .delivery_mode,
            if valid {
                Some(listmngr_core::DeliveryMode::MimeDigests)
            } else {
                None
            }
        );
    }
    println!(
        "PG AUTHORITY MATRIX PASS review={review} change={change}: HTTP {}, business/audit/Out/log verified",
        response.status()
    );
}

/// The Phase 4 acceptance journey at a phone viewport: one person signs up,
/// verifies, signs in, creates a list, subscribes an address with mailbox
/// confirmation, sees the first post held, accepts it from the moderation
/// page, changes a setting and signs out. The harness bridges the mails'
/// tokens, seats the new account as a domain owner and holds the post; it
/// then checks the database for what the browser claimed.
#[tokio::test]
#[ignore = "requires WEBUI_BROWSER_PYTHON, WEBUI_JOURNEY_SCRIPT and WEBUI_BROWSER_OUTPUT"]
async fn chromium_acceptance_journey() {
    let python = std::env::var("WEBUI_BROWSER_PYTHON").expect("browser Python");
    let script = std::env::var("WEBUI_JOURNEY_SCRIPT").expect("journey script");
    let output =
        std::env::var("WEBUI_BROWSER_OUTPUT").expect("outside-repository evidence directory");
    private_browser_evidence_directory(&output);
    let bridge = std::path::Path::new(&output).join(".journey");
    let _ = std::fs::remove_dir_all(&bridge);
    std::fs::create_dir_all(&bridge).unwrap();
    // A file, not `sqlite::memory:`: the pool's one connection is dropped
    // with whichever task holds it when `select!` abandons the bridge or
    // the server is aborted, and a reconnect to an in-memory database is
    // an empty database ("no such table" after JOURNEY PASS, on CI).
    let database_file = std::path::Path::new(&output).join(".journey.db");
    let _ = std::fs::remove_file(&database_file);
    let db = Database::connect(&format!("sqlite://{}?mode=rwc", database_file.display()), 1)
        .await
        .unwrap();
    db.migrate().await.unwrap();
    let (db, _) = seeded_fixture(db).await;
    user(&db, "browser@example.com", true).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let public = format!("http://localhost:{}", address.port());
    let mut config = Config::default();
    config.site.base_url = public.clone();
    let app = listmngr_api::router(db.clone(), config);
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let child_bridge = bridge.clone();
    let mut child = tokio::task::spawn_blocking(move || {
        std::process::Command::new(python)
            .arg(script)
            .env("WEBUI_URL", public)
            .env("WEBUI_OUTPUT", output)
            .env("WEBUI_BRIDGE_DIR", child_bridge)
            .env("WEBUI_TEST_PASSWORD", "walrus-corridor-lantern-92")
            .status()
            .expect("launch browser")
    });
    let bridge_db = db.clone();
    let bridge_dir = bridge.clone();
    let bridge_task = async move {
        let mut tokens: Vec<String> = Vec::new();
        let mut granted = false;
        let mut held = false;
        loop {
            journey_tokens(&bridge_db, &bridge_dir, &mut tokens).await;
            if !granted {
                granted = journey_grant(&bridge_db, &bridge_dir).await;
            }
            if !held {
                held = journey_hold(&bridge_db, &bridge_dir).await;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    };
    let result = tokio::time::timeout(std::time::Duration::from_secs(420), async {
        tokio::select! { result = &mut child => result, () = bridge_task => child.await }
    })
    .await
    .expect("journey deadline");
    let _ = std::fs::remove_dir_all(&bridge);
    server.abort();
    assert!(result.unwrap().success(), "journey assertions failed");
    journey_database_state(&db).await;
    drop(db);
    let _ = std::fs::remove_file(&database_file);
}

/// What the browser claimed, checked in the database after the journey.
async fn journey_database_state(db: &Database) {
    let journey = db
        .users()
        .get_by_email("journey@example.com")
        .await
        .unwrap();
    assert!(
        db.addresses()
            .get("journey@example.com")
            .await
            .unwrap()
            .verified_on
            .is_some()
    );
    let list: listmngr_core::ListId = "journey.example.com".parse().unwrap();
    let stored = db.lists().get(&list).await.unwrap();
    assert_eq!(stored.display_name, "Journey list");
    assert_eq!(stored.description, "Set during the journey");
    let owners = db
        .members()
        .roster(&list, listmngr_core::MemberRole::Owner)
        .await
        .unwrap();
    assert_eq!(owners.len(), 1);
    assert_eq!(owners[0].user_id, Some(journey.id));
    let members = db
        .members()
        .roster(&list, listmngr_core::MemberRole::Member)
        .await
        .unwrap();
    assert_eq!(members.len(), 1, "the confirmed subscription");
    let accepted: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM held_messages WHERE list_id='journey.example.com' AND disposition='accepted'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(
        accepted, 1,
        "the held post accepted from the moderation page"
    );
    let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM web_sessions WHERE user_id=$1")
        .bind(journey.id.to_string())
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(sessions, 0, "logout ended the session");
    println!(
        "JOURNEY DB PASS: account verified; list created with its creator as owner; one confirmed member; the held post accepted; the description saved; no session left."
    );
}

/// Every token the site has mailed so far, each written once as
/// `token-<n>` in the order the mails appeared.
async fn journey_tokens(db: &Database, bridge: &std::path::Path, seen: &mut Vec<String>) {
    let raws: Vec<Vec<u8>> = sqlx::query_scalar("SELECT raw FROM message_blobs")
        .fetch_all(db.pool())
        .await
        .unwrap_or_default();
    for raw in raws {
        let Ok(text) = std::str::from_utf8(&raw) else {
            continue;
        };
        let text = text.replace("\r\n", "\n");
        let found = text
            .split("Token: ")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .or_else(|| {
                text.split("enter this token:")
                    .nth(1)
                    .and_then(|rest| rest.split_whitespace().next())
            });
        if let Some(token) = found
            && !seen.iter().any(|known| known == token)
        {
            seen.push(token.to_owned());
            std::fs::write(bridge.join(format!("token-{}", seen.len())), token).unwrap();
        }
    }
}

/// Once the journey account has a verified address, seat it as an owner of
/// the fixture domain so it may create a list.
async fn journey_grant(db: &Database, bridge: &std::path::Path) -> bool {
    let Ok(address) = db.addresses().get("journey@example.com").await else {
        return false;
    };
    let (Some(user), Some(_)) = (address.user_id, address.verified_on) else {
        return false;
    };
    db.domains().add_owner("example.com", user).await.unwrap();
    std::fs::write(bridge.join("owner-granted"), "granted").unwrap();
    true
}

/// Once the friend's subscription is confirmed, hold a nonmember post on
/// the new list, as the mail path would.
async fn journey_hold(db: &Database, bridge: &std::path::Path) -> bool {
    let list: listmngr_core::ListId = match "journey.example.com".parse() {
        Ok(list) => list,
        Err(_) => return false,
    };
    let Ok(members) = db
        .members()
        .roster(&list, listmngr_core::MemberRole::Member)
        .await
    else {
        return false;
    };
    if members.is_empty() {
        return false;
    }
    held_on(db, "journey.example.com").await;
    std::fs::write(bridge.join("post-held"), "held").unwrap();
    true
}
