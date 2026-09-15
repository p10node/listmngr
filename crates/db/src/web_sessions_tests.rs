//! Real `SQLite` contention tests. Observation is after `SQLITE_BUSY`, never a sleep
//! pretending the request reached the write boundary.
use super::*;
use crate::mail_queue::{NewMessage, Queue};
use crate::moderation::{HeldId, ReviewAction};
use listmngr_core::{DeliveryMode, DeliveryStatus, MemberRole, SubscriptionMode};

#[path = "web_recovery_lock_tests.rs"]
mod recovery;

#[tokio::test]
async fn browser_login_issuance_rejects_changed_password() {
    let (db, path, u, _, _, _) = fixture(false, false).await;
    let old = db
        .create_web_session(None, None, chrono::Utc::now().timestamp_millis())
        .await
        .unwrap();
    let proof = db
        .verify_browser_login("user@example.com", "very secure password")
        .await
        .unwrap();
    db.users()
        .set_password(u.id, "a different secure password")
        .await
        .unwrap();
    let result = db.issue_browser_login(proof, &old).await;
    assert!(
        matches!(result, Err(Error::Authentication)),
        "a password verified before reset must not mint a session"
    );
    assert!(
        db.web_session(&old.token, chrono::Utc::now().timestamp_millis())
            .await
            .is_ok()
    );
    db.pool().close().await;
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn browser_login_issuance_valid_rotates() {
    let (db, path, u, _, _, _) = fixture(false, false).await;
    let old = db
        .create_web_session(None, None, chrono::Utc::now().timestamp_millis())
        .await
        .unwrap();
    let crate::web_sessions::LoginOutcome::Complete(fresh) = db
        .browser_login("user@example.com", "very secure password", &old)
        .await
        .unwrap()
    else {
        panic!("no second factor is enrolled")
    };
    assert_eq!(fresh.user_id, Some(u.id));
    assert_ne!(fresh.token, old.token);
    assert!(
        db.web_session(&fresh.token, chrono::Utc::now().timestamp_millis())
            .await
            .is_ok()
    );
    assert!(
        db.web_session(&old.token, chrono::Utc::now().timestamp_millis())
            .await
            .is_err()
    );
    db.pool().close().await;
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn member_admin_rechecks_authority_after_real_writer_contention() {
    for change in [
        "valid",
        "session",
        "password",
        "unverify",
        "unlink",
        "server_owner",
        "audit",
    ] {
        let (db, path, user, member, _, session) = fixture(false, true).await;
        assert_eq!(
            db.browser_admin_members(&session, &member.list_id, 0)
                .await
                .unwrap()
                .len(),
            1
        );
        let mut blocker = db.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let request = db.clone();
        let id = member.id;
        let list = member.list_id.clone();
        let task = tokio::spawn(BROWSER_LOCK_BUSY.scope(sender, async move {
            request
                .browser_member_policy(
                    &session,
                    &list,
                    id,
                    Some(listmngr_core::ModerationAction::Hold),
                )
                .await
        }));
        tokio::time::timeout(std::time::Duration::from_secs(10), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(!task.is_finished());
        revoke(&mut blocker, change, &user, &member).await;
        blocker.commit().await.unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(10), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.is_ok(), change == "valid");
        assert_eq!(
            db.members().get(member.id).await.unwrap().moderation_action,
            if change == "valid" {
                Some(listmngr_core::ModerationAction::Hold)
            } else {
                None
            }
        );
        let audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE action='member.update' AND target_id=$1",
        )
        .bind(member.id.to_string())
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(audits, i64::from(change == "valid"));
        db.pool().close().await;
        std::fs::remove_file(path).unwrap();
    }
}

#[tokio::test]
async fn member_departure_rechecks_authority_after_real_writer_contention() {
    for change in [
        "valid", "session", "password", "unverify", "unlink", "as_user",
    ] {
        let (db, path, user, member, _, session) = fixture(false, false).await;
        assert_eq!(
            db.browser_leave_preview(&session, member.id)
                .await
                .unwrap()
                .0,
            member.list_id
        );
        let mut blocker = db.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let request = db.clone();
        let member_id = member.id;
        let task = tokio::spawn(BROWSER_LOCK_BUSY.scope(sender, async move {
            request.browser_leave(&session, member_id).await
        }));
        tokio::time::timeout(std::time::Duration::from_secs(10), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(!task.is_finished());
        revoke(&mut blocker, change, &user, &member).await;
        blocker.commit().await.unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(10), task)
            .await
            .unwrap()
            .unwrap();
        if change == "valid" {
            assert_eq!(result.unwrap(), member.list_id);
        } else {
            assert!(matches!(
                result,
                Err(Error::Authentication | Error::Forbidden(_))
            ));
        }
        assert_eq!(db.members().get(member.id).await.is_ok(), change != "valid");
        assert_eq!(
            db.preferences().get(member.preferences_id).await.is_ok(),
            change != "valid"
        );
        assert!(db.users().get(user.id).await.is_ok());
        let audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE action='member.delete' AND target_id=$1",
        )
        .bind(member.id.to_string())
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(audits, i64::from(change == "valid"));
        db.pool().close().await;
        std::fs::remove_file(path).unwrap();
    }
}

#[tokio::test]
async fn private_archive_read_rechecks_authority_after_real_writer_contention() {
    for change in [
        "valid", "session", "password", "unverify", "unlink", "role", "expiry", "as_user",
    ] {
        let (db, path, user, member, _, session) = fixture(false, false).await;
        db.lists()
            .update(
                &"public.example.com".parse().unwrap(),
                &serde_json::json!({"archive_policy":"private"}),
            )
            .await
            .unwrap();
        let raw = b"From: sender@example.com\r\nSubject: Archive\r\n\r\nlocked archive text";
        sqlx::query("INSERT INTO archive_messages(list_id,hash,thread,subject,body,raw_b64,created_at) VALUES('public.example.com','private-message','private-thread','Archive','locked archive text',$1,1)")
            .bind(base64::Engine::encode(&base64::engine::general_purpose::STANDARD, raw))
            .execute(db.pool()).await.unwrap();
        let mut blocker = db.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let request = db.clone();
        let task = tokio::spawn(BROWSER_LOCK_BUSY.scope(sender, async move {
            request
                .archive()
                .read_browser_message(
                    &"public.example.com".parse().unwrap(),
                    Some(&session),
                    "private-message",
                )
                .await
        }));
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(10), receiver.recv())
                .await
                .unwrap()
                .is_some()
        );
        if change == "expiry" {
            sqlx::query("UPDATE web_sessions SET expires_at=0 WHERE user_id=$1")
                .bind(user.id.to_string())
                .execute(&mut *blocker)
                .await
                .unwrap();
        } else {
            revoke(&mut blocker, change, &user, &member).await;
        }
        blocker.commit().await.unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(10), task)
            .await
            .unwrap()
            .unwrap();
        if change == "valid" {
            assert!(result.unwrap().body.contains("locked archive text"));
        } else {
            assert!(
                matches!(result, Err(Error::Authentication | Error::Forbidden(_))),
                "{change}: {result:?}"
            );
        }
        db.pool().close().await;
        std::fs::remove_file(path).unwrap();
    }
}

/// Hold notices are covered by their own tests; these fixtures count posts.
async fn quiet_hold_notices(db: &Database, list: &listmngr_core::ListId) {
    db.lists()
        .update(
            list,
            &serde_json::json!({"respond_to_post_requests": false, "admin_immed_notify": false}),
        )
        .await
        .unwrap();
}

async fn fixture(
    review: bool,
    owner: bool,
) -> (
    Database,
    std::path::PathBuf,
    listmngr_core::User,
    listmngr_core::Member,
    HeldId,
    WebSession,
) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../target/webui-authority-{}.sqlite",
        uuid::Uuid::now_v7()
    ));
    let db = Database::connect(&format!("sqlite://{}?mode=rwc", path.display()), 3)
        .await
        .unwrap();
    db.migrate().await.unwrap();
    db.domains().create("example.com", "", None).await.unwrap();
    let list: listmngr_core::ListId = "public.example.com".parse().unwrap();
    db.lists()
        .create(crate::NewList {
            list_id: list.clone(),
            display_name: "Public".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    quiet_hold_notices(&db, &list).await;
    let u = db
        .users()
        .create(crate::NewUser {
            display_name: "User".into(),
            email: "user@example.com".into(),
            password: "very secure password".into(),
            server_owner: owner,
        })
        .await
        .unwrap();
    db.addresses()
        .verify("user@example.com", true)
        .await
        .unwrap();
    let m = db
        .members()
        .create(crate::NewMember {
            list_id: "public.example.com".parse().unwrap(),
            email: "user@example.com".into(),
            role: if review && !owner {
                MemberRole::Moderator
            } else {
                MemberRole::Member
            },
            subscription_mode: SubscriptionMode::AsUser,
            display_name: "User".into(),
        })
        .await
        .unwrap();
    let session = db
        .create_web_session(Some(u.id), None, chrono::Utc::now().timestamp_millis())
        .await
        .unwrap();
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: b"From: sender@example.com\r\n\r\nbody".to_vec(),
                external_id: uuid::Uuid::now_v7().to_string(),
                context: "{}".into(),
                queue: Queue::In,
                max_attempts: 5,
            },
            1000,
        )
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::In, "test", 1000, 10000)
        .await
        .unwrap()
        .unwrap();
    let h = db
        .moderation()
        .hold(
            &lease,
            &"public.example.com".parse().unwrap(),
            "sender@example.com",
            "subject",
            "held",
            1000,
        )
        .await
        .unwrap()
        .id;
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
    (db, path, u, m, h, session)
}

async fn revoke(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    change: &str,
    u: &listmngr_core::User,
    m: &listmngr_core::Member,
) {
    let query = match change {
        "valid" | "expiry" => return,
        "session" => "DELETE FROM web_sessions WHERE user_id=$1",
        "password" => "UPDATE user_credentials SET password_updated_at='revoked' WHERE user_id=$1",
        "unverify" => "UPDATE addresses SET verified_on=NULL WHERE user_id=$1",
        "unlink" => "UPDATE addresses SET user_id=NULL WHERE user_id=$1",
        "as_user" => "UPDATE members SET user_id=NULL WHERE user_id=$1",
        "role" => "DELETE FROM members WHERE user_id=$1",
        "server_owner" => "UPDATE users SET is_server_owner=0 WHERE id=$1",
        "policy_member" => {
            "UPDATE preferences SET delivery_status='by_moderator' WHERE id=(SELECT preferences_id FROM members WHERE user_id=$1)"
        }
        "policy_user" => {
            "UPDATE preferences SET delivery_status='by_bounces' WHERE id=(SELECT preferences_id FROM users WHERE id=$1)"
        }
        "policy_address" => {
            // A separate nullable address layer rather than aliasing the member.
            sqlx::query("INSERT INTO preferences(id,delivery_status) VALUES($1,'unknown')")
                .bind(uuid::Uuid::now_v7().to_string())
                .execute(&mut **tx)
                .await
                .unwrap();
            "UPDATE addresses SET preferences_id=(SELECT id FROM preferences WHERE delivery_status='unknown') WHERE user_id=$1"
        }
        "audit" => {
            sqlx::query("CREATE TRIGGER reject_browser_audit BEFORE INSERT ON audit_log BEGIN SELECT RAISE(ABORT,'owned test audit failure'); END").execute(&mut **tx).await.unwrap();
            return;
        }
        _ => panic!("unknown case {}", m.id),
    };
    sqlx::query(query)
        .bind(u.id.to_string())
        .execute(&mut **tx)
        .await
        .unwrap();
}

#[tokio::test]
async fn sqlite_inflight_authority_write_lock_barrier() {
    for review in [false, true] {
        for change in [
            "valid",
            "session",
            "password",
            "unverify",
            "unlink",
            "as_user",
            "role",
            "expiry",
            "server_owner",
            "policy_member",
            "policy_address",
            "policy_user",
            "audit",
        ] {
            if (review && change.starts_with("policy")) || (!review && change == "server_owner") {
                continue;
            }
            sqlite_case(review, change).await;
        }
    }
}

async fn sqlite_case(review: bool, change: &str) {
    let (db, path, u, m, h, session) = fixture(review, change == "server_owner").await;
    let expiry = chrono::Utc::now().timestamp_millis() + 1000;
    if change == "expiry" {
        sqlx::query("UPDATE web_sessions SET expires_at=$1 WHERE user_id=$2")
            .bind(expiry)
            .bind(u.id.to_string())
            .execute(db.pool())
            .await
            .unwrap();
    }
    let mut blocker = db.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let task_db = db.clone();
    let task = tokio::spawn(BROWSER_LOCK_BUSY.scope(sender, async move {
        if review {
            task_db
                .browser_review(
                    &session,
                    &"public.example.com".parse().unwrap(),
                    h,
                    &ReviewAction::Accept { max_attempts: 5 },
                    "review",
                )
                .await
        } else {
            task_db
                .browser_preferences(
                    &session,
                    m.id,
                    DeliveryMode::MimeDigests,
                    DeliveryStatus::ByUser,
                )
                .await
        }
    }));
    tokio::time::timeout(std::time::Duration::from_secs(10), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !task.is_finished(),
        "must retry after actual SQLite write-lock contention"
    );
    revoke(&mut blocker, change, &u, &m).await;
    if change == "expiry" {
        while chrono::Utc::now().timestamp_millis() <= expiry {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
    blocker.commit().await.unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap();
    verify_effects(&db, &m, h, review, change, result).await;
    println!(
        "SQLITE BUSY BARRIER PASS review={review} change={change}: authority, business, Out, log, audit verified"
    );
    db.pool().close().await;
    std::fs::remove_file(path).unwrap();
}

async fn verify_effects(
    db: &Database,
    m: &listmngr_core::Member,
    h: HeldId,
    review: bool,
    change: &str,
    result: Result<()>,
) {
    match change {
        "valid" => result.unwrap(),
        "session" | "password" | "expiry" => {
            assert!(matches!(result, Err(Error::Authentication)), "{result:?}");
        }
        "audit" => assert!(matches!(result, Err(Error::Database(_))), "{result:?}"),
        _ => assert!(matches!(result, Err(Error::Forbidden(_))), "{result:?}"),
    }
    let events:i64=sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='preferences.update' OR action='moderation.accept'").fetch_one(db.pool()).await.unwrap();
    assert_eq!(events, i64::from(change == "valid"));
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
    assert_eq!(item.disposition.is_some(), review && change == "valid");
    assert_eq!(children, i64::from(review && change == "valid"));
    assert_eq!(logs, children);
    assert_eq!(
        db.preferences()
            .get(m.preferences_id)
            .await
            .unwrap()
            .delivery_mode,
        if !review && change == "valid" {
            Some(DeliveryMode::MimeDigests)
        } else {
            None
        }
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keep each fixture and its causal assertions together.
async fn held_recipient_snapshot_lock_barrier() {
    for (browser, change) in [
        (true, "unsubscribe"),
        (true, "disable"),
        (false, "unsubscribe"),
        (false, "disable"),
    ] {
        let (db, path, _, _, held, session) = fixture(true, false).await;
        let list: listmngr_core::ListId = "public.example.com".parse().unwrap();
        let mut members = Vec::new();
        for email in ["stale@example.com", "Keep@Example.com"] {
            members.push(
                db.members()
                    .create(crate::NewMember {
                        list_id: list.clone(),
                        email: email.into(),
                        role: MemberRole::Member,
                        subscription_mode: SubscriptionMode::AsAddress,
                        display_name: email.into(),
                    })
                    .await
                    .unwrap(),
            );
        }
        // Acceptance intent must not carry a pre-transaction candidate snapshot.
        let action = ReviewAction::Accept { max_attempts: 5 };
        let mut blocker = db.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let task_db = db.clone();
        let task = tokio::spawn(BROWSER_LOCK_BUSY.scope(sender, async move {
            if browser {
                task_db
                    .browser_review(&session, &list, held, &action, "snapshot test")
                    .await
            } else {
                task_db
                    .moderation()
                    .review(
                        held,
                        &AuditContext::system(),
                        &action,
                        "snapshot test",
                        2000,
                    )
                    .await
            }
        }));
        tokio::time::timeout(std::time::Duration::from_secs(10), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(!task.is_finished());
        if change == "disable" {
            sqlx::query("UPDATE preferences SET delivery_status='by_user' WHERE id=$1")
                .bind(members[0].preferences_id.to_string())
                .execute(&mut *blocker)
                .await
                .unwrap();
        } else {
            sqlx::query("DELETE FROM members WHERE id=$1")
                .bind(members[0].id.to_string())
                .execute(&mut *blocker)
                .await
                .unwrap();
        }
        blocker.commit().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let emails: Vec<String> =
            sqlx::query_scalar("SELECT email FROM delivery_recipients ORDER BY email")
                .fetch_all(db.pool())
                .await
                .unwrap();
        assert_eq!(
            emails,
            vec!["Keep@Example.com"],
            "committed {change} must remove stale recipients, not the live survivor"
        );
        let logs: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM moderation_log WHERE action='accept'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        let audits: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='moderation.accept'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!((logs, audits), (1, 1));
        assert!(
            db.moderation()
                .get(held)
                .await
                .unwrap()
                .disposition
                .is_some()
        );
        println!(
            "HELD SNAPSHOT BARRIER PASS browser={browser} change={change}: committed writer, exact survivor, disposition/log/audit"
        );
        db.pool().close().await;
        std::fs::remove_file(path).unwrap();
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keep each fixture and its causal assertions together.
async fn held_recipient_effective_policy_and_atomicity() {
    for case in [
        "defaults",
        "user_disabled",
        "address_override",
        "member_override",
        "digest",
        "own",
        "audit",
    ] {
        let (db, path, user, _, held, session) = fixture(true, false).await;
        let list = "public.example.com".parse().unwrap();
        let member = db
            .members()
            .create(crate::NewMember {
                list_id: list,
                email: "user@example.com".into(),
                role: MemberRole::Member,
                subscription_mode: SubscriptionMode::AsUser,
                display_name: "recipient".into(),
            })
            .await
            .unwrap();
        sqlx::query("UPDATE addresses SET original_email='User@Example.com' WHERE id=$1")
            .bind(member.address_id.to_string())
            .execute(db.pool())
            .await
            .unwrap();
        if matches!(
            case,
            "user_disabled" | "address_override" | "member_override"
        ) {
            sqlx::query("UPDATE preferences SET delivery_status='by_user' WHERE id=(SELECT preferences_id FROM users WHERE id=$1)")
                .bind(user.id.to_string()).execute(db.pool()).await.unwrap();
        }
        if matches!(case, "address_override" | "member_override") {
            let id = uuid::Uuid::now_v7().to_string();
            sqlx::query("INSERT INTO preferences(id,delivery_status) VALUES($1,'enabled')")
                .bind(&id)
                .execute(db.pool())
                .await
                .unwrap();
            sqlx::query("UPDATE addresses SET preferences_id=$1 WHERE id=$2")
                .bind(id)
                .bind(member.address_id.to_string())
                .execute(db.pool())
                .await
                .unwrap();
        }
        if case == "member_override" {
            sqlx::query("UPDATE preferences SET delivery_status='by_user' WHERE id=$1")
                .bind(member.preferences_id.to_string())
                .execute(db.pool())
                .await
                .unwrap();
        }
        if case == "digest" {
            sqlx::query("UPDATE preferences SET delivery_mode='mime_digests' WHERE id=$1")
                .bind(member.preferences_id.to_string())
                .execute(db.pool())
                .await
                .unwrap();
        }
        if case == "own" {
            sqlx::query("UPDATE held_messages SET sender='user@example.com' WHERE id=$1")
                .bind(held.0.to_string())
                .execute(db.pool())
                .await
                .unwrap();
            sqlx::query("UPDATE preferences SET receive_own_postings=0 WHERE id=$1")
                .bind(member.preferences_id.to_string())
                .execute(db.pool())
                .await
                .unwrap();
        }
        if case == "audit" {
            sqlx::query("CREATE TRIGGER fail_held_audit BEFORE INSERT ON audit_log WHEN NEW.action='moderation.accept' BEGIN SELECT RAISE(ABORT,'owned held audit failure'); END").execute(db.pool()).await.unwrap();
        }
        let result = db
            .browser_review(
                &session,
                &"public.example.com".parse().unwrap(),
                held,
                &ReviewAction::Accept { max_attempts: 5 },
                "policy",
            )
            .await;
        if case == "audit" {
            assert!(matches!(result, Err(Error::Database(_))), "{result:?}");
        } else {
            result.unwrap();
        }
        let emails: Vec<String> =
            sqlx::query_scalar("SELECT email FROM delivery_recipients ORDER BY email")
                .fetch_all(db.pool())
                .await
                .unwrap();
        let expected = if matches!(case, "defaults" | "address_override") {
            vec!["User@Example.com"]
        } else {
            vec![]
        };
        assert_eq!(emails, expected, "policy case={case}");
        let children: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs WHERE queue='out'")
            .fetch_one(db.pool())
            .await
            .unwrap();
        let logs: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM moderation_log WHERE action='accept'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        let audits: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='moderation.accept'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        let committed = i64::from(case != "audit");
        assert_eq!((children, logs, audits), (committed, committed, committed));
        // Composed acceptance must retain every canonical child, including when
        // policy selects no immediate recipients; audit failure rolls all back.
        for queue in ["digest", "archive"] {
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs WHERE queue=$1")
                .bind(queue)
                .fetch_one(db.pool())
                .await
                .unwrap();
            assert_eq!(
                count, committed,
                "composed fanout case={case} queue={queue}"
            );
        }
        assert_eq!(
            db.moderation()
                .get(held)
                .await
                .unwrap()
                .disposition
                .is_some(),
            case != "audit"
        );
        println!(
            "HELD POLICY PASS case={case}: AsUser layers, SMTP spelling, exact recipients, atomic effects"
        );
        db.pool().close().await;
        std::fs::remove_file(path).unwrap();
    }
}
