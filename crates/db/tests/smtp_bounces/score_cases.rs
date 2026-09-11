use super::*;
use listmngr_core::{MemberRole, SmtpFailure, SmtpFailureStage, SubscriptionMode};

pub async fn member(db: &Database, role: MemberRole, enabled: bool) {
    db.lists()
        .update(
            &"test.example.com".parse().unwrap(),
            &serde_json::json!({"process_bounces":enabled}),
        )
        .await
        .unwrap();
    db.members()
        .create(listmngr_db::NewMember {
            list_id: "test.example.com".parse().unwrap(),
            email: "Mixed@Example.com".into(),
            display_name: String::new(),
            role,
            subscription_mode: SubscriptionMode::AsAddress,
        })
        .await
        .unwrap();
}

pub async fn finish(
    db: &Database,
    lease: &Lease,
    stage: SmtpFailureStage,
    code: u16,
) -> listmngr_core::Result<()> {
    db.mail_queue()
        .finish_delivery_with_smtp(
            lease,
            104,
            &[(
                "Mixed@Example.com".into(),
                RecipientOutcome::Failed,
                String::new(),
            )],
            0,
            &[("Mixed@Example.com".into(), SmtpFailure { stage, code })],
        )
        .await
        .map(|_| ())
}

pub async fn score(db: &Database) -> f64 {
    sqlx::query_scalar("SELECT COALESCE(SUM(bounce_score),0.0) FROM members")
        .fetch_one(db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn threshold_disables_and_resets_only_on_fresh_increment() {
    let (db, lease) = fixture().await;
    member(&db, MemberRole::Member, true).await;
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
    finish(&db, &lease, SmtpFailureStage::Rcpt, 550)
        .await
        .unwrap();
    assert!(
        (score(&db).await - 0.0).abs() < f64::EPSILON,
        "threshold must reset score"
    );
    let status: Option<String> = sqlx::query_scalar(
        "SELECT p.delivery_status FROM members m JOIN preferences p ON p.id=m.preferences_id",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(status.as_deref(), Some("by_bounces"));
}

#[tokio::test]
async fn score_filters_and_default_off_preserve_event_history() {
    for (role, enabled, stage, code, expected) in [
        (MemberRole::Member, true, SmtpFailureStage::Rcpt, 550, 1.0),
        (MemberRole::Member, false, SmtpFailureStage::Rcpt, 550, 0.0),
        (MemberRole::Owner, true, SmtpFailureStage::Rcpt, 550, 0.0),
        (
            MemberRole::Moderator,
            true,
            SmtpFailureStage::Rcpt,
            550,
            0.0,
        ),
        (
            MemberRole::Nonmember,
            true,
            SmtpFailureStage::Rcpt,
            550,
            0.0,
        ),
        (MemberRole::Member, true, SmtpFailureStage::Ehlo, 550, 0.0),
        (
            MemberRole::Member,
            true,
            SmtpFailureStage::MailFrom,
            550,
            0.0,
        ),
        (
            MemberRole::Member,
            true,
            SmtpFailureStage::DataStart,
            550,
            0.0,
        ),
        (
            MemberRole::Member,
            true,
            SmtpFailureStage::DataFinal,
            550,
            0.0,
        ),
        (MemberRole::Member, true, SmtpFailureStage::Rcpt, 450, 0.0),
    ] {
        let (db, lease) = fixture().await;
        member(&db, role, enabled).await;
        db.lists()
            .update(
                &"test.example.com".parse().unwrap(),
                &serde_json::json!({"bounce_score_threshold":0.5}),
            )
            .await
            .unwrap();
        db.mail_queue()
            .begin_delivery(&lease, 103, &["Mixed@Example.com".into()])
            .await
            .unwrap();
        if code < 500 {
            assert!(finish(&db, &lease, stage, code).await.is_err());
            assert!((score(&db).await - 0.0).abs() < f64::EPSILON);
            assert_eq!(count(&db).await, 0);
            continue;
        }
        finish(&db, &lease, stage, code).await.unwrap();
        assert!(score(&db).await.abs() < f64::EPSILON);
        let disabled: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='bounce.disable'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(disabled, i64::from(expected > 0.0));
        assert_eq!(count(&db).await, 1);
    }
}

#[tokio::test]
async fn score_audit_failure_rolls_back_every_effect_and_retry_is_not_replayed() {
    for action in ["bounce.record", "bounce.score", "queue.retry"] {
        let (db, lease) = fixture().await;
        member(&db, MemberRole::Member, true).await;
        db.mail_queue()
            .begin_delivery(&lease, 103, &["Mixed@Example.com".into()])
            .await
            .unwrap();
        sqlx::query(&format!("CREATE TRIGGER score_sabotage BEFORE INSERT ON audit_log WHEN NEW.action='{action}' BEGIN SELECT RAISE(ABORT,'fixture'); END")).execute(db.pool()).await.unwrap();
        assert!(
            finish(&db, &lease, SmtpFailureStage::Rcpt, 550)
                .await
                .is_err()
        );
        assert!((score(&db).await - 0.0).abs() < f64::EPSILON);
        assert_eq!(count(&db).await, 0);
        let status: String = sqlx::query_scalar(
            "SELECT status FROM delivery_recipients WHERE email='Mixed@Example.com'",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(status, "ambiguous");
        sqlx::query("DROP TRIGGER score_sabotage")
            .execute(db.pool())
            .await
            .unwrap();
        finish(&db, &lease, SmtpFailureStage::Rcpt, 550)
            .await
            .unwrap();
        assert!((score(&db).await - 1.0).abs() < f64::EPSILON);
        let processed: i64 = sqlx::query_scalar("SELECT processed FROM bounce_events")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(processed, 1);
        assert!(
            finish(&db, &lease, SmtpFailureStage::Rcpt, 550)
                .await
                .is_err()
        );
        assert!((score(&db).await - 1.0).abs() < f64::EPSILON);
        assert_eq!(count(&db).await, 1);
    }
}

#[tokio::test]
async fn legacy_event_conflict_does_not_backfill_or_rescore() {
    let (db, lease) = fixture().await;
    member(&db, MemberRole::Member, true).await;
    db.mail_queue()
        .begin_delivery(&lease, 103, &["Mixed@Example.com".into()])
        .await
        .unwrap();
    sqlx::query("INSERT INTO bounce_events(id,list_id,recipient,job_id,message_id,created_at,source,context,processed) VALUES('old','test.example.com','Mixed@Example.com',$1,$2,100,'smtp_permanent_failure','normal',0)").bind(lease.job.id.0.to_string()).bind(lease.job.message_id.0.to_string()).execute(db.pool()).await.unwrap();
    finish(&db, &lease, SmtpFailureStage::Rcpt, 550)
        .await
        .unwrap();
    assert!((score(&db).await - 0.0).abs() < f64::EPSILON);
    let processed: i64 = sqlx::query_scalar("SELECT processed FROM bounce_events")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(processed, 0);
}
