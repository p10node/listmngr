use super::*;

async fn audit_diffs(db: &Database) -> Vec<String> {
    sqlx::query_scalar("SELECT diff FROM audit_log WHERE action='bounce.increment_notice'")
        .fetch_all(db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn increment_notice_daily_stale_same_value_and_out_of_order() {
    for (old, last, expected, notice_score) in [
        (1.0, "1969-12-25T00:00:00+00:00", 2, 1),
        (3.0, "1969-12-25T00:00:00+00:00", 2, 1),
        (1.0, "1969-12-31T00:00:00+00:00", 2, 2),
        (1.0, "1970-01-01T00:00:00+00:00", 0, 0),
        (1.0, "1970-01-01T00:00:00.104+00:00", 0, 0),
        (1.0, "1970-01-02T00:00:00+00:00", 0, 0),
    ] {
        let (db, lease) = setup_at("sqlite::memory:").await;
        sqlx::query(
            "UPDATE members SET bounce_score=$1,last_bounce_received=$2 WHERE role='member'",
        )
        .bind(old)
        .bind(last)
        .execute(db.pool())
        .await
        .unwrap();
        finish(&db, &lease, SmtpFailureStage::Rcpt, 550)
            .await
            .unwrap();
        let raws = notices(&db).await;
        assert_eq!(raws.len(), expected, "{last}");
        for raw in raws {
            assert!(
                String::from_utf8(raw)
                    .unwrap()
                    .contains(&format!("has been incremented to {notice_score}.\r\n"))
            );
        }
        let receipt: String =
            sqlx::query_scalar("SELECT last_bounce_received FROM members WHERE role='member'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        if last == "1970-01-01T00:00:00+00:00" {
            assert_eq!(receipt, "1970-01-01T00:00:00.104+00:00");
        }
    }
}

#[tokio::test]
async fn increment_notice_flags_independent_at_threshold_and_config_only_inert() {
    for increment in [false, true] {
        for disable in [false, true] {
            let (db, lease) = setup_at("sqlite::memory:").await;
            db.lists().update(&"test.example.com".parse().unwrap(),&serde_json::json!({"bounce_score_threshold":1,"bounce_notify_owner_on_disable":disable,"bounce_notify_owner_on_bounce_increment":increment})).await.unwrap();
            assert!(notices(&db).await.is_empty());
            finish(&db, &lease, SmtpFailureStage::Rcpt, 550)
                .await
                .unwrap();
            let raws = notices(&db).await;
            assert_eq!(
                raws.len(),
                2 * (usize::from(increment) + usize::from(disable))
            );
            assert_eq!(audit_diffs(&db).await.len(), usize::from(increment));
            let score: f64 =
                sqlx::query_scalar("SELECT bounce_score FROM members WHERE role='member'")
                    .fetch_one(db.pool())
                    .await
                    .unwrap();
            assert!(score.abs() < f64::EPSILON);
            for raw in raws {
                let raw = String::from_utf8(raw).unwrap();
                if raw.contains("Subject: Member bounce score increased") {
                    assert!(raw.contains("bounce score of 1"));
                }
            }
        }
    }
}

#[tokio::test]
async fn increment_notice_empty_roster_audits_zero_and_foreign_owner_excluded() {
    let (db, lease) = setup_at("sqlite::memory:").await;
    db.lists()
        .create(NewList {
            list_id: "other.example.com".parse().unwrap(),
            display_name: String::new(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    sqlx::query("UPDATE members SET list_id='other.example.com' WHERE role!='member'")
        .execute(db.pool())
        .await
        .unwrap();
    finish(&db, &lease, SmtpFailureStage::Rcpt, 550)
        .await
        .unwrap();
    assert!(notices(&db).await.is_empty());
    let diffs = audit_diffs(&db).await;
    assert_eq!(diffs.len(), 1);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&diffs[0]).unwrap()["recipient_count"],
        0
    );
}

#[tokio::test]
async fn increment_notice_ineligible_observations_are_inert() {
    for kind in [
        "disabled",
        "process_off",
        "nonmember",
        "internal",
        "nonrcpt",
    ] {
        let (db, lease) = setup_at("sqlite::memory:").await;
        let query = match kind {
            "disabled" => {
                "UPDATE preferences SET delivery_status='by_user' WHERE id IN (SELECT preferences_id FROM members WHERE role='member')"
            }
            "process_off" => "UPDATE mailing_lists SET process_bounces=0",
            "nonmember" => "UPDATE members SET role='nonmember' WHERE role='member'",
            "internal" => {
                "INSERT INTO workflow_notices(job_id) SELECT id FROM queue_jobs WHERE queue='out'"
            }
            _ => "UPDATE mailing_lists SET process_bounces=1",
        };
        sqlx::query(query).execute(db.pool()).await.unwrap();
        let before = notices(&db).await.len();
        let stage = if kind == "nonrcpt" {
            SmtpFailureStage::DataFinal
        } else {
            SmtpFailureStage::Rcpt
        };
        finish(&db, &lease, stage, 550).await.unwrap();
        assert_eq!(notices(&db).await.len(), before, "{kind}");
        assert!(audit_diffs(&db).await.is_empty(), "{kind}");
    }
}

async fn state(db: &Database) -> Vec<String> {
    let mut state = Vec::new();
    for table in [
        "queue_jobs",
        "messages",
        "message_blobs",
        "workflow_notices",
        "bounce_events",
        "audit_log",
    ] {
        let n: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(db.pool())
            .await
            .unwrap();
        state.push(n.to_string());
    }
    let rows:Vec<(f64,Option<String>,Option<String>)>=sqlx::query_as("SELECT m.bounce_score,m.last_bounce_received,p.delivery_status FROM members m JOIN preferences p ON p.id=m.preferences_id WHERE m.role='member'").fetch_all(db.pool()).await.unwrap();
    state.push(format!("{rows:?}"));
    let rows: Vec<(String, String, Option<String>)> =
        sqlx::query_as("SELECT email,status,attempt_token FROM delivery_recipients ORDER BY email")
            .fetch_all(db.pool())
            .await
            .unwrap();
    state.push(format!("{rows:?}"));
    let rows: Vec<(String, Option<String>, Option<i64>)> =
        sqlx::query_as("SELECT state,lease_token,lease_until FROM queue_jobs ORDER BY id")
            .fetch_all(db.pool())
            .await
            .unwrap();
    state.push(format!("{rows:?}"));
    state
}

#[tokio::test]
async fn increment_notice_audit_and_provenance_failure_roll_back_all_effects() {
    for trigger in [
        "CREATE TRIGGER sabotage BEFORE INSERT ON audit_log WHEN NEW.action='bounce.increment_notice' BEGIN SELECT RAISE(ABORT,'fixture'); END",
        "CREATE TRIGGER sabotage BEFORE INSERT ON workflow_notices BEGIN SELECT RAISE(ABORT,'fixture'); END",
        "CREATE TRIGGER sabotage BEFORE INSERT ON audit_log WHEN NEW.action='bounce.disable_notice' BEGIN SELECT RAISE(ABORT,'fixture'); END",
    ] {
        let (db, lease) = setup_at("sqlite::memory:").await;
        db.lists()
            .update(
                &"test.example.com".parse().unwrap(),
                &serde_json::json!({"bounce_score_threshold":1}),
            )
            .await
            .unwrap();
        let before = state(&db).await;
        sqlx::query(trigger).execute(db.pool()).await.unwrap();
        assert!(
            finish(&db, &lease, SmtpFailureStage::Rcpt, 550)
                .await
                .is_err()
        );
        assert_eq!(state(&db).await, before);
        sqlx::query("DROP TRIGGER sabotage")
            .execute(db.pool())
            .await
            .unwrap();
        finish(&db, &lease, SmtpFailureStage::Rcpt, 550)
            .await
            .unwrap();
        assert_eq!(notices(&db).await.len(), 4);
    }
}

#[tokio::test]
#[ignore = "requires explicit disposable TEST_POSTGRES_URL; owns schema"]
async fn postgres_increment_notice_atomic_regression() {
    let url = std::env::var("TEST_POSTGRES_URL").expect("explicit disposable PostgreSQL URL");
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("increment_notice_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let scoped = format!(
        "{url}{}options=-csearch_path%3D{schema}",
        if url.contains('?') { '&' } else { '?' }
    );
    let result=tokio::spawn(async move {
        let (db,lease)=setup_at(&scoped).await;
        let before=state(&db).await;
        sqlx::raw_sql("CREATE FUNCTION increment_sabotage() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='bounce.increment_notice' THEN RAISE EXCEPTION 'fixture'; END IF; RETURN NEW; END $$; CREATE TRIGGER sabotage BEFORE INSERT ON audit_log FOR EACH ROW EXECUTE FUNCTION increment_sabotage();").execute(db.pool()).await.unwrap();
        assert!(finish(&db,&lease,SmtpFailureStage::Rcpt,550).await.is_err());
        assert_eq!(state(&db).await,before);
        sqlx::query("DROP TRIGGER sabotage ON audit_log").execute(db.pool()).await.unwrap();
        finish(&db,&lease,SmtpFailureStage::Rcpt,550).await.unwrap();
        assert_eq!(notices(&db).await.len(),2);
        assert_eq!(audit_diffs(&db).await.len(),1);
        db.pool().close().await;
    }).await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    result.unwrap();
}
