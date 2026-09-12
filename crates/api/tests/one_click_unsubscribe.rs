//! RFC 8058 one-click unsubscribe: `POST /unsubscribe/{list_id}?token=…`
//! with the body `List-Unsubscribe=One-Click` removes the membership at
//! once; `GET` shows a zero-JS confirmation page; bad, foreign, expired and
//! reused tokens change nothing.
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use listmngr_core::{Config, ListId, MemberRole, SubscriptionMode};
use listmngr_db::{Database, NewList, NewMember};
use tower::ServiceExt;

const LIST: &str = "one.example.invalid";

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

async fn fixture() -> (Database, axum::Router, listmngr_core::Member) {
    let db = Database::connect("sqlite::memory:", 1)
        .await
        .unwrap()
        .with_base_url("https://lists.example.invalid");
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: LIST.parse().unwrap(),
            display_name: "One Click".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    db.lists()
        .update(&list.id, &serde_json::json!({"send_goodbye_message": true}))
        .await
        .unwrap();
    let member = db
        .members()
        .create(NewMember {
            list_id: list.id,
            email: "reader@example.invalid".into(),
            display_name: String::new(),
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsAddress,
        })
        .await
        .unwrap();
    let app = listmngr_api::router(db.clone(), Config::default());
    (db, app, member)
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<&str>,
) -> (StatusCode, String) {
    let mut request = Request::builder().method(method).uri(uri);
    if body.is_some() {
        request = request.header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    }
    let mut request = request
        .body(Body::from(body.unwrap_or_default().to_owned()))
        .unwrap();
    request.extensions_mut().insert(axum::extract::ConnectInfo(
        "203.0.113.9:4242".parse::<std::net::SocketAddr>().unwrap(),
    ));
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8_lossy(&body).into_owned())
}

async fn member_count(db: &Database) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM members")
        .fetch_one(db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn a_post_with_the_rfc_body_unsubscribes_at_once_with_goodbye_and_audit() {
    let (db, app, member) = fixture().await;
    let list: ListId = LIST.parse().unwrap();
    let url = db
        .one_click()
        .url_for(
            "https://lists.example.invalid",
            &list,
            "reader@example.invalid",
            now(),
        )
        .await
        .unwrap()
        .expect("a member gets a link");
    assert!(
        url.starts_with("https://lists.example.invalid/unsubscribe/one.example.invalid?token=")
    );
    assert!(!url.contains("reader"), "the link never names the address");
    let path = url.trim_start_matches("https://lists.example.invalid");

    let (status, body) = call(&app, "POST", path, Some("List-Unsubscribe=One-Click")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("unsubscribed"), "{body}");
    assert_eq!(member_count(&db).await, 0);
    let audit: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM audit_log WHERE target_type='member' AND target_id=$1 ORDER BY at",
    )
    .bind(member.id.to_string())
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert!(
        audit.contains(&"member.unsubscribe.one_click".to_owned()),
        "{audit:?}"
    );
    let goodbye: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workflow_notices")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(goodbye, 1, "the list's goodbye notice is queued");

    // Reuse: the membership is gone, so the same link is not found.
    let (status, _) = call(&app, "POST", path, Some("List-Unsubscribe=One-Click")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_shows_a_confirmation_form_and_changes_nothing() {
    let (db, app, _) = fixture().await;
    let list: ListId = LIST.parse().unwrap();
    let url = db
        .one_click()
        .url_for(
            "https://lists.example.invalid",
            &list,
            "reader@example.invalid",
            now(),
        )
        .await
        .unwrap()
        .unwrap();
    let path = url.trim_start_matches("https://lists.example.invalid");
    let (status, body) = call(&app, "GET", path, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<form method=\"post\""), "{body}");
    assert!(body.contains("One Click"), "the list is named: {body}");
    assert!(!body.contains("<script"), "zero-JS page");
    assert_eq!(member_count(&db).await, 1);

    // The human form posts the same way the mail provider does.
    let (status, _) = call(&app, "POST", path, Some("List-Unsubscribe=One-Click")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(member_count(&db).await, 0);
}

#[tokio::test]
async fn bad_foreign_expired_and_wrong_body_requests_change_nothing() {
    let (db, app, member) = fixture().await;
    let list: ListId = LIST.parse().unwrap();
    let signer = db.one_click().signer().await.unwrap();
    let good = signer.issue(&list, member.id, now());
    let other: ListId = "other.example.invalid".parse().unwrap();
    let foreign = signer.issue(&other, member.id, now());
    let expired = signer.issue(
        &list,
        member.id,
        now() - listmngr_core::one_click::TTL_SECS - 10,
    );
    for (label, path, body, status) in [
        (
            "wrong body",
            format!("/unsubscribe/{LIST}?token={good}"),
            "List-Unsubscribe=Later",
            StatusCode::BAD_REQUEST,
        ),
        (
            "missing token",
            format!("/unsubscribe/{LIST}"),
            "List-Unsubscribe=One-Click",
            StatusCode::BAD_REQUEST,
        ),
        (
            "garbage token",
            format!("/unsubscribe/{LIST}?token=not.a.token"),
            "List-Unsubscribe=One-Click",
            StatusCode::NOT_FOUND,
        ),
        (
            "foreign list",
            format!("/unsubscribe/{LIST}?token={foreign}"),
            "List-Unsubscribe=One-Click",
            StatusCode::NOT_FOUND,
        ),
        (
            "expired",
            format!("/unsubscribe/{LIST}?token={expired}"),
            "List-Unsubscribe=One-Click",
            StatusCode::NOT_FOUND,
        ),
        (
            "unknown list",
            format!("/unsubscribe/nope.example.invalid?token={good}"),
            "List-Unsubscribe=One-Click",
            StatusCode::NOT_FOUND,
        ),
    ] {
        let (got, _) = call(&app, "POST", &path, Some(body)).await;
        assert_eq!(got, status, "{label}");
        assert_eq!(member_count(&db).await, 1, "{label} changed the roster");
    }
    let (status, _) = call(
        &app,
        "GET",
        &format!("/unsubscribe/{LIST}?token=not.a.token"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn only_members_get_links_and_the_key_is_stable() {
    let (db, _, _) = fixture().await;
    let list: ListId = LIST.parse().unwrap();
    assert!(
        db.one_click()
            .url_for("https://x.invalid", &list, "stranger@example.invalid", 1)
            .await
            .unwrap()
            .is_none()
    );
    let first = db
        .one_click()
        .url_for("https://x.invalid", &list, "Reader@Example.invalid", 1)
        .await
        .unwrap()
        .unwrap();
    let second = db
        .one_click()
        .url_for("https://x.invalid", &list, "reader@example.invalid", 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        first, second,
        "the key is generated once and the address is canonical"
    );
    let secrets: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM site_secrets")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(secrets, 1);
}
