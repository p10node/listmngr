//! Supplemental recovery contention coverage; not retroactive test-first evidence.
use super::*;

#[tokio::test]
async fn recovery_rechecks_authority_after_observed_writer_contention() {
    for change in [
        "valid",
        "session",
        "password",
        "unverify",
        "unlink",
        "as_user",
        "expiry",
        "policy_member",
    ] {
        let (db, path, user, member, _, session) = fixture(false, false).await;
        sqlx::query("UPDATE preferences SET delivery_status='by_bounces',delivery_mode='mime_digests' WHERE id=$1")
            .bind(member.preferences_id.0.to_string()).execute(db.pool()).await.unwrap();
        sqlx::query("UPDATE members SET bounce_score=7,total_warnings_sent=2 WHERE id=$1")
            .bind(member.id.to_string())
            .execute(db.pool())
            .await
            .unwrap();
        assert_eq!(
            db.browser_recover_preview(&session, member.id)
                .await
                .unwrap()
                .0,
            member.list_id
        );
        let expiry = chrono::Utc::now().timestamp_millis() + 1000;
        if change == "expiry" {
            sqlx::query("UPDATE web_sessions SET expires_at=$1 WHERE user_id=$2")
                .bind(expiry)
                .bind(user.id.to_string())
                .execute(db.pool())
                .await
                .unwrap();
        }
        let mut blocker = db.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let request = db.clone();
        let member_id = member.id;
        let task = tokio::spawn(BROWSER_LOCK_BUSY.scope(sender, async move {
            request.browser_recover(&session, member_id).await
        }));
        tokio::time::timeout(std::time::Duration::from_secs(10), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(!task.is_finished());
        if change == "expiry" {
            assert!(chrono::Utc::now().timestamp_millis() < expiry);
            while chrono::Utc::now().timestamp_millis() < expiry {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        }
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
        let preference = db.preferences().get(member.preferences_id).await.unwrap();
        let expected = match change {
            "valid" => DeliveryStatus::Enabled,
            "policy_member" => DeliveryStatus::ByModerator,
            _ => DeliveryStatus::ByBounces,
        };
        assert_eq!(preference.delivery_status, Some(expected));
        assert_eq!(preference.delivery_mode, Some(DeliveryMode::MimeDigests));
        let (score, warnings): (f64, i64) =
            sqlx::query_as("SELECT bounce_score,total_warnings_sent FROM members WHERE id=$1")
                .bind(member.id.to_string())
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert!((score - if change == "valid" { 0.0 } else { 7.0 }).abs() < f64::EPSILON);
        assert_eq!(warnings, if change == "valid" { 0 } else { 2 });
        let audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE action='bounce.recover' AND target_id=$1",
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
