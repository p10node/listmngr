//! Shared owner-settings contention corpus, observed only after a real DB lock conflict.
use super::{BROWSER_LOCK_BUSY, Database, WebSession};
use listmngr_core::{MemberRole, SubscriptionMode};

#[tokio::test]
async fn sqlite_settings_authority_barrier() {
    let path =
        std::env::temp_dir().join(format!("listmngr-settings-{}.sqlite", uuid::Uuid::now_v7()));
    let db = Database::connect(&format!("sqlite://{}?mode=rwc", path.display()), 3)
        .await
        .unwrap();
    db.migrate().await.unwrap();
    let mut connections = Vec::new();
    for _ in 0..3 {
        let mut connection = db.pool().acquire().await.unwrap();
        sqlx::query("PRAGMA busy_timeout=1")
            .execute(&mut *connection)
            .await
            .unwrap();
        connections.push(connection);
    }
    drop(connections);
    matrix(&db, true).await;
    db.pool().close().await;
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
#[ignore = "requires NEW empty disposable WEBUI_SETTINGS_BARRIER_POSTGRES_URL"]
async fn postgres_settings_authority_barrier() {
    let db = Database::connect(
        &std::env::var("WEBUI_SETTINGS_BARRIER_POSTGRES_URL").unwrap(),
        3,
    )
    .await
    .unwrap();
    db.migrate().await.unwrap();
    matrix(&db, false).await;
    db.pool().close().await;
}

async fn matrix(db: &Database, sqlite: bool) {
    db.domains().create("example.com", "", None).await.unwrap();
    for (i, change) in [
        "valid",
        "server_valid",
        "session",
        "password",
        "expiry",
        "unverify",
        "unlink",
        "role",
        "moderator",
        "as_user",
        "server_owner",
        "audit",
    ]
    .into_iter()
    .enumerate()
    {
        scenario(db, sqlite, i, change).await;
    }
}

async fn seed(
    db: &Database,
    i: usize,
    server: bool,
) -> (listmngr_core::MailingList, listmngr_core::User, WebSession) {
    let list = db
        .lists()
        .create(crate::NewList {
            list_id: format!("settings-{i}.example.com").parse().unwrap(),
            display_name: "Before".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let email = format!("settings-{i}@example.com");
    let user = db
        .users()
        .create(crate::NewUser {
            display_name: "Owner".into(),
            email: email.clone(),
            password: "very secure fixture password".into(),
            server_owner: server,
        })
        .await
        .unwrap();
    db.addresses().verify(&email, true).await.unwrap();
    if !server {
        db.members()
            .create(crate::NewMember {
                list_id: list.id.clone(),
                email,
                role: MemberRole::Owner,
                subscription_mode: SubscriptionMode::AsUser,
                display_name: "Owner".into(),
            })
            .await
            .unwrap();
    }
    let session = db
        .create_web_session(Some(user.id), None, chrono::Utc::now().timestamp_millis())
        .await
        .unwrap();
    (list, user, session)
}

async fn scenario(db: &Database, sqlite: bool, i: usize, change: &str) {
    let (list, user, session) = seed(db, i, change.starts_with("server_")).await;
    db.browser_list_settings(&session, &list.id).await.unwrap();
    let mut blocker = if sqlite {
        db.pool().begin_with("BEGIN IMMEDIATE").await.unwrap()
    } else {
        db.pool().begin().await.unwrap()
    };
    // Actual ordinary user DML conflicts with the browser's authority reservation.
    sqlx::query("UPDATE users SET id=id WHERE id=$1")
        .bind(user.id.to_string())
        .execute(&mut *blocker)
        .await
        .unwrap();
    let request = db.clone();
    let id = list.id.clone();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(BROWSER_LOCK_BUSY.scope(sender, async move {
        request
            .browser_update_list_settings(
                &session,
                &id,
                &serde_json::json!({"display_name":"After"}),
            )
            .await
    }));
    tokio::time::timeout(std::time::Duration::from_secs(10), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(!task.is_finished());
    revoke(&mut blocker, user.id, change, sqlite).await;
    sqlx::query("UPDATE mailing_lists SET subject_prefix='[concurrent]' WHERE list_id=$1")
        .bind(list.id.as_str())
        .execute(&mut *blocker)
        .await
        .unwrap();
    blocker.commit().await.unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap();
    let valid = matches!(change, "valid" | "server_valid");
    assert_eq!(result.is_ok(), valid, "{change}: {result:?}");
    let saved = db.lists().get(&list.id).await.unwrap();
    assert_eq!(saved.display_name, if valid { "After" } else { "Before" });
    assert_eq!(saved.subject_prefix, "[concurrent]");
    let audits: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='list.config' AND target_id=$1 AND actor_user_id=$2").bind(list.id.as_str()).bind(user.id.to_string()).fetch_one(db.pool()).await.unwrap();
    assert_eq!(audits, i64::from(valid));
    if change == "audit" {
        sqlx::query(if sqlite {
            "DROP TRIGGER reject_settings_barrier_audit"
        } else {
            "ALTER TABLE audit_log DROP CONSTRAINT reject_settings_barrier_audit"
        })
        .execute(db.pool())
        .await
        .unwrap();
    }
    println!(
        "SETTINGS BARRIER PASS sqlite={sqlite} change={change}: live authority, fresh unrelated field, atomic audit"
    );
}

async fn revoke(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    user: listmngr_core::UserId,
    change: &str,
    sqlite: bool,
) {
    let sql = match change {
        "valid" | "server_valid" => return,
        "session" => "DELETE FROM web_sessions WHERE user_id=$1",
        "password" => "UPDATE user_credentials SET password_updated_at='revoked' WHERE user_id=$1",
        "expiry" => "UPDATE web_sessions SET expires_at=0 WHERE user_id=$1",
        "unverify" => "UPDATE addresses SET verified_on=NULL WHERE user_id=$1",
        "unlink" => "UPDATE addresses SET user_id=NULL WHERE user_id=$1",
        "role" => "DELETE FROM members WHERE user_id=$1",
        "moderator" => "UPDATE members SET role='moderator' WHERE user_id=$1",
        "as_user" => "UPDATE members SET user_id=NULL WHERE user_id=$1",
        "server_owner" => "UPDATE users SET is_server_owner=0 WHERE id=$1",
        "audit" => {
            sqlx::query(if sqlite { "CREATE TRIGGER reject_settings_barrier_audit BEFORE INSERT ON audit_log WHEN NEW.action='list.config' BEGIN SELECT RAISE(ABORT,'owned failure'); END" } else { "ALTER TABLE audit_log ADD CONSTRAINT reject_settings_barrier_audit CHECK (action <> 'list.config') NOT VALID" }).execute(&mut **tx).await.unwrap();
            return;
        }
        _ => panic!("unknown case"),
    };
    sqlx::query(sql)
        .bind(user.to_string())
        .execute(&mut **tx)
        .await
        .unwrap();
}
