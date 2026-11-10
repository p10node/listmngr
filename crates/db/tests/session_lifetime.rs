//! Audit F-17: `web.session_idle` and `web.session_absolute` bound a signed-in
//! browser session. Use extends the idle deadline, never past the absolute one.
use listmngr_db::{Database, NewUser};
use std::time::Duration;

async fn signed_in(idle_ms: i64, absolute_ms: i64) -> (Database, String) {
    let db = Database::connect("sqlite::memory:", 1)
        .await
        .unwrap()
        .with_session_limits(idle_ms, absolute_ms);
    db.migrate().await.unwrap();
    let user = db
        .users()
        .create(NewUser {
            display_name: "U".into(),
            email: "u@example.com".into(),
            password: "a very secure fixture password".into(),
            server_owner: false,
        })
        .await
        .unwrap();
    let session = db
        .create_web_session(Some(user.id), None, chrono::Utc::now().timestamp_millis())
        .await
        .unwrap();
    (db, session.token)
}

fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[tokio::test]
async fn an_unused_session_lapses_after_the_idle_window() {
    let (db, token) = signed_in(1_000, 600_000).await;
    assert!(db.web_session(&token, now()).await.is_ok());
    tokio::time::sleep(Duration::from_millis(1_400)).await;
    assert!(
        db.web_session(&token, now()).await.is_err(),
        "the configured idle window, not a fixed 8 h, ends the session"
    );
}

#[tokio::test]
async fn use_extends_the_idle_deadline_but_never_past_the_absolute_one() {
    let (db, token) = signed_in(2_000, 3_000).await;
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    assert!(db.web_session(&token, now()).await.is_ok());
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    assert!(
        db.web_session(&token, now()).await.is_ok(),
        "use at 1.2 s kept a 2 s idle session alive at 2.4 s"
    );
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    assert!(
        db.web_session(&token, now()).await.is_err(),
        "the absolute life (3 s) ends it although it was in use"
    );
}

#[tokio::test]
async fn the_stored_deadline_follows_the_configured_window() {
    let (db, _) = signed_in(60_000, 120_000).await;
    let span: i64 = sqlx::query_scalar("SELECT expires_at-created_at FROM web_sessions")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(span, 60_000);
}
