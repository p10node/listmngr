use super::super::BROWSER_LOCK_BUSY;
use super::*;

#[tokio::test]
async fn password_change_sqlite_lock_matrix() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../target/password-{}.sqlite",
        uuid::Uuid::now_v7()
    ));
    let db = Database::connect(&format!("sqlite://{}?mode=rwc", path.display()), 3)
        .await
        .unwrap();
    db.migrate().await.unwrap();
    let mut connections = Vec::new();
    for _ in 0..3 {
        let mut c = db.pool().acquire().await.unwrap();
        sqlx::query("PRAGMA busy_timeout=1")
            .execute(&mut *c)
            .await
            .unwrap();
        connections.push(c);
    }
    drop(connections);
    matrix(&db, true).await;
    db.pool().close().await;
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
#[ignore = "requires NEW empty disposable WEBUI_PASSWORD_POSTGRES_URL"]
async fn password_change_postgres_lock_matrix() {
    let db = Database::connect(&std::env::var("WEBUI_PASSWORD_POSTGRES_URL").unwrap(), 3)
        .await
        .unwrap();
    db.migrate().await.unwrap();
    matrix(&db, false).await;
    db.pool().close().await;
}

async fn matrix(db: &Database, sqlite: bool) {
    for change in ["valid", "hash", "session", "expiry", "unverify", "audit"] {
        case(db, sqlite, change).await;
    }
}

async fn case(db: &Database, sqlite: bool, change: &str) {
    let email = format!("{change}@example.com");
    let user = db
        .users()
        .create(crate::NewUser {
            display_name: "Fixture".into(),
            email: email.clone(),
            password: "very secure password".into(),
            server_owner: false,
        })
        .await
        .unwrap();
    db.addresses().verify(&email, true).await.unwrap();
    let session = db
        .create_web_session(Some(user.id), None, chrono::Utc::now().timestamp_millis())
        .await
        .unwrap();
    let candidate = WebSession {
        token: session.token.clone(),
        csrf: session.csrf.clone(),
        user_id: session.user_id,
    };
    let mut blocker = if sqlite {
        db.pool().begin_with("BEGIN IMMEDIATE").await.unwrap()
    } else {
        db.pool().begin().await.unwrap()
    };
    sqlx::query("UPDATE user_credentials SET password_hash=password_hash WHERE user_id=$1")
        .bind(user.id.to_string())
        .execute(&mut *blocker)
        .await
        .unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let request = db.clone();
    let task = tokio::spawn(BROWSER_LOCK_BUSY.scope(sender, async move {
        request
            .browser_change_password(
                &candidate,
                "very secure password",
                "new strong password phrase 2026!",
            )
            .await
    }));
    tokio::time::timeout(std::time::Duration::from_secs(30), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(!task.is_finished(), "must observe actual DML contention");
    let sql = match change {
        "hash" => {
            "UPDATE user_credentials SET password_hash='changed-same-version' WHERE user_id=$1"
        }
        "session" => "DELETE FROM web_sessions WHERE user_id=$1",
        "expiry" => "UPDATE web_sessions SET expires_at=0 WHERE user_id=$1",
        "unverify" => "UPDATE addresses SET verified_on=NULL WHERE user_id=$1",
        _ => "UPDATE users SET display_name=display_name WHERE id=$1",
    };
    sqlx::query(sql)
        .bind(user.id.to_string())
        .execute(&mut *blocker)
        .await
        .unwrap();
    if change == "audit" {
        sqlx::query(if sqlite { "CREATE TRIGGER reject_password_audit BEFORE INSERT ON audit_log WHEN NEW.action='user.password' BEGIN SELECT RAISE(ABORT,'owned audit failure'); END" } else { "ALTER TABLE audit_log ADD CONSTRAINT reject_password_audit CHECK (action <> 'user.password') NOT VALID" }).execute(&mut *blocker).await.unwrap();
    }
    let expected: (String, String) = sqlx::query_as(
        "SELECT password_hash,password_updated_at FROM user_credentials WHERE user_id=$1",
    )
    .bind(user.id.to_string())
    .fetch_one(&mut *blocker)
    .await
    .unwrap();
    let expected_sessions: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM web_sessions WHERE user_id=$1")
            .bind(user.id.to_string())
            .fetch_one(&mut *blocker)
            .await
            .unwrap();
    blocker.commit().await.unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(30), task)
        .await
        .unwrap()
        .unwrap();
    check_effects(
        db,
        sqlite,
        change,
        user,
        session,
        (expected, expected_sessions),
        result,
    )
    .await;
}

async fn check_effects(
    db: &Database,
    sqlite: bool,
    change: &str,
    user: listmngr_core::User,
    session: WebSession,
    expected: ((String, String), i64),
    result: Result<()>,
) {
    let (expected, expected_sessions) = expected;
    if change == "valid" {
        result.unwrap();
        assert!(
            db.users()
                .verify_password(user.id, "new strong password phrase 2026!")
                .await
                .unwrap()
        );
    } else {
        assert!(result.is_err(), "{change}");
        let actual: (String, String) = sqlx::query_as(
            "SELECT password_hash,password_updated_at FROM user_credentials WHERE user_id=$1",
        )
        .bind(user.id.to_string())
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(actual, expected, "credential mutation escaped rollback");
    }
    let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM web_sessions WHERE user_id=$1")
        .bind(user.id.to_string())
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(
        sessions,
        if change == "valid" {
            0
        } else {
            expected_sessions
        }
    );
    let audits: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log WHERE action='user.password' AND target_id=$1",
    )
    .bind(user.id.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(audits, i64::from(change == "valid"));
    if change == "audit" {
        sqlx::query(if sqlite {
            "DROP TRIGGER reject_password_audit"
        } else {
            "ALTER TABLE audit_log DROP CONSTRAINT reject_password_audit"
        })
        .execute(db.pool())
        .await
        .unwrap();
        db.browser_change_password(
            &session,
            "very secure password",
            "new strong password phrase 2026!",
        )
        .await
        .unwrap();
    }
}
