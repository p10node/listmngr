//! Issuance-boundary tests: real password proof followed by ordinary DML under
//! an independently held writer reservation. Busy notification is observation only.
use super::*;

#[tokio::test]
async fn browser_login_issuance_sqlite_lock_matrix() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../target/login-{}.sqlite",
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
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn browser_login_issuance_postgres_lock_matrix() {
    let schema = crate::test_support::IsolatedSchema::create("web_login")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 3).await.unwrap();
    db.migrate().await.unwrap();
    matrix(&db, false).await;
    db.pool().close().await;
    schema.drop().await.unwrap();
}

async fn matrix(db: &Database, sqlite: bool) {
    for change in [
        "valid", "hash", "version", "unverify", "unlink", "reassign", "expiry", "rotation", "csrf",
        "audit",
    ] {
        case(db, sqlite, change).await;
    }
}

// Keep the same ordered barrier/mutation/effect matrix visible for both backends.
#[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
async fn case(db: &Database, sqlite: bool, change: &str) {
    let email = format!("{change}@example.com");
    let user = db
        .users()
        .create(crate::NewUser {
            display_name: "Login".into(),
            email: email.clone(),
            password: "very secure password".into(),
            server_owner: false,
        })
        .await
        .unwrap();
    db.addresses().verify(&email, true).await.unwrap();
    let other = if change == "reassign" {
        Some(
            db.users()
                .create(crate::NewUser {
                    display_name: "Other".into(),
                    email: "other@example.com".into(),
                    password: "other secure password".into(),
                    server_owner: false,
                })
                .await
                .unwrap(),
        )
    } else {
        None
    };
    let previous = db
        .create_web_session(None, None, chrono::Utc::now().timestamp_millis())
        .await
        .unwrap();
    let previous_token = previous.token.clone();
    let proof = db
        .verify_browser_login(&email, "very secure password")
        .await
        .unwrap();
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='web.login'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let sessions_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM web_sessions")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let mut blocker = if sqlite {
        db.pool().begin_with("BEGIN IMMEDIATE").await.unwrap()
    } else {
        db.pool().begin().await.unwrap()
    };
    // An ordinary UPDATE obtains PostgreSQL's RowExclusive table lock. It must
    // conflict with issuance, even though it did not use browser_write_tx.
    sqlx::query("UPDATE user_credentials SET password_hash=password_hash WHERE user_id=$1")
        .bind(user.id.to_string())
        .execute(&mut *blocker)
        .await
        .unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let task_db = db.clone();
    let task = tokio::spawn(BROWSER_LOCK_BUSY.scope(sender, async move {
        task_db.issue_browser_login(proof, &previous).await
    }));
    tokio::time::timeout(std::time::Duration::from_secs(10), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !task.is_finished(),
        "issuance must encounter the actual DML lock"
    );
    let sql = match change {
        "valid" => "UPDATE users SET display_name='Still valid' WHERE id=$1",
        "hash" => {
            "UPDATE user_credentials SET password_hash='changed-with-same-version' WHERE user_id=$1"
        }
        "version" => "UPDATE user_credentials SET password_updated_at='changed' WHERE user_id=$1",
        "unverify" => "UPDATE addresses SET verified_on=NULL WHERE user_id=$1",
        "unlink" => "UPDATE addresses SET user_id=NULL WHERE user_id=$1",
        "reassign" => "UPDATE addresses SET user_id=$2 WHERE user_id=$1",
        "expiry" => "UPDATE web_sessions SET expires_at=0 WHERE token_hash=$1",
        "rotation" => "DELETE FROM web_sessions WHERE token_hash=$1",
        "csrf" => "UPDATE web_sessions SET csrf='rotated' WHERE token_hash=$1",
        "audit" => "UPDATE users SET display_name=display_name WHERE id=$1",
        _ => unreachable!(),
    };
    let mut query = sqlx::query(sql).bind(if matches!(change, "expiry" | "rotation" | "csrf") {
        digest(&previous_token)
    } else {
        user.id.to_string()
    });
    if let Some(other) = other {
        query = query.bind(other.id.to_string());
    }
    query.execute(&mut *blocker).await.unwrap();
    if change == "audit" {
        if sqlite {
            sqlx::query("CREATE TRIGGER reject_login_audit BEFORE INSERT ON audit_log WHEN NEW.action='web.login' BEGIN SELECT RAISE(ABORT,'owned login audit failure'); END").execute(&mut *blocker).await.unwrap();
        } else {
            sqlx::query("ALTER TABLE audit_log ADD CONSTRAINT reject_login_audit CHECK (action <> 'web.login') NOT VALID").execute(&mut *blocker).await.unwrap();
        }
    }
    blocker.commit().await.unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap();
    if change == "valid" {
        let fresh = result.unwrap();
        assert_eq!(fresh.user_id, Some(user.id));
        assert_ne!(fresh.token, previous_token);
        assert!(
            db.web_session(&fresh.token, chrono::Utc::now().timestamp_millis())
                .await
                .is_ok()
        );
        assert!(
            db.web_session(&previous_token, chrono::Utc::now().timestamp_millis())
                .await
                .is_err()
        );
    } else if change == "audit" {
        assert!(matches!(result, Err(Error::Database(_))));
    } else {
        assert!(
            matches!(result, Err(Error::Authentication)),
            "case {change}"
        );
    }
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='web.login'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(after - before, i64::from(change == "valid"));
    let sessions_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM web_sessions")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(
        sessions_after,
        sessions_before - i64::from(change == "rotation")
    );
    let old_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM web_sessions WHERE token_hash=$1")
        .bind(digest(&previous_token))
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(old_rows, i64::from(!matches!(change, "valid" | "rotation")));
    println!("LOGIN LOCK PASS sqlite={sqlite} case={change}: authority/session/audit verified");
}
