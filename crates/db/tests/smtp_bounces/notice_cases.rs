use super::score_cases::{finish, member};
use super::*;
use listmngr_core::{MemberRole, SmtpFailureStage, SubscriptionMode};

async fn setup() -> (Database, Lease) {
    setup_at("sqlite::memory:").await
}

async fn setup_at(url: &str) -> (Database, Lease) {
    let (db, lease) = fixture_at(url).await;
    member(&db, MemberRole::Member, true).await;
    db.lists()
        .create(NewList {
            list_id: "other.example.com".parse().unwrap(),
            display_name: String::new(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    db.members()
        .create(listmngr_db::NewMember {
            list_id: "other.example.com".parse().unwrap(),
            email: "Foreign@example.com".into(),
            display_name: String::new(),
            role: MemberRole::Owner,
            subscription_mode: SubscriptionMode::AsAddress,
        })
        .await
        .unwrap();
    db.lists()
        .update(
            &"test.example.com".parse().unwrap(),
            &serde_json::json!({"bounce_score_threshold":1}),
        )
        .await
        .unwrap();
    for (email, role) in [
        ("Owner@Example.com", MemberRole::Owner),
        ("Owner@Example.com", MemberRole::Moderator),
        ("Mod@example.com", MemberRole::Moderator),
    ] {
        db.members()
            .create(listmngr_db::NewMember {
                list_id: "test.example.com".parse().unwrap(),
                email: email.into(),
                display_name: String::new(),
                role,
                subscription_mode: SubscriptionMode::AsAddress,
            })
            .await
            .unwrap();
    }
    db.mail_queue()
        .begin_delivery(&lease, 103, &["Mixed@Example.com".into()])
        .await
        .unwrap();
    (db, lease)
}

#[tokio::test]
#[ignore = "requires explicit disposable TEST_POSTGRES_URL; owns schema"]
async fn postgres_disable_notice_snapshot_and_atomic_failure() {
    let url = std::env::var("TEST_POSTGRES_URL").expect("explicit disposable PostgreSQL URL");
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("disable_notice_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let url = format!(
        "{url}{}options=-csearch_path%3D{schema}",
        if url.contains('?') { '&' } else { '?' }
    );
    let (db, lease) = setup_at(&url).await;
    let cleanup = db.clone();
    let result = tokio::spawn(async move {
        let before = counts(&db).await;
        sqlx::raw_sql("CREATE FUNCTION notice_sabotage() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'fixture'; END $$; CREATE TRIGGER sabotage BEFORE INSERT ON workflow_notices FOR EACH ROW EXECUTE FUNCTION notice_sabotage();").execute(db.pool()).await.unwrap();
        assert!(finish(&db, &lease, SmtpFailureStage::Rcpt, 550).await.is_err());
        assert_eq!(counts(&db).await, before);
        let member_state: (f64, Option<String>) = sqlx::query_as("SELECT bounce_score,last_bounce_received FROM members WHERE role='member'").fetch_one(db.pool()).await.unwrap();
        assert!(member_state.0.abs() < f64::EPSILON);
        assert!(member_state.1.is_none());
        sqlx::query("DROP TRIGGER sabotage ON workflow_notices").execute(db.pool()).await.unwrap();
        snapshot_case(&db, &lease).await;
    }).await;
    cleanup.pool().close().await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    result.unwrap();
}

async fn counts(db: &Database) -> Vec<i64> {
    let mut result = Vec::new();
    for table in [
        "queue_jobs",
        "messages",
        "message_blobs",
        "workflow_notices",
        "bounce_events",
        "audit_log",
    ] {
        result.push(
            sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
                .fetch_one(db.pool())
                .await
                .unwrap(),
        );
    }
    result
}

#[tokio::test]
async fn disable_notice_failure_rolls_back_everything_then_retry() {
    for trigger in [
        "CREATE TRIGGER sabotage BEFORE INSERT ON audit_log WHEN NEW.action='bounce.disable_notice' BEGIN SELECT RAISE(ABORT,'fixture'); END",
        "CREATE TRIGGER sabotage BEFORE INSERT ON workflow_notices BEGIN SELECT RAISE(ABORT,'fixture'); END",
        "CREATE TRIGGER sabotage BEFORE INSERT ON audit_log WHEN NEW.action='bounce.disable' BEGIN SELECT RAISE(ABORT,'fixture'); END",
    ] {
        let (db, lease) = setup().await;
        let before = counts(&db).await;
        sqlx::query(trigger).execute(db.pool()).await.unwrap();
        assert!(
            finish(&db, &lease, SmtpFailureStage::Rcpt, 550)
                .await
                .is_err(),
            "notice publication must reach required audit"
        );
        assert_eq!(counts(&db).await, before);
        let member_state: (f64, Option<String>) = sqlx::query_as(
            "SELECT bounce_score,last_bounce_received FROM members WHERE role='member'",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert!(member_state.0.abs() < f64::EPSILON);
        assert!(member_state.1.is_none());
        let status: Option<String> = sqlx::query_scalar("SELECT p.delivery_status FROM preferences p JOIN members m ON m.preferences_id=p.id WHERE m.role='member'").fetch_one(db.pool()).await.unwrap();
        assert!(status.is_none());
        let state: String = sqlx::query_scalar(
            "SELECT status FROM delivery_recipients WHERE email='Mixed@Example.com'",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(state, "ambiguous");
        sqlx::query("DROP TRIGGER sabotage")
            .execute(db.pool())
            .await
            .unwrap();
        finish(&db, &lease, SmtpFailureStage::Rcpt, 550)
            .await
            .unwrap();
        assert_eq!(counts(&db).await[3], 2);
    }
}

#[tokio::test]
async fn disable_notice_snapshots_admins_once_and_replay_is_inert() {
    let (db, lease) = setup().await;
    snapshot_case(&db, &lease).await;
}

async fn snapshot_case(db: &Database, lease: &Lease) {
    finish(db, lease, SmtpFailureStage::Rcpt, 550)
        .await
        .unwrap();
    let recipients: Vec<String> = sqlx::query_scalar("SELECT r.email FROM delivery_recipients r JOIN workflow_notices n ON n.job_id=r.job_id ORDER BY r.email").fetch_all(db.pool()).await.unwrap();
    assert_eq!(recipients, vec!["Mod@example.com", "Owner@Example.com"]);
    assert!(
        finish(db, lease, SmtpFailureStage::Rcpt, 550)
            .await
            .is_err()
    );
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workflow_notices")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(n, 2);
    let raws: Vec<Vec<u8>> = sqlx::query_scalar("SELECT b.raw FROM message_blobs b JOIN messages m ON m.store_key=b.store_key JOIN queue_jobs q ON q.message_id=m.id JOIN workflow_notices n ON n.job_id=q.id").fetch_all(db.pool()).await.unwrap();
    for raw in raws {
        assert!(raw.len() <= 4096);
        let raw = String::from_utf8(raw).unwrap();
        assert!(raw.contains("Auto-Submitted: auto-generated\r\n"));
        assert!(
            raw.contains("Mixed@Example.com's subscription has been disabled on test@example.com")
        );
        assert!(!raw.contains("author@example.com"));
        assert!(!raw.contains("private"));
    }
}

#[tokio::test]
async fn disable_notice_off_and_zero_admins_do_not_prevent_disable() {
    for enabled in [false, true] {
        let (db, lease) = setup().await;
        if enabled {
            sqlx::query("DELETE FROM members WHERE list_id='test.example.com' AND role!='member'")
                .execute(db.pool())
                .await
                .unwrap();
        } else {
            db.lists()
                .update(
                    &"test.example.com".parse().unwrap(),
                    &serde_json::json!({"bounce_notify_owner_on_disable":false}),
                )
                .await
                .unwrap();
        }
        finish(&db, &lease, SmtpFailureStage::Rcpt, 550)
            .await
            .unwrap();
        assert_eq!(counts(&db).await[3], 0);
        let status: String = sqlx::query_scalar("SELECT p.delivery_status FROM preferences p JOIN members m ON m.preferences_id=p.id WHERE m.role='member'").fetch_one(db.pool()).await.unwrap();
        assert_eq!(status, "by_bounces");
        let audits: Vec<String> =
            sqlx::query_scalar("SELECT diff FROM audit_log WHERE action='bounce.disable_notice'")
                .fetch_all(db.pool())
                .await
                .unwrap();
        assert_eq!(audits.len(), usize::from(enabled));
        if enabled {
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&audits[0]).unwrap()["recipient_count"],
                0
            );
        }
        db.lists()
            .update(
                &"test.example.com".parse().unwrap(),
                &serde_json::json!({"bounce_notify_owner_on_disable":true}),
            )
            .await
            .unwrap();
        assert_eq!(counts(&db).await[3], 0);
    }
}
