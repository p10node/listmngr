use super::*;
use chrono::{DateTime, Duration, Utc};
use listmngr_core::{MemberRole, SubscriptionMode};

pub fn at() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-09-09T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}
async fn count(db: &Database, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
        .fetch_one(db.pool())
        .await
        .unwrap()
}
async fn install_sabotage(db: &Database, action: &str) {
    let sqlite = db.pool().acquire().await.unwrap().backend_name() == "SQLite";
    if sqlite {
        sqlx::query(&format!("CREATE TRIGGER reject_maintenance BEFORE INSERT ON audit_log WHEN NEW.action='{action}' BEGIN SELECT RAISE(ABORT,'private-fixture-diagnostic'); END")).execute(db.pool()).await.unwrap();
    } else {
        sqlx::raw_sql(&format!("CREATE FUNCTION reject_maintenance() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='{action}' THEN RAISE EXCEPTION 'private-fixture-diagnostic'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_maintenance BEFORE INSERT ON audit_log FOR EACH ROW EXECUTE FUNCTION reject_maintenance();")).execute(db.pool()).await.unwrap();
    }
}
async fn remove_sabotage(db: &Database) {
    let sqlite = db.pool().acquire().await.unwrap().backend_name() == "SQLite";
    if sqlite {
        sqlx::query("DROP TRIGGER reject_maintenance")
            .execute(db.pool())
            .await
            .unwrap();
    } else {
        sqlx::raw_sql(
            "DROP TRIGGER reject_maintenance ON audit_log; DROP FUNCTION reject_maintenance();",
        )
        .execute(db.pool())
        .await
        .unwrap();
    }
}

pub async fn audit_rollback(db: &Database, id: &listmngr_core::ListId) {
    let m = disabled(db, id, "Rollback@Example.invalid").await;
    let before = count(db, "audit_log").await;
    for action in ["bounce.warning", "bounce.remove", "bounce.removal_notice"] {
        let removing = action != "bounce.warning";
        db.lists().update(id,&json!({"bounce_you_are_disabled_warnings":if removing {0}else{3},"send_goodbye_message":true})).await.unwrap();
        install_sabotage(db, action).await;
        let summary = db
            .bounce_maintenance()
            .sweep_at(100, None, at())
            .await
            .unwrap();
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.warned, 0);
        assert_eq!(summary.removed, 0);
        assert!(db.members().get(m.id).await.is_ok());
        let state: (i64, Option<String>) =
            sqlx::query_as("SELECT total_warnings_sent,last_warning_sent FROM members WHERE id=$1")
                .bind(m.id.0.to_string())
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(state, (0, None));
        for table in [
            "workflow_notices",
            "queue_jobs",
            "messages",
            "message_blobs",
            "delivery_recipients",
        ] {
            assert_eq!(count(db, table).await, 0);
        }
        remove_sabotage(db).await;
    }
    assert_eq!(count(db, "audit_log").await, before + 3); // config writes only
    assert_eq!(
        db.bounce_maintenance()
            .sweep_at(100, None, at())
            .await
            .unwrap()
            .removed,
        1
    );
}
#[tokio::test]
async fn warning_removal_and_empty_admin_audits_rollback_every_effect() {
    let (db, id) = fixture().await;
    audit_rollback(&db, &id).await;
}
#[tokio::test]
async fn zero_count_removes_immediately_and_flags_are_independent() {
    for notify in [false, true] {
        let (db, id) = fixture().await;
        let m = disabled(&db, &id, "Zero@Example.invalid").await;
        sqlx::query("UPDATE members SET last_warning_sent=$1")
            .bind((at() + Duration::days(10)).to_rfc3339())
            .execute(db.pool())
            .await
            .unwrap();
        db.lists().update(&id,&json!({"bounce_you_are_disabled_warnings":0,"bounce_notify_owner_on_removal":notify})).await.unwrap();
        assert!(db.members().get(m.id).await.is_ok()); // config never deletes
        let s = db
            .bounce_maintenance()
            .sweep_at(1, None, at())
            .await
            .unwrap();
        assert_eq!(s.removed, 1);
        assert_eq!(s.warned, 0);
        assert_eq!(count(&db, "workflow_notices").await, 0);
        let audits: Vec<String> =
            sqlx::query_scalar("SELECT diff FROM audit_log WHERE action='bounce.removal_notice'")
                .fetch_all(db.pool())
                .await
                .unwrap();
        assert_eq!(audits.len(), usize::from(notify));
        if notify {
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&audits[0]).unwrap()["recipient_count"],
                0
            );
        }
    }
}
#[tokio::test]
async fn keyset_progresses_over_non_due_and_failed_items() {
    let (db, id) = fixture().await;
    let first = disabled(&db, &id, "First@Example.invalid").await;
    let second = disabled(&db, &id, "Second@Example.invalid").await;
    let third = disabled(&db, &id, "Third@Example.invalid").await;
    sqlx::query("UPDATE members SET last_warning_sent=$1 WHERE id=$2")
        .bind(at().to_rfc3339())
        .bind(first.id.0.to_string())
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE addresses SET original_email='private\r\nsecret' WHERE id=$1")
        .bind(second.address_id.0.to_string())
        .execute(db.pool())
        .await
        .unwrap();
    let a = db
        .bounce_maintenance()
        .sweep_at(1, None, at())
        .await
        .unwrap();
    assert_eq!(a.scanned, 1);
    assert_eq!(a.warned, 0);
    assert_eq!(a.next_cursor, Some(first.id.0));
    let b = db
        .bounce_maintenance()
        .sweep_at(2, a.next_cursor, at())
        .await
        .unwrap();
    assert_eq!(b.scanned, 2);
    assert_eq!(b.failed, 1);
    assert_eq!(b.warned, 1);
    assert_eq!(b.next_cursor, Some(third.id.0));
    let end = db
        .bounce_maintenance()
        .sweep_at(1, b.next_cursor, at())
        .await
        .unwrap();
    assert_eq!(end.scanned, 0);
    assert_eq!(end.next_cursor, None);
    assert_eq!(count(&db, "workflow_notices").await, 1);
    for bad in [0, 1001, u32::MAX] {
        assert!(
            db.bounce_maintenance()
                .sweep_at(bad, None, at())
                .await
                .is_err()
        );
    }
}
#[tokio::test]
async fn only_own_member_bounce_disable_is_eligible() {
    let (db, id) = fixture().await;
    let m = disabled(&db, &id, "Status@Example.invalid").await;
    for reason in ["enabled", "by_user", "by_moderator", "by_admin", "unknown"] {
        sqlx::query("UPDATE preferences SET delivery_status=$1 WHERE id=$2")
            .bind(reason)
            .bind(m.preferences_id.0.to_string())
            .execute(db.pool())
            .await
            .unwrap();
        let s = db
            .bounce_maintenance()
            .sweep_at(100, None, at())
            .await
            .unwrap();
        assert_eq!(s.warned, 0);
        assert_eq!(s.removed, 0);
    }
    sqlx::query("UPDATE preferences SET delivery_status='by_bounces' WHERE id=$1")
        .bind(m.preferences_id.0.to_string())
        .execute(db.pool())
        .await
        .unwrap();
    db.lists()
        .update(&id, &json!({"process_bounces":false}))
        .await
        .unwrap();
    assert_eq!(
        db.bounce_maintenance()
            .sweep_at(100, None, at())
            .await
            .unwrap()
            .scanned,
        0
    );
    db.lists()
        .update(&id, &json!({"process_bounces":true}))
        .await
        .unwrap();
    for role in ["owner", "moderator", "nonmember"] {
        sqlx::query("UPDATE members SET role=$1 WHERE id=$2")
            .bind(role)
            .bind(m.id.0.to_string())
            .execute(db.pool())
            .await
            .unwrap();
        assert_eq!(
            db.bounce_maintenance()
                .sweep_at(100, None, at())
                .await
                .unwrap()
                .scanned,
            0
        );
    }
    assert_eq!(count(&db, "workflow_notices").await, 0);
}
#[tokio::test]
async fn positive_warning_boundary_and_zero_interval_one_action_per_invocation() {
    let (db, id) = fixture().await;
    disabled(&db, &id, "Timing@Example.invalid").await;
    assert_eq!(
        db.bounce_maintenance()
            .sweep_at(100, None, at())
            .await
            .unwrap()
            .warned,
        1
    );
    let due = at() + Duration::days(7);
    assert_eq!(
        db.bounce_maintenance()
            .sweep_at(100, None, due - Duration::nanoseconds(1))
            .await
            .unwrap()
            .warned,
        0
    );
    assert_eq!(
        db.bounce_maintenance()
            .sweep_at(100, None, due)
            .await
            .unwrap()
            .warned,
        1
    );
    db.lists()
        .update(&id, &json!({"bounce_you_are_disabled_warnings_interval":0}))
        .await
        .unwrap();
    assert_eq!(
        db.bounce_maintenance()
            .sweep_at(100, None, due)
            .await
            .unwrap()
            .warned,
        1
    );
    assert_eq!(
        db.bounce_maintenance()
            .sweep_at(100, None, due)
            .await
            .unwrap()
            .removed,
        1
    );
}
#[tokio::test]
async fn removal_admin_dedup_isolation_other_roles_and_unsafe_roster_rollback() {
    let (db, id) = fixture().await;
    let member = disabled(&db, &id, "Both@Example.invalid").await;
    for role in [MemberRole::Owner, MemberRole::Moderator] {
        db.members()
            .create(listmngr_db::NewMember {
                list_id: id.clone(),
                email: "Both@Example.invalid".into(),
                role,
                subscription_mode: SubscriptionMode::AsAddress,
                display_name: String::new(),
            })
            .await
            .unwrap();
    }
    db.lists()
        .create(NewList {
            list_id: "other.example.invalid".parse().unwrap(),
            display_name: String::new(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    for email in ["Other@Example.invalid", "Both@Example.invalid"] {
        db.members()
            .create(listmngr_db::NewMember {
                list_id: "other.example.invalid".parse().unwrap(),
                email: email.into(),
                role: MemberRole::Owner,
                subscription_mode: SubscriptionMode::AsAddress,
                display_name: String::new(),
            })
            .await
            .unwrap();
    }
    db.lists()
        .update(
            &id,
            &json!({"bounce_you_are_disabled_warnings":0,"send_goodbye_message":true}),
        )
        .await
        .unwrap();
    sqlx::query(
        "UPDATE addresses SET original_email='maintenance-owner@example.invalid' WHERE id=$1",
    )
    .bind(member.address_id.0.to_string())
    .execute(db.pool())
    .await
    .unwrap();
    let fail = db
        .bounce_maintenance()
        .sweep_at(100, None, at())
        .await
        .unwrap();
    assert_eq!(fail.failed, 1);
    assert!(db.members().get(member.id).await.is_ok());
    assert_eq!(count(&db, "workflow_notices").await, 0);
    sqlx::query("UPDATE addresses SET original_email='Both@Example.invalid' WHERE id=$1")
        .bind(member.address_id.0.to_string())
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(
        db.bounce_maintenance()
            .sweep_at(100, None, at())
            .await
            .unwrap()
            .removed,
        1
    );
    let recipients: Vec<String> = sqlx::query_scalar("SELECT email FROM delivery_recipients")
        .fetch_all(db.pool())
        .await
        .unwrap();
    assert_eq!(recipients, ["Both@Example.invalid", "Both@Example.invalid"]);
    assert_eq!(count(&db, "members").await, 4);
}
