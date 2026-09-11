use listmngr_db::mail_queue::{NewMessage, Queue};
use listmngr_db::{Database, NewList};

async fn fixture() -> Database {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    seeded_fixture(db).await
}

async fn seeded_fixture(db: Database) -> Database {
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
    db
}
#[tokio::test]
async fn munge_does_not_change_archive_authorship_at_ingestion_or_publication() {
    let db = fixture().await;
    let list = "dev.example.invalid".parse().unwrap();
    let lease = enqueue(
        &db,
        "dev.example.invalid",
        "munge",
        "From: Author <author@elsewhere.invalid>\r\nSender: old@elsewhere.invalid\r\n",
        "body\r\n",
    )
    .await;
    listmngr_archive::process(&db, &lease, 101).await.unwrap();
    db.lists().update(&list,&serde_json::json!({"dmarc_mitigate_action":"munge_from","dmarc_mitigate_unconditionally":true})).await.unwrap();
    let rows = db
        .archive()
        .read(&list, None, None, "", 100, 0)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&rows[0].raw);
    assert!(text.contains("From: Author <author@elsewhere.invalid>"));
    assert!(text.contains("Sender: old@elsewhere.invalid"));
    assert!(text.ends_with("\r\n\r\nbody\r\n"));
    let lease = enqueue(
        &db,
        "dev.example.invalid",
        "munge-enabled",
        "From: Author <author@elsewhere.invalid>\r\n",
        "body2\r\n",
    )
    .await;
    listmngr_archive::process(&db, &lease, 101).await.unwrap();
    let rows = db
        .archive()
        .read(&list, None, None, "body2", 100, 0)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&rows[0].raw);
    assert!(text.contains("From: Author <author@elsewhere.invalid>"));
    assert!(!text.contains("via dev@example.invalid"));
    db.lists()
        .update(&list, &serde_json::json!({"anonymous_list":true}))
        .await
        .unwrap();
    let rows = db
        .archive()
        .read(&list, None, None, "", 100, 0)
        .await
        .unwrap();
    for row in rows {
        assert!(!String::from_utf8_lossy(&row.raw).contains("elsewhere.invalid"));
    }
}

#[tokio::test]
async fn archive_publication_cooks_headers_and_indexes_safe_mime() {
    for anonymous in [false, true] {
        let db = fixture().await;
        let list = "dev.example.invalid".parse().unwrap();
        db.lists()
            .update(&list, &serde_json::json!({"anonymous_list":anonymous}))
            .await
            .unwrap();
        let body = "--boundary\r\nContent-Type: text/plain\r\n\r\nunique body\r\n--boundary--\r\n";
        let lease = enqueue(&db, "dev.example.invalid", "privacy", "From: author@secret.invalid\r\nBcc: hidden@secret.invalid\r\nApproved: password\r\nContent-Type: multipart/mixed; boundary=boundary\r\n", body).await;
        listmngr_archive::process(&db, &lease, 101).await.unwrap();
        let rows = db
            .archive()
            .read(&list, None, None, "unique body", 100, 0)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        let published = String::from_utf8(rows[0].raw.clone()).unwrap();
        assert!(!published.contains("Bcc:"), "archive leaks Bcc");
        assert!(!published.contains("password"), "archive leaks approval");
        assert_eq!(published.contains("author@secret.invalid"), !anonymous);
        assert_eq!(published.split_once("\r\n\r\n").unwrap().1, body);
        assert!(published.contains("List-Post:"));
        assert!(
            String::from_utf8(listmngr_archive::mbox(&rows))
                .unwrap()
                .contains(body)
        );
        db.lists()
            .update(&list, &serde_json::json!({"anonymous_list":true}))
            .await
            .unwrap();
        let reread = db
            .archive()
            .read(&list, None, None, "", 100, 0)
            .await
            .unwrap();
        assert!(
            !String::from_utf8_lossy(&reread[0].raw).contains("secret.invalid"),
            "publication must revalidate current anonymous policy"
        );
    }
}
#[tokio::test]
async fn approved_held_post_schedules_archive_without_recipients() {
    let db = fixture().await;
    let message = db
        .mail_queue()
        .enqueue(
            NewMessage {
                raw: b"Message-ID: <held@example.invalid>\r\n\r\nheld body".to_vec(),
                external_id: "<held@example.invalid>".into(),
                context: r#"{"list_id":"dev.example.invalid"}"#.into(),
                queue: Queue::In,
                max_attempts: 3,
            },
            100,
        )
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::In, "in", 100, 1000)
        .await
        .unwrap()
        .unwrap();
    let held = db
        .moderation()
        .hold(
            &lease,
            &"dev.example.invalid".parse().unwrap(),
            "sender@example.invalid",
            "subject",
            "moderation",
            101,
        )
        .await
        .unwrap();
    db.moderation()
        .review(
            held.id,
            &listmngr_db::AuditContext::system(),
            &listmngr_db::moderation::ReviewAction::Accept { max_attempts: 3 },
            "approved",
            102,
        )
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::Archive, "archive", 103, 1000)
        .await
        .unwrap();
    assert!(
        lease.is_some(),
        "approval must schedule archive even with zero recipients"
    );
    assert_eq!(lease.unwrap().job.message_id, message.message_id);
    assert!(
        db.moderation()
            .accept(held.id, None, &[], 3, 104)
            .await
            .is_err()
    );
    for queue in ["out", "digest", "archive"] {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs WHERE message_id=$1 AND queue=$2")
                .bind(message.message_id.0.to_string())
                .bind(queue)
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(
            count, 1,
            "held approval must create each child exactly once"
        );
    }
}
async fn enqueue(
    db: &Database,
    list: &str,
    id: &str,
    headers: &str,
    body: &str,
) -> listmngr_db::mail_queue::Lease {
    let raw = format!("Message-ID: <{id}@example.invalid>\r\nSubject: {id}\r\n{headers}\r\n{body}");
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: raw.into_bytes(),
                external_id: format!("<{id}@example.invalid>"),
                context: serde_json::json!({"list_id":list}).to_string(),
                queue: Queue::Archive,
                max_attempts: 3,
            },
            100,
        )
        .await
        .unwrap();
    db.mail_queue()
        .claim(Queue::Archive, "test", 100, 1000)
        .await
        .unwrap()
        .unwrap()
}
#[tokio::test]
async fn reply_to_reply_joins_root_without_cross_list_aliasing() {
    let db = fixture().await;
    for (id, headers) in [
        ("root", ""),
        ("child", "In-Reply-To: <root@example.invalid>\r\n"),
        ("grandchild", "In-Reply-To: <child@example.invalid>\r\n"),
    ] {
        let lease = enqueue(&db, "dev.example.invalid", id, headers, id).await;
        listmngr_archive::process(&db, &lease, 101).await.unwrap();
    }
    let root = listmngr_mail::message_id_hash("root@example.invalid").unwrap();
    let rows = db
        .archive()
        .read(
            &"dev.example.invalid".parse().unwrap(),
            None,
            Some(&root),
            "",
            100,
            0,
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 3);
    db.lists()
        .create(NewList {
            list_id: "other.example.invalid".parse().unwrap(),
            display_name: "other".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let lease = enqueue(
        &db,
        "other.example.invalid",
        "root",
        "",
        "different list body",
    )
    .await;
    listmngr_archive::process(&db, &lease, 101).await.unwrap();
    let rows = db
        .archive()
        .read(
            &"other.example.invalid".parse().unwrap(),
            None,
            Some(&root),
            "",
            100,
            0,
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].body, "different list body");
}
#[tokio::test]
async fn malformed_thread_metadata_does_not_reject_valid_archive_messages() {
    let db = fixture().await;
    verify_malformed_thread_metadata(db).await;
}

async fn verify_malformed_thread_metadata(db: Database) {
    for (id, headers) in [
        ("valid-root", ""),
        (
            "bad-first",
            "References: <invalid> <valid-root@example.invalid>\r\n",
        ),
        (
            "fallback",
            "References: <invalid>\r\nIn-Reply-To: <valid-root@example.invalid>\r\n",
        ),
        (
            "orphan",
            "References: <invalid>\r\nIn-Reply-To: <also-invalid>\r\n",
        ),
    ] {
        let lease = enqueue(&db, "dev.example.invalid", id, headers, id).await;
        listmngr_archive::process(&db, &lease, 101)
            .await
            .expect("thread metadata must not poison an otherwise valid post");
        let state: String = sqlx::query_scalar("SELECT state FROM queue_jobs WHERE id=$1")
            .bind(lease.job.id.0.to_string())
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(state, "done");
    }
    let list = "dev.example.invalid".parse().unwrap();
    let root = listmngr_mail::message_id_hash("valid-root@example.invalid").unwrap();
    let rows = db
        .archive()
        .read(&list, None, Some(&root), "", 100, 0)
        .await
        .unwrap();
    assert_eq!(rows.len(), 3);
    let orphan = listmngr_mail::message_id_hash("orphan@example.invalid").unwrap();
    let rows = db
        .archive()
        .read(&list, None, Some(&orphan), "", 100, 0)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].body, "orphan");
}

#[tokio::test]
async fn references_only_replies_join_the_root_even_when_parent_arrives_late() {
    let db = fixture().await;
    verify_late_thread_parent(db).await;
}

async fn verify_late_thread_parent(db: Database) {
    for (id, headers) in [
        (
            "grandchild-ref",
            "References: <child-ref@example.invalid>\r\n",
        ),
        ("child-ref", "References: <root-ref@example.invalid>\r\n"),
        ("root-ref", ""),
    ] {
        let lease = enqueue(&db, "dev.example.invalid", id, headers, id).await;
        listmngr_archive::process(&db, &lease, 101).await.unwrap();
    }
    let root = listmngr_mail::message_id_hash("root-ref@example.invalid").unwrap();
    let rows = db
        .archive()
        .read(
            &"dev.example.invalid".parse().unwrap(),
            None,
            Some(&root),
            "",
            100,
            0,
        )
        .await
        .unwrap();
    assert_eq!(
        rows.len(),
        3,
        "single References must not become independent roots"
    );
    assert!(rows.iter().any(|row| row.body == "grandchild-ref"));
}

#[tokio::test]
#[ignore = "requires NEW empty disposable ARCHIVE_THREAD_POSTGRES_URL"]
async fn postgres_archive_thread_metadata_matrix() {
    let db = Database::connect(&std::env::var("ARCHIVE_THREAD_POSTGRES_URL").unwrap(), 3)
        .await
        .unwrap();
    let db = seeded_fixture(db).await;
    verify_malformed_thread_metadata(db.clone()).await;
    verify_late_thread_parent(db).await;
}

#[tokio::test]
async fn rollback_index_ack_and_retry_then_never_policy_stores_nothing() {
    let db = fixture().await;
    let list = "dev.example.invalid".parse().unwrap();
    let lease = enqueue(&db, "dev.example.invalid", "rollback", "", "rollback body").await;
    sqlx::query("CREATE TRIGGER sabotage_archive BEFORE INSERT ON audit_log WHEN NEW.action='archive.index' BEGIN SELECT RAISE(ABORT,'fixture'); END").execute(db.pool()).await.unwrap();
    assert!(listmngr_archive::process(&db, &lease, 101).await.is_err());
    assert!(
        db.archive()
            .read(&list, None, None, "", 100, 0)
            .await
            .unwrap()
            .is_empty()
    );
    let state: String = sqlx::query_scalar("SELECT state FROM queue_jobs WHERE id=$1")
        .bind(lease.job.id.0.to_string())
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(state, "leased");
    sqlx::query("DROP TRIGGER sabotage_archive")
        .execute(db.pool())
        .await
        .unwrap();
    listmngr_archive::process(&db, &lease, 102).await.unwrap();
    let lease = enqueue(
        &db,
        "dev.example.invalid",
        "never",
        "",
        "must not be archived",
    )
    .await;
    sqlx::query("UPDATE mailing_lists SET archive_policy='never'")
        .execute(db.pool())
        .await
        .unwrap();
    listmngr_archive::process(&db, &lease, 103).await.unwrap();
    assert!(
        db.archive()
            .read(&list, None, None, "", 100, 0)
            .await
            .is_err()
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM archive_messages")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
}
#[tokio::test]
async fn durable_mime_archive() {
    let db = fixture().await;
    let raw = b"Message-ID: <one@example.invalid>\r\nSubject: =?UTF-8?Q?hello_=C3=A9?=\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\nYm9keSDDqQ==\r\n";
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: raw.to_vec(),
                external_id: "<one@example.invalid>".into(),
                context: r#"{"list_id":"dev.example.invalid"}"#.into(),
                queue: Queue::Archive,
                max_attempts: 3,
            },
            100,
        )
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::Archive, "archive", 100, 1000)
        .await
        .unwrap()
        .unwrap();
    listmngr_archive::process(&db, &lease, 101).await.unwrap();
    let rows = db
        .archive()
        .read(
            &"dev.example.invalid".parse().unwrap(),
            None,
            None,
            "",
            50,
            0,
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].subject, "[dev] hello é");
    assert_eq!(rows[0].body, "body é");
    assert_eq!(
        rows[0].hash,
        listmngr_mail::message_id_hash("<one@example.invalid>").unwrap()
    );
}
