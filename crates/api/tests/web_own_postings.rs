#[path = "web_own_postings/controls.rs"]
mod controls;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use listmngr_core::{Config, MemberRole, SubscriptionMode};
use listmngr_db::{Database, NewList, NewMember, NewUser};
use tower::ServiceExt;

async fn fixture() -> (
    Database,
    listmngr_core::Member,
    listmngr_db::web_sessions::WebSession,
) {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    seed(db).await
}

async fn seed(
    db: Database,
) -> (
    Database,
    listmngr_core::Member,
    listmngr_db::web_sessions::WebSession,
) {
    db.domains()
        .create("recover.invalid", "", None)
        .await
        .unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: "list.recover.invalid".parse().unwrap(),
            display_name: "Recovery".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    db.users()
        .create(NewUser {
            email: "own@recover.invalid".into(),
            display_name: "Own".into(),
            password: "a very secure fixture password".into(),
            server_owner: false,
        })
        .await
        .unwrap();
    db.addresses()
        .verify("own@recover.invalid", true)
        .await
        .unwrap();
    let m = db
        .members()
        .create(NewMember {
            list_id: list.id,
            email: "own@recover.invalid".into(),
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsUser,
            display_name: "Own".into(),
        })
        .await
        .unwrap();
    let anon = db
        .create_web_session(None, None, chrono::Utc::now().timestamp_millis())
        .await
        .unwrap();
    let session = db
        .browser_login(
            "own@recover.invalid",
            "a very secure fixture password",
            &anon,
        )
        .await
        .unwrap();
    (db, m, session)
}

#[tokio::test]
async fn own_postings_post_persists_false() {
    let (db, m, session) = fixture().await;
    let mut config = Config::default();
    config.site.base_url = "http://localhost".into();
    let app = listmngr_api::router(db.clone(), config);
    let response = call(
        &app,
        "POST",
        &format!("/web/members/{}/preferences", m.id),
        &format!("listmngr_session={}", session.token),
        &format!(
            "csrf={}&delivery_mode=regular&delivery_status=enabled&receive_own_postings=false",
            session.csrf
        ),
        "http://localhost",
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let own: i64 = sqlx::query_scalar("SELECT receive_own_postings FROM preferences WHERE id=$1")
        .bind(m.preferences_id.0.to_string())
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(own, 0);
}

#[tokio::test]
async fn own_postings_account_selects_effective_value() {
    let (db, m, session) = fixture().await;
    let mut config = Config::default();
    config.site.base_url = "http://localhost".into();
    let app = listmngr_api::router(db.clone(), config);
    for value in [0_i32, 1] {
        sqlx::query("UPDATE preferences SET receive_own_postings=NULL WHERE id=$1")
            .bind(m.preferences_id.0.to_string())
            .execute(db.pool())
            .await
            .unwrap();
        let inherited = uuid::Uuid::now_v7().to_string();
        sqlx::query("INSERT INTO preferences(id,receive_own_postings) VALUES($1,$2)")
            .bind(&inherited)
            .bind(value)
            .execute(db.pool())
            .await
            .unwrap();
        sqlx::query("UPDATE addresses SET preferences_id=$1 WHERE id=$2")
            .bind(&inherited)
            .bind(m.address_id.0.to_string())
            .execute(db.pool())
            .await
            .unwrap();
        let response = call(
            &app,
            "GET",
            "/web/account",
            &format!("listmngr_session={}", session.token),
            "",
            "http://localhost",
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let html = String::from_utf8(
            to_bytes(response.into_body(), 100_000)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(html.contains("Receive your own posts"));
        let select = html
            .split("name=\"receive_own_postings\"")
            .nth(1)
            .expect("own postings select")
            .split("</select>")
            .next()
            .unwrap();
        assert!(select.contains(&format!("value=\"{}\" selected", value == 1)));
    }
}

async fn call(
    app: &axum::Router,
    method: &str,
    url: &str,
    cookie: &str,
    body: &str,
    origin: &str,
) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(url)
                .header("cookie", cookie)
                .header("origin", origin)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(body.to_owned()))
                .unwrap(),
        )
        .await
        .unwrap()
}
