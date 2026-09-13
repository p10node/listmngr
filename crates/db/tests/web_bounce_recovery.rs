use listmngr_core::{MemberRole, SubscriptionMode};
use listmngr_db::{Database, NewList, NewMember, NewUser, web_sessions::WebSession};
use sqlx::Row;

async fn seed(db: &Database) -> (listmngr_core::Member, WebSession) {
    db.domains()
        .create("recover.invalid", "", None)
        .await
        .unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: "list.recover.invalid".parse().unwrap(),
            display_name: "Recovery".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let _user = db
        .users()
        .create(NewUser {
            email: "own@recover.invalid".into(),
            display_name: "Own".into(),
            password: "a very secure fixture password".into(),
            server_owner: false,
        })
        .await
        .unwrap();
    db.addresses()
        .verify("own@recover.invalid", true)
        .await
        .unwrap();
    let m = db
        .members()
        .create(NewMember {
            list_id: list.id,
            email: "own@recover.invalid".into(),
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsUser,
            display_name: "Own".into(),
        })
        .await
        .unwrap();
    sqlx::query("UPDATE preferences SET delivery_status='by_bounces',delivery_mode='mime_digests' WHERE id=$1").bind(m.preferences_id.0.to_string()).execute(db.pool()).await.unwrap();
    sqlx::query("UPDATE members SET bounce_score=7,last_bounce_received='2026-01-01T00:00:00Z',total_warnings_sent=2,last_warning_sent='2026-01-02T00:00:00Z' WHERE id=$1").bind(m.id.to_string()).execute(db.pool()).await.unwrap();
    let anon = db
        .create_web_session(None, None, chrono::Utc::now().timestamp_millis())
        .await
        .unwrap();
    let session = db
        .browser_login(
            "own@recover.invalid",
            "a very secure fixture password",
            &anon,
        )
        .await
        .unwrap();
    (m, session)
}

async fn success(db: &Database) {
    let (m, s) = seed(db).await;
    db.browser_recover(&s, m.id).await.unwrap();
    let row=sqlx::query("SELECT m.bounce_score,m.last_bounce_received,m.total_warnings_sent,m.last_warning_sent,p.delivery_status,p.delivery_mode FROM members m JOIN preferences p ON p.id=m.preferences_id WHERE m.id=$1").bind(m.id.to_string()).fetch_one(db.pool()).await.unwrap();
    assert_eq!(row.get::<String, _>("delivery_status"), "enabled");
    assert_eq!(row.get::<String, _>("delivery_mode"), "mime_digests");
    assert!(row.get::<f64, _>("bounce_score").abs() < f64::EPSILON);
    assert_eq!(row.get::<i64, _>("total_warnings_sent"), 0);
    assert!(
        row.get::<Option<String>, _>("last_bounce_received")
            .is_none()
    );
    assert!(row.get::<Option<String>, _>("last_warning_sent").is_none());
    let audits:i64=sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='bounce.recover' AND actor_user_id=$1 AND target_id=$2").bind(s.user_id.unwrap().to_string()).bind(m.id.to_string()).fetch_one(db.pool()).await.unwrap();
    assert_eq!(audits, 1);
}

async fn eligibility(db: &Database) -> (listmngr_core::Member, WebSession) {
    let (m, s) = seed(db).await;
    let inherited = uuid::Uuid::now_v7().to_string();
    sqlx::query("INSERT INTO preferences (id,delivery_status) VALUES ($1,'by_bounces')")
        .bind(&inherited)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE addresses SET preferences_id=$1 WHERE email='own@recover.invalid'")
        .bind(&inherited)
        .execute(db.pool())
        .await
        .unwrap();
    let foreign = db
        .users()
        .create(NewUser {
            email: "foreign@recover.invalid".into(),
            display_name: "Foreign".into(),
            password: "different secure fixture password".into(),
            server_owner: false,
        })
        .await
        .unwrap();
    let foreign_session = db
        .create_web_session(
            Some(foreign.id),
            None,
            chrono::Utc::now().timestamp_millis(),
        )
        .await
        .unwrap();
    assert!(
        db.browser_recover_preview(&foreign_session, m.id)
            .await
            .is_err()
    );
    assert!(db.browser_recover(&foreign_session, m.id).await.is_err());
    for status in [
        Some("by_moderator"),
        Some("unknown"),
        Some("by_user"),
        Some("enabled"),
        None,
    ] {
        sqlx::query("UPDATE preferences SET delivery_status=$1 WHERE id=$2")
            .bind(status)
            .bind(m.preferences_id.0.to_string())
            .execute(db.pool())
            .await
            .unwrap();
        assert!(db.browser_recover_preview(&s, m.id).await.is_err());
        assert!(db.browser_recover(&s, m.id).await.is_err());
    }
    sqlx::query("UPDATE preferences SET delivery_status='by_bounces' WHERE id=$1")
        .bind(m.preferences_id.0.to_string())
        .execute(db.pool())
        .await
        .unwrap();
    for (change, undo) in [
        (
            "UPDATE addresses SET verified_on=NULL WHERE email='own@recover.invalid'",
            "UPDATE addresses SET verified_on='2026-01-01T00:00:00Z' WHERE email='own@recover.invalid'",
        ),
        (
            "UPDATE addresses SET user_id=NULL WHERE email='own@recover.invalid'",
            "UPDATE addresses SET user_id=(SELECT user_id FROM members WHERE role='member') WHERE email='own@recover.invalid'",
        ),
        (
            "UPDATE members SET user_id=NULL",
            "UPDATE members SET user_id=(SELECT user_id FROM addresses WHERE email='own@recover.invalid')",
        ),
        (
            "UPDATE members SET role='moderator'",
            "UPDATE members SET role='member'",
        ),
        (
            "UPDATE members SET role='nonmember'",
            "UPDATE members SET role='member'",
        ),
    ] {
        sqlx::query(change).execute(db.pool()).await.unwrap();
        assert!(db.browser_recover(&s, m.id).await.is_err());
        sqlx::query(undo).execute(db.pool()).await.unwrap();
    }
    assert!(
        db.browser_preferences(
            &s,
            m.id,
            listmngr_core::DeliveryMode::Regular,
            listmngr_core::DeliveryStatus::Enabled
        )
        .await
        .is_err()
    );
    (m, s)
}

async fn rollback(db: &Database, sqlite: bool, m: &listmngr_core::Member, s: &WebSession) {
    sqlx::query(if sqlite {"CREATE TRIGGER reject_recover BEFORE INSERT ON audit_log WHEN NEW.action='bounce.recover' BEGIN SELECT RAISE(ABORT,'owned failure'); END"} else {"ALTER TABLE audit_log ADD CONSTRAINT reject_recover CHECK (action <> 'bounce.recover') NOT VALID"}).execute(db.pool()).await.unwrap();
    assert!(db.browser_recover(s, m.id).await.is_err());
    let row=sqlx::query("SELECT bounce_score,total_warnings_sent,last_bounce_received,last_warning_sent FROM members WHERE id=$1").bind(m.id.to_string()).fetch_one(db.pool()).await.unwrap();
    assert!((row.get::<f64, _>("bounce_score") - 7.0).abs() < f64::EPSILON);
    assert_eq!(row.get::<i64, _>("total_warnings_sent"), 2);
    assert!(
        row.get::<Option<String>, _>("last_bounce_received")
            .is_some()
    );
    assert!(row.get::<Option<String>, _>("last_warning_sent").is_some());
    let preference = db.preferences().get(m.preferences_id).await.unwrap();
    assert_eq!(
        preference.delivery_status,
        Some(listmngr_core::DeliveryStatus::ByBounces)
    );
    assert_eq!(
        preference.delivery_mode,
        Some(listmngr_core::DeliveryMode::MimeDigests)
    );
    sqlx::query(if sqlite {
        "DROP TRIGGER reject_recover"
    } else {
        "ALTER TABLE audit_log DROP CONSTRAINT reject_recover"
    })
    .execute(db.pool())
    .await
    .unwrap();
}

async fn controls(db: &Database, sqlite: bool) {
    let (m, s) = eligibility(db).await;
    rollback(db, sqlite, &m, &s).await;
    db.browser_recover(&s, m.id).await.unwrap();
    assert!(db.browser_recover(&s, m.id).await.is_err());
    let audits: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='bounce.recover'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(audits, 1);
    sqlx::query("UPDATE mailing_lists SET process_bounces=1")
        .execute(db.pool())
        .await
        .unwrap();
    let sweep = db
        .bounce_maintenance()
        .sweep_at(100, None, chrono::Utc::now() + chrono::Duration::days(100))
        .await
        .unwrap();
    assert_eq!((sweep.warned, sweep.removed, sweep.failed), (0, 0, 0));
    sqlx::query("UPDATE preferences SET delivery_status='by_bounces' WHERE id=$1")
        .bind(m.preferences_id.0.to_string())
        .execute(db.pool())
        .await
        .unwrap();
    db.delete_web_session(&s).await.unwrap();
    assert!(db.browser_recover(&s, m.id).await.is_err());
}

#[tokio::test]
async fn sqlite_recovery_controls() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    controls(&db, true).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns isolated schema"]
async fn postgres_recovery_controls() {
    let url = std::env::var("TEST_POSTGRES_URL").unwrap();
    let admin = Database::connect(&url, 1).await.unwrap();
    let schema = format!("web_recovery_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(admin.pool())
        .await
        .unwrap();
    let sep = if url.contains('?') { '&' } else { '?' };
    let fixture = format!("{url}{sep}options=-csearch_path%3D{schema}");
    let result = tokio::spawn(async move {
        let db = Database::connect(&fixture, 3).await.unwrap();
        db.migrate().await.unwrap();
        controls(&db, false).await;
        db.pool().close().await;
    })
    .await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(admin.pool())
        .await
        .unwrap();
    admin.pool().close().await;
    result.unwrap();
}

#[tokio::test]
async fn sqlite_recovery_success() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    success(&db).await;
}
