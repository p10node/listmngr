use super::score_cases::{finish, member, score};
use super::*;
use listmngr_core::{DeliveryStatus, MemberRole, Preferences, SmtpFailureStage};

#[tokio::test]
async fn winning_disable_resets_stale_warning_cycle() {
    let (db, lease) = fixture().await;
    member(&db, MemberRole::Member, true).await;
    db.lists()
        .update(
            &"test.example.com".parse().unwrap(),
            &serde_json::json!({"bounce_score_threshold":1}),
        )
        .await
        .unwrap();
    sqlx::query(
        "UPDATE members SET total_warnings_sent=91,last_warning_sent='1969-12-01T00:00:00Z'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    db.mail_queue()
        .begin_delivery(&lease, 103, &["Mixed@Example.com".into()])
        .await
        .unwrap();
    finish(&db, &lease, SmtpFailureStage::Rcpt, 550)
        .await
        .unwrap();
    let (count, receipt): (i64, Option<String>) =
        sqlx::query_as("SELECT total_warnings_sent,last_warning_sent FROM members")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(count, 0);
    assert_eq!(receipt, None);
}

#[tokio::test]
async fn legacy_admin_and_reset_bounces_remain_untouched() {
    for reason in ["by_admin", "by_bounces"] {
        let (db, lease) = fixture().await;
        member(&db, MemberRole::Member, true).await;
        sqlx::query("UPDATE preferences SET delivery_status=$1 WHERE id IN (SELECT preferences_id FROM members)").bind(reason).execute(db.pool()).await.unwrap();
        db.mail_queue()
            .begin_delivery(&lease, 103, &["Mixed@Example.com".into()])
            .await
            .unwrap();
        finish(&db, &lease, SmtpFailureStage::Rcpt, 550)
            .await
            .unwrap();
        assert!(score(&db).await.abs() < f64::EPSILON);
        let status:String=sqlx::query_scalar("SELECT delivery_status FROM preferences WHERE id IN (SELECT preferences_id FROM members)").fetch_one(db.pool()).await.unwrap();
        assert_eq!(status, reason);
        let received: Option<String> =
            sqlx::query_scalar("SELECT last_bounce_received FROM members")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert!(received.is_none());
    }
}

#[tokio::test]
async fn fractional_threshold_and_clock_preservation() {
    for (old, last, threshold, expected, disabled) in [
        (1.25, "1969-12-31T00:00:00+00:00", 2.5, 2.25, false),
        (1.5, "1969-12-31T00:00:00+00:00", 2.5, 0.0, true),
        (3.0, "1970-01-01T00:00:00+00:00", 2.5, 3.0, false),
        (3.0, "1970-01-01T00:00:00.104+00:00", 2.5, 3.0, false),
        (3.0, "1970-01-02T00:00:00+00:00", 2.5, 3.0, false),
        (3.0, "1969-12-25T00:00:00+00:00", 2.5, 1.0, false),
        (3.0, "1969-12-25T00:00:00+00:00", 1.0, 0.0, true),
        (1.0, "1969-12-25T00:00:00+00:00", 1.0, 0.0, true),
    ] {
        let (db, lease) = fixture().await;
        member(&db, MemberRole::Member, true).await;
        db.lists()
            .update(
                &"test.example.com".parse().unwrap(),
                &serde_json::json!({"bounce_score_threshold":threshold}),
            )
            .await
            .unwrap();
        sqlx::query("UPDATE members SET bounce_score=$1,last_bounce_received=$2")
            .bind(old)
            .bind(last)
            .execute(db.pool())
            .await
            .unwrap();
        db.mail_queue()
            .begin_delivery(&lease, 103, &["Mixed@Example.com".into()])
            .await
            .unwrap();
        finish(&db, &lease, SmtpFailureStage::Rcpt, 550)
            .await
            .unwrap();
        assert!(
            (score(&db).await - expected).abs() < f64::EPSILON,
            "{last}, threshold {threshold}"
        );
        let audits: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='bounce.disable'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(audits, i64::from(disabled));
    }
}

#[tokio::test]
async fn disabled_layers_preserve_reason_score_receipt_and_other_preferences() {
    for layer in ["member", "address", "user"] {
        for status in [
            DeliveryStatus::ByUser,
            DeliveryStatus::Unknown,
            DeliveryStatus::ByModerator,
            DeliveryStatus::ByBounces,
        ] {
            let (db, lease) = fixture().await;
            let user = db
                .users()
                .create(listmngr_db::NewUser {
                    email: "Mixed@Example.com".into(),
                    display_name: String::new(),
                    password: "Fixture!Orbit-7Quartz".into(),
                    server_owner: false,
                })
                .await
                .unwrap();
            member(&db, MemberRole::Member, true).await;
            let m = db
                .members()
                .roster(&"test.example.com".parse().unwrap(), MemberRole::Member)
                .await
                .unwrap()
                .remove(0);
            let p = Preferences {
                delivery_status: Some(status),
                hide_address: Some(true),
                ..Preferences::default()
            };
            match layer {
                "member" => db.preferences().set_member(m.id, p).await.unwrap(),
                "address" => db
                    .preferences()
                    .set_address("Mixed@Example.com", p)
                    .await
                    .unwrap(),
                _ => db.preferences().set_user(user.id, p).await.unwrap(),
            }
            sqlx::query("UPDATE members SET bounce_score=4,last_bounce_received='1969-12-31T00:00:00+00:00'").execute(db.pool()).await.unwrap();
            let before = db.preferences().get(m.preferences_id).await.unwrap();
            db.mail_queue()
                .begin_delivery(&lease, 103, &["Mixed@Example.com".into()])
                .await
                .unwrap();
            finish(&db, &lease, SmtpFailureStage::Rcpt, 550)
                .await
                .unwrap();
            assert!(
                (score(&db).await - 4.0).abs() < f64::EPSILON,
                "{layer} {status}"
            );
            assert_eq!(
                db.preferences()
                    .resolve_member(m.id, "en")
                    .await
                    .unwrap()
                    .delivery_status,
                Some(status)
            );
            assert_eq!(
                serde_json::to_value(before).unwrap(),
                serde_json::to_value(db.preferences().get(m.preferences_id).await.unwrap())
                    .unwrap()
            );
            let received: String = sqlx::query_scalar("SELECT last_bounce_received FROM members")
                .fetch_one(db.pool())
                .await
                .unwrap();
            assert_eq!(received, "1969-12-31T00:00:00+00:00");
            let processed: i64 = sqlx::query_scalar("SELECT processed FROM bounce_events")
                .fetch_one(db.pool())
                .await
                .unwrap();
            assert_eq!(processed, 0);
        }
    }
}

#[tokio::test]
async fn disable_audit_rollback_and_retry_preserves_member_preferences() {
    let (db, lease) = fixture().await;
    member(&db, MemberRole::Member, true).await;
    let m = db
        .members()
        .roster(&"test.example.com".parse().unwrap(), MemberRole::Member)
        .await
        .unwrap()
        .remove(0);
    db.preferences()
        .set_member(
            m.id,
            Preferences {
                hide_address: Some(true),
                preferred_language: Some("vi".into()),
                ..Preferences::default()
            },
        )
        .await
        .unwrap();
    sqlx::query(
        "UPDATE members SET bounce_score=4,last_bounce_received='1969-12-31T00:00:00+00:00'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    db.mail_queue()
        .begin_delivery(&lease, 103, &["Mixed@Example.com".into()])
        .await
        .unwrap();
    sqlx::query("CREATE TRIGGER disable_sabotage BEFORE INSERT ON audit_log WHEN NEW.action='bounce.disable' BEGIN SELECT RAISE(ABORT,'fixture'); END").execute(db.pool()).await.unwrap();
    assert!(
        finish(&db, &lease, SmtpFailureStage::Rcpt, 550)
            .await
            .is_err()
    );
    assert!((score(&db).await - 4.0).abs() < f64::EPSILON);
    assert_eq!(count(&db).await, 0);
    assert_eq!(
        db.preferences()
            .get(m.preferences_id)
            .await
            .unwrap()
            .delivery_status,
        None
    );
    let status: String = sqlx::query_scalar(
        "SELECT status FROM delivery_recipients WHERE email='Mixed@Example.com'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(status, "ambiguous");
    sqlx::query("DROP TRIGGER disable_sabotage")
        .execute(db.pool())
        .await
        .unwrap();
    finish(&db, &lease, SmtpFailureStage::Rcpt, 550)
        .await
        .unwrap();
    let p = db.preferences().get(m.preferences_id).await.unwrap();
    assert_eq!(p.delivery_status, Some(DeliveryStatus::ByBounces));
    assert_eq!(p.hide_address, Some(true));
    assert_eq!(p.preferred_language.as_deref(), Some("vi"));
    assert!((score(&db).await - 0.0).abs() < f64::EPSILON);
    assert!(
        finish(&db, &lease, SmtpFailureStage::Rcpt, 550)
            .await
            .is_err()
    );
    assert_eq!(count(&db).await, 1);
}
