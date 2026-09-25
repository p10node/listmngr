use listmngr_core::{Config, ListId, MemberRole, SubscriptionMode};
use listmngr_db::{Database, NewList, NewMember};
use listmngr_runners::{MailRoleConfig, serve_mail_role};
use std::time::Duration;
use tokio::{net::TcpListener, sync::watch, time::timeout};

async fn fixture() -> (Database, ListId, tempfile::TempDir) {
    // A cancelled SQLx operation may discard the sole pooled connection.
    // File-backed disposable state survives that legitimate shutdown behavior.
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("scheduler.db").display()
    );
    let db = Database::connect(&url, 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    let id: ListId = "scheduler.example.invalid".parse().unwrap();
    db.lists()
        .create(NewList {
            list_id: id.clone(),
            display_name: "Scheduler".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    db.lists()
        .update(&id, &serde_json::json!({"process_bounces":true}))
        .await
        .unwrap();
    (db, id, directory)
}

async fn disabled(db: &Database, id: &ListId, email: &str) {
    let member = db
        .members()
        .create(NewMember {
            list_id: id.clone(),
            email: email.into(),
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsAddress,
            display_name: String::new(),
        })
        .await
        .unwrap();
    sqlx::query("UPDATE preferences SET delivery_status='by_bounces' WHERE id=$1")
        .bind(member.preferences_id.0.to_string())
        .execute(db.pool())
        .await
        .unwrap();
}

async fn warnings(db: &Database) -> i64 {
    sqlx::query_scalar("SELECT COALESCE(SUM(total_warnings_sent),0) FROM members")
        .fetch_one(db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn real_scheduler_advances_multiple_pages_and_respects_list_interval() {
    timeout(Duration::from_secs(10), async {
        let (db, id, _directory) = fixture().await;
        for email in [
            "first@example.invalid",
            "second@example.invalid",
            "third@example.invalid",
        ] {
            disabled(&db, &id, email).await;
        }
        let (tx, rx) = watch::channel(false);
        let mut tasks = tokio::task::JoinSet::new();
        tasks.spawn(listmngr_runners::bounce_maintenance::run(
            db.clone(),
            Duration::from_millis(100),
            1,
            rx,
        ));
        let first_cycle = timeout(Duration::from_secs(2), async {
            while warnings(&db).await != 3 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        if first_cycle.is_err() {
            tx.send(true).unwrap();
            timeout(Duration::from_secs(1), tasks.join_next())
                .await
                .unwrap();
            panic!("bounded page cursor starved later members");
        }
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(warnings(&db).await, 3, "positive day interval ignored");
        db.lists()
            .update(
                &id,
                &serde_json::json!({"bounce_you_are_disabled_warnings_interval":0}),
            )
            .await
            .unwrap();
        timeout(Duration::from_secs(2), async {
            while warnings(&db).await < 6 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("empty-page wrap did not revisit non-due members");
        tx.send(true).unwrap();
        timeout(Duration::from_secs(1), tasks.join_next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let count = warnings(&db).await;
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            warnings(&db).await,
            count,
            "scheduler detached after shutdown"
        );
        db.pool().close().await;
    })
    .await
    .expect("bounded fixture timeout");
}

#[tokio::test]
async fn enabled_serve_publishes_due_warning_after_first_interval() {
    serve_case(true, true).await;
}

#[tokio::test]
async fn default_off_and_per_list_gate_do_not_publish() {
    serve_case(false, true).await;
    serve_case(true, false).await;
}

#[tokio::test]
async fn blocked_real_pool_acquisition_is_cancelled_on_shutdown() {
    timeout(Duration::from_secs(5), async {
        let (db, id, _directory) = fixture().await;
        disabled(&db, &id, "blocked@example.invalid").await;
        let held = db.pool().acquire().await.unwrap();
        let (tx, rx) = watch::channel(false);
        let mut tasks = tokio::task::JoinSet::new();
        tasks.spawn(listmngr_runners::bounce_maintenance::run(
            db.clone(),
            Duration::from_millis(20),
            1,
            rx,
        ));
        tokio::time::sleep(Duration::from_millis(100)).await;
        tx.send(true).unwrap();
        timeout(Duration::from_millis(200), tasks.join_next())
            .await
            .expect("blocked DB future not cancelled")
            .unwrap()
            .unwrap();
        drop(held);
        assert_eq!(warnings(&db).await, 0);
        db.pool().close().await;
    })
    .await
    .expect("bounded blocked-pool fixture");
}

async fn serve_case(enabled: bool, process_bounces: bool) {
    timeout(Duration::from_secs(10), async {
        let (db, id, _directory) = fixture().await;
        disabled(&db, &id, "Subscriber@example.invalid").await;
        db.lists()
            .update(&id, &serde_json::json!({"process_bounces":process_bounces}))
            .await
            .unwrap();
        let smtp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let lmtp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let index = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.archive.index_path = index.path().to_str().unwrap().into();
        config.mta.enabled = true;
        config.mta.smtp_tls = "plaintext_trusted_relay".into();
        config.mta.smtp_relay = smtp.local_addr().unwrap().to_string();
        config.mta.bounce_maintenance_enabled = enabled;
        config.mta.bounce_maintenance_interval_secs = 1;
        config.mta.bounce_maintenance_batch_size = 1;
        let mut role = MailRoleConfig::from_core(&config).unwrap();
        role.session_drain_timeout = Duration::from_millis(50);
        let (tx, rx) = watch::channel(false);
        let mut task = tokio::task::JoinSet::new();
        task.spawn(serve_mail_role(db.clone(), config, role, lmtp, None, rx));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(warnings(&db).await, 0, "first page must be delayed");
        let published = timeout(Duration::from_secs(3), async {
            while warnings(&db).await != 1 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        tx.send(true).unwrap();
        timeout(Duration::from_secs(1), task.join_next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .unwrap();
        if !enabled || !process_bounces {
            assert!(published.is_err());
            assert_eq!(warnings(&db).await, 0);
            db.pool().close().await;
            return;
        }
        assert!(
            published.is_ok(),
            "serve scheduler never published due warning"
        );
        let raw: Vec<u8> = sqlx::query_scalar("SELECT raw FROM message_blobs")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert!(
            String::from_utf8(raw)
                .unwrap()
                .contains("has been disabled due")
        );
        let recipients: Vec<String> = sqlx::query_scalar("SELECT email FROM delivery_recipients")
            .fetch_all(db.pool())
            .await
            .unwrap();
        assert_eq!(recipients, ["Subscriber@example.invalid"]);
        db.pool().close().await;
    })
    .await
    .expect("bounded fixture timeout");
}
