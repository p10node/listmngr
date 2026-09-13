use super::{call, csrf, fixture, login_as, member, seeded_fixture, text, user};
use axum::http::StatusCode;
use listmngr_core::MemberRole;
use listmngr_db::Database;

#[tokio::test]
async fn literal_member_search_preserves_selection() {
    let (db, app) = fixture().await;
    matrix(db, app).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_literal_member_search() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_search")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 3).await.unwrap();
    db.migrate().await.unwrap();
    let (db, app) = seeded_fixture(db).await;
    matrix(db, app).await;
    schema.drop().await.unwrap();
}

async fn matrix(db: Database, app: axum::Router) {
    user(&db, "owner@example.com", false).await;
    member(&db, "owner@example.com", MemberRole::Owner).await;
    for i in 0..25 {
        member(&db, &format!("aaa{i:02}@example.com"), MemberRole::Member).await;
        member(&db, &format!("z%_!+{i:02}@example.com"), MemberRole::Member).await;
    }
    let cookie = login_as(&app, "owner@example.com").await;
    let query = "Z%_!+";
    let path = format!(
        "/web/lists/public.example.com/members?{}",
        serde_urlencoded::to_string([("q", query)]).unwrap()
    );
    let response = call(&app, "GET", &path, &cookie, "").await;
    assert_eq!(response.status(), StatusCode::OK);
    let first = text(response).await;
    assert_eq!(first.matches("<article>").count(), 20);
    assert!(first.contains("z%_!+00@example.com"));
    assert!(!first.contains("aaa00@example.com"));
    let next = link(&first, "Next page");
    let second = text(call(&app, "GET", &next, &cookie, "").await).await;
    assert_eq!(second.matches("<article>").count(), 5);
    assert!(second.contains("z%_!+24@example.com"));
    assert!(!second.contains("z%_!+00@example.com"));
    let previous = link(&second, "Previous page");
    let previous = text(call(&app, "GET", &previous, &cookie, "").await).await;
    assert!(previous.contains("z%_!+00@example.com"));
    save_from_page(&db, &app, &cookie, &second).await;
    negatives(&app, &cookie).await;
}

fn link(html: &str, label: &str) -> String {
    html.split(&format!("\">{label}</a>"))
        .next()
        .unwrap()
        .rsplit("href=\"")
        .next()
        .unwrap()
        .replace("&amp;", "&")
}

async fn save_from_page(db: &Database, app: &axum::Router, cookie: &str, html: &str) {
    let target = db
        .members()
        .find("z%_!+24@example.com")
        .await
        .unwrap()
        .remove(0);
    let form = serde_urlencoded::to_string([
        ("csrf", csrf(html)),
        ("action", "hold".into()),
        ("q", hidden_value(html, "q")),
        ("page", hidden_value(html, "page")),
    ])
    .unwrap();
    let path = format!("/web/lists/public.example.com/members/{}/policy", target.id);
    invalid_return_query(db, app, cookie, &csrf(html), target.id).await;
    let response = call(app, "POST", &path, cookie, &form).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let redirect = response.headers()["location"].to_str().unwrap();
    assert!(redirect.starts_with("/web/lists/public.example.com/members?"));
    let saved = text(call(app, "GET", redirect, cookie, "").await).await;
    assert_eq!(saved.matches("<article>").count(), 5);
    assert!(saved.contains("z%_!+24@example.com"));
    assert_eq!(
        db.members().get(target.id).await.unwrap().moderation_action,
        Some(listmngr_core::ModerationAction::Hold)
    );
}

fn hidden_value(html: &str, name: &str) -> String {
    html.split(&format!("type=\"hidden\" name=\"{name}\" value=\""))
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .to_owned()
}

async fn invalid_return_query(
    db: &Database,
    app: &axum::Router,
    cookie: &str,
    csrf: &str,
    member: listmngr_core::MemberId,
) {
    for (q, page) in [("x".repeat(321), "0"), (String::new(), "10001")] {
        let form = serde_urlencoded::to_string([
            ("csrf", csrf),
            ("action", "discard"),
            ("q", q.as_str()),
            ("page", page),
        ])
        .unwrap();
        let path = format!("/web/lists/public.example.com/members/{member}/policy");
        assert_eq!(
            call(app, "POST", &path, cookie, &form).await.status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            db.members().get(member).await.unwrap().moderation_action,
            None
        );
    }
}

async fn reject_control_queries(app: &axum::Router, cookie: &str) {
    for q in ["\0", "\n", "\r", "\t"] {
        let path = format!(
            "/web/lists/public.example.com/members?{}",
            serde_urlencoded::to_string([("q", q)]).unwrap()
        );
        assert_eq!(
            call(app, "GET", &path, cookie, "").await.status(),
            StatusCode::BAD_REQUEST
        );
    }
}

async fn negatives(app: &axum::Router, cookie: &str) {
    reject_control_queries(app, cookie).await;
    for query in ["missing", "<script>\"&", "aaa00%", "aaa0_"] {
        let path = format!(
            "/web/lists/public.example.com/members?{}",
            serde_urlencoded::to_string([("q", query)]).unwrap()
        );
        let html = text(call(app, "GET", &path, cookie, "").await).await;
        assert_eq!(html.matches("<article>").count(), 0);
        assert!(!html.contains("<script>"));
    }
    assert_eq!(
        call(
            app,
            "GET",
            "/web/lists/private.example.com/members?q=aaa",
            cookie,
            ""
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    let huge = format!(
        "/web/lists/public.example.com/members?q={}",
        "x".repeat(321)
    );
    assert_eq!(
        call(app, "GET", &huge, cookie, "").await.status(),
        StatusCode::BAD_REQUEST
    );
}
