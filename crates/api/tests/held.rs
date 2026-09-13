#[path = "held/rejection_notice.rs"]
mod rejection_notice;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
    response::Response,
};
use listmngr_core::{Config, ListId, MemberRole, SubscriptionMode};
use listmngr_db::mail_queue::{NewMessage, Queue};
use listmngr_db::{Database, NewList, NewMember, NewUser};
use serde_json::Value;
use tower::ServiceExt;

struct Fixture {
    app: axum::Router,
    db: Database,
    admin_token: String,
    list_id: ListId,
}

async fn fixture() -> Fixture {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    fixture_with_db(db).await
}

async fn fixture_with_db(db: Database) -> Fixture {
    db.migrate().await.unwrap();
    db.domains()
        .create("dev.example.invalid", "dev", None)
        .await
        .unwrap();
    let list_id: ListId = "dev.dev.example.invalid".parse().unwrap();
    db.lists()
        .create(NewList {
            list_id: list_id.clone(),
            display_name: "Dev".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    // These fixtures exercise moderation, not hold notices: keep the queue
    // limited to the posts and the notices under test.
    db.lists()
        .update(
            &list_id,
            &serde_json::json!({"respond_to_post_requests": false, "admin_immed_notify": false}),
        )
        .await
        .unwrap();
    db.members()
        .create(NewMember {
            list_id: list_id.clone(),
            email: "member@example.invalid".into(),
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsAddress,
            display_name: String::new(),
        })
        .await
        .unwrap();
    let user = db
        .users()
        .create(NewUser {
            display_name: "Admin".into(),
            email: "admin@example.invalid".into(),
            password: "very secure password".into(),
            server_owner: true,
        })
        .await
        .unwrap();
    let admin_token = db
        .tokens()
        .create(user.id, "admin", &["admin"], None)
        .await
        .unwrap()
        .token;
    let mut config = Config::default();
    config.security.rate_limit.api = "1000/min".into();
    let app = listmngr_api::router(db.clone(), config);
    Fixture {
        app,
        db,
        admin_token,
        list_id,
    }
}

/// Seed one durable submission and hold it, exactly like the `in` processor
/// would (claim the `in` job, then `moderation().hold`), without going
/// through a real LMTP socket: this test exercises the REST layer, not intake.
async fn seed_held(
    db: &Database,
    list_id: &ListId,
    sender: &str,
    subject: &str,
) -> listmngr_db::moderation::HeldId {
    let raw = format!(
        "Message-ID: <{}@example.invalid>\r\nSubject: {subject}\r\n\r\nbody",
        uuid::Uuid::now_v7()
    );
    let job = db
        .mail_queue()
        .enqueue(
            NewMessage {
                raw: raw.into_bytes(),
                external_id: format!("<seed-{}@example.invalid>", uuid::Uuid::now_v7()),
                context: serde_json::json!({"version":1,"list_id":list_id.to_string(),"envelope_sender":sender}).to_string(),
                queue: Queue::In,
                max_attempts: 5,
            },
            1000,
        )
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::In, "test-worker", 1000, 10_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lease.job.id, job.id);
    db.moderation()
        .hold(&lease, list_id, sender, subject, "nonmember", 1000)
        .await
        .unwrap()
        .id
}

async fn get(app: &axum::Router, uri: &str, token: &str) -> Response {
    let mut request = Request::builder()
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    request.extensions_mut().insert(axum::extract::ConnectInfo(
        "127.0.0.1:4242".parse::<std::net::SocketAddr>().unwrap(),
    ));
    app.clone().oneshot(request).await.unwrap()
}

async fn post_form(app: &axum::Router, uri: &str, token: &str, form: &str) -> Response {
    let mut request = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(form.to_owned()))
        .unwrap();
    request.extensions_mut().insert(axum::extract::ConnectInfo(
        "127.0.0.1:4242".parse::<std::net::SocketAddr>().unwrap(),
    ));
    app.clone().oneshot(request).await.unwrap()
}

async fn post_action(f: &Fixture, uri: &str, action: &str, json: bool) -> Response {
    let (content_type, body) = if json {
        (
            "application/json",
            serde_json::json!({"action":action,"comment":"reviewed + nguyên"}).to_string(),
        )
    } else {
        (
            "application/x-www-form-urlencoded",
            format!("action={action}&comment=reviewed+%2B+nguy%C3%AAn"),
        )
    };
    let mut request = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {}", f.admin_token))
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from(body))
        .unwrap();
    request.extensions_mut().insert(axum::extract::ConnectInfo(
        "127.0.0.1:4242".parse::<std::net::SocketAddr>().unwrap(),
    ));
    f.app.clone().oneshot(request).await.unwrap()
}

#[tokio::test]
async fn accept_and_defer_preserve_comments_on_both_prefixes_and_encodings() {
    let f = fixture().await;
    check_accept_and_defer_preserve_comments_on_both_prefixes_and_encodings(&f).await;
}

async fn check_accept_and_defer_preserve_comments_on_both_prefixes_and_encodings(f: &Fixture) {
    for prefix in ["/api/v1", "/3.1"] {
        for json in [false, true] {
            for action in ["accept", "defer"] {
                let id = seed_held(&f.db, &f.list_id, "sender@example.invalid", "Comment").await;
                let uri = format!("{prefix}/lists/{}/held/{}", f.list_id, id.0);
                assert_eq!(
                    post_action(f, &uri, action, json).await.status(),
                    StatusCode::NO_CONTENT
                );
                let reasons: Vec<String> = sqlx::query_scalar(
                    "SELECT reason FROM moderation_log WHERE held_id=$1 AND action=$2",
                )
                .bind(id.0.to_string())
                .bind(action)
                .fetch_all(f.db.pool())
                .await
                .unwrap();
                assert_eq!(
                    reasons,
                    vec!["reviewed + nguyên"],
                    "{prefix} json={json} action={action}"
                );
            }
        }
    }
}

#[tokio::test]
async fn moderation_audit_preserves_edge_context() {
    let f = fixture().await;
    check_moderation_audit_preserves_edge_context(&f).await;
}

async fn check_moderation_audit_preserves_edge_context(f: &Fixture) {
    let expected: (String, String) =
        sqlx::query_as("SELECT user_id,id FROM api_tokens WHERE name='admin'")
            .fetch_one(f.db.pool())
            .await
            .unwrap();
    for action in ["accept", "reject", "discard", "defer"] {
        let id = seed_held(&f.db, &f.list_id, "sender@example.invalid", "Audit").await;
        let uri = format!("/api/v1/lists/{}/held/{}", f.list_id, id.0);
        assert_eq!(
            post_action(f, &uri, action, true).await.status(),
            StatusCode::NO_CONTENT
        );
        let audit: (Option<String>, Option<String>, Option<String>, String) = sqlx::query_as("SELECT actor_user_id,actor_token_id,ip,diff FROM audit_log WHERE target_id=$1 AND action != 'moderation.hold'")
            .bind(id.0.to_string()).fetch_one(f.db.pool()).await.unwrap();
        assert_eq!(audit.0.as_deref(), Some(expected.0.as_str()));
        assert_eq!(
            audit.1.as_deref(),
            Some(expected.1.as_str()),
            "{action} token lost"
        );
        assert_eq!(audit.2.as_deref(), Some("127.0.0.1"));
        assert_eq!(
            serde_json::from_str::<Value>(&audit.3).unwrap()["reason"],
            "reviewed + nguyên"
        );
    }
}

#[tokio::test]
async fn disposed_defer_conflicts_without_stray_writes() {
    let f = fixture().await;
    check_disposed_defer_conflicts_without_stray_writes(&f).await;
}

async fn check_disposed_defer_conflicts_without_stray_writes(f: &Fixture) {
    for action in ["accept", "reject", "discard"] {
        let id = seed_held(&f.db, &f.list_id, "sender@example.invalid", "Disposed").await;
        let uri = format!("/3.1/lists/{}/held/{}", f.list_id, id.0);
        assert_eq!(
            post_action(f, &uri, action, false).await.status(),
            StatusCode::NO_CONTENT
        );
        let before = held_write_counts(&f.db).await;
        assert_eq!(
            post_action(f, &uri, "defer", true).await.status(),
            StatusCode::CONFLICT
        );
        assert_eq!(held_write_counts(&f.db).await, before);
    }
}

async fn held_write_counts(db: &Database) -> (i64, i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT COUNT(*) FROM queue_jobs WHERE queue='out'), (SELECT COUNT(*) FROM moderation_log), (SELECT COUNT(*) FROM audit_log WHERE target_type='held_message'), (SELECT COUNT(*) FROM delivery_recipients)")
        .fetch_one(db.pool()).await.unwrap()
}

#[tokio::test]
async fn racing_accept_and_defer_have_a_serializable_result() {
    let f = fixture().await;
    check_racing_accept_and_defer_have_a_serializable_result(&f).await;
}

async fn check_racing_accept_and_defer_have_a_serializable_result(f: &Fixture) {
    let id = seed_held(&f.db, &f.list_id, "sender@example.invalid", "Race").await;
    let uri = format!("/api/v1/lists/{}/held/{}", f.list_id, id.0);
    let before = held_write_counts(&f.db).await;
    let (accept, defer) = tokio::join!(
        post_action(f, &uri, "accept", true),
        post_action(f, &uri, "defer", false)
    );
    assert_eq!(accept.status(), StatusCode::NO_CONTENT);
    assert!(matches!(
        defer.status(),
        StatusCode::NO_CONTENT | StatusCode::CONFLICT
    ));
    let deferred = i64::from(defer.status() == StatusCode::NO_CONTENT);
    let after = held_write_counts(&f.db).await;
    assert_eq!(
        after,
        (
            before.0 + 1,
            before.1 + 1 + deferred,
            before.2 + 1 + deferred,
            before.3 + 1
        )
    );
    assert_eq!(
        post_action(f, &uri, "defer", false).await.status(),
        StatusCode::CONFLICT
    );
    assert_eq!(held_write_counts(&f.db).await, after);
}

#[tokio::test]
async fn moderation_audit_failure_rolls_back_every_business_write() {
    let f = fixture().await;
    for action in ["accept", "reject", "discard", "defer"] {
        let id = seed_held(&f.db, &f.list_id, "sender@example.invalid", "Rollback").await;
        let held = f.db.moderation().get(id).await.unwrap();
        let before = held_write_counts(&f.db).await;
        sqlx::query("CREATE TRIGGER fail_review_audit BEFORE INSERT ON audit_log WHEN NEW.target_type='held_message' BEGIN SELECT RAISE(ABORT, 'audit sabotage'); END").execute(f.db.pool()).await.unwrap();
        let uri = format!("/api/v1/lists/{}/held/{}", f.list_id, id.0);
        assert_eq!(
            post_action(&f, &uri, action, true).await.status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(f.db.moderation().get(id).await.unwrap(), held);
        assert_eq!(held_write_counts(&f.db).await, before);
        sqlx::query("DROP TRIGGER fail_review_audit")
            .execute(f.db.pool())
            .await
            .unwrap();
        assert_eq!(
            post_action(&f, &uri, action, true).await.status(),
            StatusCode::NO_CONTENT
        );
    }
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; creates and drops only a unique test schema"]
async fn postgres_isolated_held_review_contract() {
    let url = std::env::var("TEST_POSTGRES_URL").expect("TEST_POSTGRES_URL is required");
    assert!(url.starts_with("postgres://") || url.starts_with("postgresql://"));
    assert!(!url.contains("options="));
    let admin = Database::connect(&url, 1).await.unwrap();
    let schema = format!("held_review_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(admin.pool())
        .await
        .unwrap();
    let separator = if url.contains('?') { '&' } else { '?' };
    let fixture_url = format!("{url}{separator}options=-csearch_path%3D{schema}");
    let result = tokio::spawn(async move {
        let f = fixture_with_db(Database::connect(&fixture_url, 4).await.unwrap()).await;
        check_accept_and_defer_preserve_comments_on_both_prefixes_and_encodings(&f).await;
        check_moderation_audit_preserves_edge_context(&f).await;
        check_disposed_defer_conflicts_without_stray_writes(&f).await;
        check_racing_accept_and_defer_have_a_serializable_result(&f).await;
        f.db.pool().close().await;
    })
    .await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(admin.pool())
        .await
        .unwrap();
    admin.pool().close().await;
    result.unwrap();
}

#[tokio::test]
async fn held_page_never_reads_message_bodies_outside_the_selected_window() {
    let f = fixture().await;
    seed_held(&f.db, &f.list_id, "one@example.invalid", "First").await;
    seed_held(&f.db, &f.list_id, "two@example.invalid", "Second").await;
    let pending = f.db.moderation().list_pending(&f.list_id).await.unwrap();
    let first = &pending[0];
    let corrupt = &pending[1];
    // SQLite-only fixture sabotage: invalid binary storage must stay isolated
    // to its own page. No live/development data or transport is involved.
    sqlx::query("UPDATE message_blobs SET raw=7 WHERE store_key=(SELECT store_key FROM messages WHERE id=$1)")
        .bind(corrupt.message_id.0.to_string()).execute(f.db.pool()).await.unwrap();
    assert!(f.db.mail_queue().message(corrupt.message_id).await.is_err());
    for prefix in ["/api/v1", "/3.1"] {
        let uri = format!("{prefix}/lists/{}/held?count=1&page=1", f.list_id);
        let response = get(&f.app, &uri, &f.admin_token).await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "off-page MIME must not be loaded"
        );
        let body = json_body(response).await;
        let entries = if prefix == "/3.1" {
            &body["entries"]
        } else {
            &body["items"]
        };
        assert_eq!(entries.as_array().unwrap().len(), 1);
        assert_eq!(entries[0]["request_id"], first.id.0.to_string());
        assert_eq!(
            body[if prefix == "/3.1" {
                "total_size"
            } else {
                "total"
            }],
            2
        );
        let count = get(
            &f.app,
            &format!("{prefix}/lists/{}/held/count", f.list_id),
            &f.admin_token,
        )
        .await;
        assert_eq!(json_body(count).await["count"], 2);
        let empty = get(
            &f.app,
            &format!("{prefix}/lists/{}/held?page=50&count=1", f.list_id),
            &f.admin_token,
        )
        .await;
        assert_eq!(empty.status(), StatusCode::OK);
        assert_eq!(json_body(empty).await["count"], 0);
    }
}

async fn json_body(response: Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn held_collection_get_and_count_expose_mailmanclient_wire_shape() {
    let f = fixture().await;
    let held_id = seed_held(
        &f.db,
        &f.list_id,
        "nonmember@example.invalid",
        "Held subject",
    )
    .await;

    let response = get(
        &f.app,
        &format!("/3.1/lists/{}/held", f.list_id),
        &f.admin_token,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let entries = body["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry["sender"], "nonmember@example.invalid");
    assert_eq!(entry["subject"], "Held subject");
    assert_eq!(entry["reason"], "nonmember");
    assert_eq!(entry["type"], "held_message");
    assert_eq!(entry["request_id"], held_id.0.to_string());
    assert!(entry["message_id"].as_str().unwrap().contains("seed-"));
    assert!(entry["msg"].as_str().unwrap().contains("Held subject"));
    assert!(!entry["hold_date"].as_str().unwrap().is_empty());
    assert_eq!(
        entry["self_link"],
        format!("/3.1/lists/{}/held/{}", f.list_id, held_id.0)
    );
    assert_eq!(body["total_size"], 1);

    let response = get(
        &f.app,
        &format!("/3.1/lists/{}/held/count", f.list_id),
        &f.admin_token,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json_body(response).await["count"], 1);

    let response = get(
        &f.app,
        &format!("/3.1/lists/{}/held/{}", f.list_id, held_id.0),
        &f.admin_token,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let single = json_body(response).await;
    assert_eq!(single["sender"], "nonmember@example.invalid");
}

#[tokio::test]
async fn cross_list_scoped_token_cannot_read_or_act_without_writes() {
    let f = fixture().await;
    let held_id = seed_held(&f.db, &f.list_id, "nonmember@example.invalid", "Subject").await;

    // A second list, and a token scoped only to it.
    let other_list: ListId = "other.dev.example.invalid".parse().unwrap();
    f.db.lists()
        .create(NewList {
            list_id: other_list.clone(),
            display_name: "Other".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let other_user =
        f.db.users()
            .create(NewUser {
                display_name: "Other owner".into(),
                email: "other-owner@example.invalid".into(),
                password: "another secure password".into(),
                server_owner: false,
            })
            .await
            .unwrap();
    let scoped_token =
        f.db.tokens()
            .create_scoped(
                other_user.id,
                "scoped",
                &["moderation"],
                Some(&other_list),
                None,
                None,
            )
            .await
            .unwrap()
            .token;

    let response = get(
        &f.app,
        &format!("/3.1/lists/{}/held", f.list_id),
        &scoped_token,
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = post_form(
        &f.app,
        &format!("/3.1/lists/{}/held/{}", f.list_id, held_id.0),
        &scoped_token,
        "action=accept",
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // No write occurred: still pending, no out job.
    let held = f.db.moderation().get(held_id).await.unwrap();
    assert!(held.disposition.is_none());
    let out_jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs WHERE queue='out'")
        .fetch_one(f.db.pool())
        .await
        .unwrap();
    assert_eq!(out_jobs, 0);
}

#[tokio::test]
async fn accept_delivers_to_resolved_members_and_replay_never_duplicates() {
    let f = fixture().await;
    let held_id = seed_held(&f.db, &f.list_id, "nonmember@example.invalid", "Subject").await;

    let response = post_form(
        &f.app,
        &format!("/3.1/lists/{}/held/{}", f.list_id, held_id.0),
        &f.admin_token,
        "action=accept",
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let held = f.db.moderation().get(held_id).await.unwrap();
    assert_eq!(
        held.disposition,
        Some(listmngr_db::moderation::Disposition::Accepted)
    );
    let out_jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs WHERE queue='out'")
        .fetch_one(f.db.pool())
        .await
        .unwrap();
    assert_eq!(out_jobs, 1);
    let job_id: String = sqlx::query_scalar("SELECT id FROM queue_jobs WHERE queue='out'")
        .fetch_one(f.db.pool())
        .await
        .unwrap();
    let recipients: Vec<String> =
        sqlx::query_scalar("SELECT email FROM delivery_recipients WHERE job_id=$1")
            .bind(&job_id)
            .fetch_all(f.db.pool())
            .await
            .unwrap();
    assert_eq!(recipients, vec!["member@example.invalid".to_owned()]);

    // Replay (racing/duplicate moderator click) must not create a second job.
    let response = post_form(
        &f.app,
        &format!("/3.1/lists/{}/held/{}", f.list_id, held_id.0),
        &f.admin_token,
        "action=accept",
    )
    .await;
    assert!(!response.status().is_success());
    let out_jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs WHERE queue='out'")
        .fetch_one(f.db.pool())
        .await
        .unwrap();
    assert_eq!(
        out_jobs, 1,
        "replayed accept must not duplicate the outgoing job"
    );
}

#[tokio::test]
async fn reject_notifies_author_while_discard_and_original_posts_have_no_delivery() {
    let f = fixture().await;
    let rejected = seed_held(&f.db, &f.list_id, "bad@example.invalid", "Spam").await;
    let discarded = seed_held(&f.db, &f.list_id, "spammer@example.invalid", "Junk").await;

    let response = post_form(
        &f.app,
        &format!("/3.1/lists/{}/held/{}", f.list_id, rejected.0),
        &f.admin_token,
        "action=reject&comment=sender+is+banned",
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        f.db.moderation().get(rejected).await.unwrap().disposition,
        Some(listmngr_db::moderation::Disposition::Rejected)
    );

    let response = post_form(
        &f.app,
        &format!("/3.1/lists/{}/held/{}", f.list_id, discarded.0),
        &f.admin_token,
        "action=discard",
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        f.db.moderation().get(discarded).await.unwrap().disposition,
        Some(listmngr_db::moderation::Disposition::Discarded)
    );

    let out_jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs WHERE queue='out'")
        .fetch_one(f.db.pool())
        .await
        .unwrap();
    assert_eq!(out_jobs, 1, "only the rejection notice may be published");
    let recipients: Vec<String> =
        sqlx::query_scalar("SELECT email FROM delivery_recipients ORDER BY email")
            .fetch_all(f.db.pool())
            .await
            .unwrap();
    assert_eq!(recipients, ["bad@example.invalid"]);
    let original_children: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs q JOIN held_messages h ON h.message_id=q.message_id WHERE q.queue!='in'")
        .fetch_one(f.db.pool()).await.unwrap();
    assert_eq!(original_children, 0, "neither original post may fan out");
}

#[tokio::test]
async fn defer_leaves_pending_and_unsupported_action_fails_explicitly() {
    let f = fixture().await;
    let held_id = seed_held(&f.db, &f.list_id, "nonmember@example.invalid", "Subject").await;

    let response = post_form(
        &f.app,
        &format!("/3.1/lists/{}/held/{}", f.list_id, held_id.0),
        &f.admin_token,
        "action=defer",
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(
        f.db.moderation()
            .get(held_id)
            .await
            .unwrap()
            .disposition
            .is_none()
    );

    let response = post_form(
        &f.app,
        &format!("/3.1/lists/{}/held/{}", f.list_id, held_id.0),
        &f.admin_token,
        "action=forward&comment=another_address@example.invalid",
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(
        f.db.moderation()
            .get(held_id)
            .await
            .unwrap()
            .disposition
            .is_none()
    );
}

#[tokio::test]
async fn unauthenticated_and_wrong_scope_requests_are_rejected_without_writes() {
    let f = fixture().await;
    let held_id = seed_held(&f.db, &f.list_id, "nonmember@example.invalid", "Subject").await;

    let mut request = Request::builder()
        .uri(format!("/3.1/lists/{}/held", f.list_id))
        .body(Body::empty())
        .unwrap();
    request.extensions_mut().insert(axum::extract::ConnectInfo(
        "127.0.0.1:4242".parse::<std::net::SocketAddr>().unwrap(),
    ));
    let response = f.app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let db = &f.db;
    let read_only_user = db
        .users()
        .create(NewUser {
            display_name: "Read only".into(),
            email: "read-only@example.invalid".into(),
            password: "yet another secure password".into(),
            server_owner: false,
        })
        .await
        .unwrap();
    let read_only_token = db
        .tokens()
        .create(read_only_user.id, "ro", &["lists:read"], None)
        .await
        .unwrap()
        .token;
    let response = post_form(
        &f.app,
        &format!("/3.1/lists/{}/held/{}", f.list_id, held_id.0),
        &read_only_token,
        "action=accept",
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(
        f.db.moderation()
            .get(held_id)
            .await
            .unwrap()
            .disposition
            .is_none()
    );
}

/// One moderation POST with an arbitrary body.
async fn post_body(f: &Fixture, uri: &str, body: String, json: bool) -> Response {
    let mut request = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {}", f.admin_token))
        .header(
            header::CONTENT_TYPE,
            if json {
                "application/json"
            } else {
                "application/x-www-form-urlencoded"
            },
        )
        .body(Body::from(body))
        .unwrap();
    request.extensions_mut().insert(axum::extract::ConnectInfo(
        "127.0.0.1:4242".parse::<std::net::SocketAddr>().unwrap(),
    ));
    f.app.clone().oneshot(request).await.unwrap()
}

/// The next outgoing job, if any.
async fn next_outgoing(db: &Database) -> Option<listmngr_db::mail_queue::Lease> {
    db.mail_queue()
        .claim(
            Queue::Out,
            "out",
            chrono::Utc::now().timestamp_millis() + 1_000,
            100,
        )
        .await
        .unwrap()
}

/// Mailman's `forward`: the decision also sends the held post, wrapped, to
/// the named address; `forward` without a usable `forward_to` is refused.
#[tokio::test]
async fn forward_sends_the_held_post_to_the_named_address_with_any_action() {
    let f = fixture().await;
    let id = seed_held(&f.db, &f.list_id, "sender@example.invalid", "Forward me").await;
    let uri = format!("/3.1/lists/{}/held/{}", f.list_id, id.0);
    // Postorius posts a form: forward=True with the address.
    for (body, json) in [
        ("action=defer&forward=True".to_owned(), false),
        (
            serde_json::json!({"action":"defer","forward":true,"forward_to":""}).to_string(),
            true,
        ),
        (
            serde_json::json!({"action":"defer","forward":true,"forward_to":"not a mailbox"})
                .to_string(),
            true,
        ),
        (
            format!(
                "action=defer&forward=True&forward_to={}",
                f.list_id.posting_address().replace('@', "%40")
            ),
            false,
        ),
    ] {
        assert_eq!(
            post_body(&f, &uri, body.clone(), json).await.status(),
            StatusCode::BAD_REQUEST,
            "{body}"
        );
    }
    assert!(
        next_outgoing(&f.db).await.is_none(),
        "a refused forward sends nothing"
    );
    assert_eq!(
        post_body(
            &f,
            &uri,
            "action=defer&forward=True&forward_to=Reviewer%40Example.NET".into(),
            false
        )
        .await
        .status(),
        StatusCode::NO_CONTENT
    );
    let lease = next_outgoing(&f.db).await.expect("the forward is queued");
    assert_eq!(
        f.db.mail_queue()
            .pending_recipients(lease.job.id)
            .await
            .unwrap(),
        ["reviewer@example.net"]
    );
    let raw =
        f.db.mail_queue()
            .message(lease.job.message_id)
            .await
            .unwrap()
            .raw;
    let text = String::from_utf8_lossy(&raw);
    assert!(
        text.contains("Subject: Forward of moderated message"),
        "{text}"
    );
    assert!(text.contains("Content-Type: message/rfc822"), "{text}");
    assert!(text.contains("Subject: Forward me"), "{text}");
    // Deferred, so still held; the log carries the address.
    let (disposition, forward_to): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT h.disposition, l.forward_to FROM held_messages h JOIN moderation_log l ON l.held_id=h.id WHERE h.id=$1",
    )
    .bind(id.0.to_string())
    .fetch_one(f.db.pool())
    .await
    .unwrap();
    assert_eq!(disposition, None);
    assert_eq!(forward_to.as_deref(), Some("reviewer@example.net"));
    // forward=false ignores forward_to and forwards nothing.
    let body =
        serde_json::json!({"action":"discard","forward":false,"forward_to":"other@example.net"})
            .to_string();
    assert_eq!(
        post_body(&f, &uri, body, true).await.status(),
        StatusCode::NO_CONTENT
    );
    assert!(next_outgoing(&f.db).await.is_none());
}
