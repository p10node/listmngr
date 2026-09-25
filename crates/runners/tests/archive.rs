use listmngr_core::{Config, ModerationAction};
use listmngr_db::mail_queue::{NewMessage, Queue};
use listmngr_db::{Database, NewList};
use listmngr_runners::{MailRoleConfig, run_in_processor};
use std::time::Duration;
#[tokio::test]
async fn mail_role_consumes_archive_and_survives_database_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}/archive.db?mode=rwc", dir.path().display());
    let db = Database::connect(&url, 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    db.lists()
        .create(NewList {
            list_id: "dev.example.invalid".parse().unwrap(),
            display_name: "dev".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw:
                    b"Message-ID: <restart@example.invalid>\r\nSubject: restart\r\n\r\nrestart-body"
                        .to_vec(),
                external_id: "<restart@example.invalid>".into(),
                context: r#"{"list_id":"dev.example.invalid"}"#.into(),
                queue: Queue::Archive,
                max_attempts: 3,
            },
            0,
        )
        .await
        .unwrap();
    let config = Config::default();
    let role = MailRoleConfig::from_core(&config).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (stop, rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(listmngr_runners::serve_mail_role(
        db.clone(),
        config,
        role,
        listener,
        None,
        rx,
    ));
    let result = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM archive_messages")
                .fetch_one(db.pool())
                .await
                .unwrap();
            if n == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    stop.send(true).unwrap();
    task.await.unwrap().unwrap();
    assert!(result.is_ok(), "running mail role must drain archive queue");
    db.pool().close().await;
    let db = Database::connect(&url, 1).await.unwrap();
    let rows = db
        .archive()
        .read(
            &"dev.example.invalid".parse().unwrap(),
            None,
            None,
            "",
            10,
            0,
        )
        .await
        .unwrap();
    assert_eq!(rows[0].body, "restart-body");
    assert!(
        db.mail_queue()
            .claim(
                Queue::Archive,
                "after-restart",
                chrono::Utc::now().timestamp_millis(),
                1000
            )
            .await
            .unwrap()
            .is_none()
    );
}
#[tokio::test]
async fn accepted_post_without_recipients_schedules_archive() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    db.lists()
        .create(NewList {
            list_id: "dev.example.invalid".parse().unwrap(),
            display_name: "dev".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let mut config = Config::default();
    config.mailman.default_nonmember_action = ModerationAction::Accept;
    let role = MailRoleConfig::from_core(&config).unwrap();
    db.mail_queue().enqueue(NewMessage {raw:b"From: poster@example.invalid\r\nTo: dev@example.invalid\r\nSubject: accepted\r\nMessage-ID: <accepted@example.invalid>\r\n\r\nbody\r\n".to_vec(),external_id:"<accepted@example.invalid>".into(),context:r#"{"list_id":"dev.example.invalid","envelope_sender":"poster@example.invalid"}"#.into(),queue:Queue::In,max_attempts:3},0).await.unwrap();
    let (stop, rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(run_in_processor(
        db.clone(),
        config,
        role,
        "in-test".into(),
        rx,
    ));
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM queue_jobs WHERE queue='in' AND state='done'",
            )
            .fetch_one(db.pool())
            .await
            .unwrap();
            if count == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    stop.send(true).unwrap();
    task.await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs WHERE queue='archive'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
}
