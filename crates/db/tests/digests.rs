use listmngr_core::ListId;
use listmngr_db::{
    Database, NewList,
    digests::{DigestOutput, DigestRecipient},
    mail_queue::{NewMessage, Queue},
};

async fn fixture(url: &str) -> (Database, ListId) {
    let db = Database::connect(url, 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    let list = "list.example.invalid".parse().unwrap();
    db.lists()
        .create(NewList {
            list_id: list,
            display_name: "Test".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    (db, "list.example.invalid".parse().unwrap())
}
async fn collect(db: &Database, list: &ListId, now: i64) {
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: b"Subject: post\r\n\r\nbody".to_vec(),
                external_id: "x".into(),
                context: "{}".into(),
                queue: Queue::Digest,
                max_attempts: 3,
            },
            now,
        )
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::Digest, "test", now, 1000)
        .await
        .unwrap()
        .unwrap();
    db.digests()
        .collect(
            &lease,
            list,
            b"Subject: safe\r\n\r\nbody",
            &[DigestRecipient {
                email: "digest@example.invalid".into(),
                mode: "mime_digests".into(),
            }],
            now,
        )
        .await
        .unwrap();
}
#[allow(clippy::unnecessary_wraps)] // Matches the fallible production renderer contract.
fn render(issue: &listmngr_db::digests::DigestIssue) -> listmngr_core::Result<Vec<DigestOutput>> {
    assert_eq!(issue.posts.len(), 1);
    Ok(vec![DigestOutput {
        raw: b"Subject: immutable digest\r\n\r\nbody".to_vec(),
        recipients: vec!["digest@example.invalid".into()],
        mode: "mime_digests".into(),
    }])
}
#[tokio::test]
async fn durable_flush_is_atomic_and_never_repeats() {
    let dir = std::env::temp_dir().join(uuid::Uuid::now_v7().to_string());
    std::fs::create_dir(&dir).unwrap();
    let url = format!("sqlite://{}?mode=rwc", dir.join("digest.db").display());
    let (db, list) = fixture(&url).await;
    collect(&db, &list, 100).await;
    sqlx::query("CREATE TRIGGER fail_digest BEFORE INSERT ON delivery_recipients BEGIN SELECT RAISE(ABORT, 'rollback'); END").execute(db.pool()).await.unwrap();
    assert!(db.digests().flush(&list, 200, true, render).await.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM digest_posts WHERE issue_id IS NULL")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM digest_issues")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        0
    );
    sqlx::query("DROP TRIGGER fail_digest")
        .execute(db.pool())
        .await
        .unwrap();
    db.pool().close().await;
    let db = Database::connect(&url, 1).await.unwrap();
    let other = Database::connect(&url, 1).await.unwrap();
    let repo = db.digests();
    let other_repo = other.digests();
    let (a, b) = tokio::join!(
        repo.flush(&list, 300, true, render),
        other_repo.flush(&list, 300, true, render)
    );
    other.pool().close().await;
    assert_eq!(a.unwrap() + b.unwrap(), 1);
    assert_eq!(
        db.digests()
            .flush(&list, 400, true, |_| panic!("must not render twice"))
            .await
            .unwrap(),
        0
    );
    let lease = db
        .mail_queue()
        .claim(Queue::Out, "smtp", 400, 1000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        db.mail_queue()
            .pending_recipients(lease.job.id)
            .await
            .unwrap(),
        vec!["digest@example.invalid"]
    );
    assert_eq!(
        db.mail_queue()
            .message(lease.job.message_id)
            .await
            .unwrap()
            .raw,
        b"Subject: immutable digest\r\n\r\nbody"
    );
    db.pool().close().await;
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn moderator_accept_with_empty_regular_roster_schedules_digest_atomically() {
    use listmngr_db::moderation::ReviewAction;
    let (db, list) = fixture("sqlite::memory:").await;
    for review in [false, true] {
        db.mail_queue()
            .enqueue(
                NewMessage {
                    raw: b"Subject: held\r\n\r\nbody".to_vec(),
                    external_id: "held".into(),
                    context: serde_json::json!({"list_id":list}).to_string(),
                    queue: Queue::In,
                    max_attempts: 3,
                },
                100,
            )
            .await
            .unwrap();
        let lease = db
            .mail_queue()
            .claim(Queue::In, "hold", 100, 1000)
            .await
            .unwrap()
            .unwrap();
        let held = db
            .moderation()
            .hold(
                &lease,
                &list,
                "author@example.invalid",
                "held",
                "policy",
                100,
            )
            .await
            .unwrap();
        if review {
            db.moderation()
                .review(
                    held.id,
                    &listmngr_db::AuditContext::system(),
                    &ReviewAction::Accept { max_attempts: 3 },
                    "",
                    200,
                )
                .await
                .unwrap();
        } else {
            db.moderation()
                .accept(held.id, None, &[], 3, 200)
                .await
                .unwrap();
        }
        let digest = db
            .mail_queue()
            .claim(Queue::Digest, "collect", 200, 1000)
            .await
            .unwrap()
            .expect("moderated acceptance must schedule digest even when no regular subscribers");
        assert_eq!(digest.job.message_id, held.message_id);
        db.mail_queue().ack(&digest, 201).await.unwrap();
    }
}

#[tokio::test]
async fn deleting_list_removes_digest_payloads_and_jobs_not_original_submission() {
    let (db, list) = fixture("sqlite::memory:").await;
    collect(&db, &list, 100).await;
    db.digests().flush(&list, 200, true, render).await.unwrap();
    db.lists().delete(&list).await.unwrap();
    for table in [
        "digest_posts",
        "digest_issues",
        "digest_deliveries",
        "delivery_recipients",
    ] {
        assert_eq!(
            sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*) FROM {table}"))
                .fetch_one(db.pool())
                .await
                .unwrap(),
            0,
            "{table}"
        );
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM messages")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM message_blobs")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM queue_jobs WHERE queue='out'")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn periodic_boundary_and_size_threshold_keep_small_young_posts_pending() {
    let (db, list) = fixture("sqlite::memory:").await;
    collect(&db, &list, 100).await;
    assert_eq!(
        db.digests()
            .flush(&list, 86_400_099, false, |_| panic!("not due"))
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        db.digests()
            .flush(&list, 86_400_100, false, render)
            .await
            .unwrap(),
        1
    );
    collect(&db, &list, 86_400_200).await;
    sqlx::query("UPDATE digest_posts SET raw=$1 WHERE issue_id IS NULL")
        .bind(vec![b'x'; 1024 * 1024])
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(
        db.digests()
            .flush(&list, 86_400_201, false, render)
            .await
            .unwrap(),
        1
    );
    assert_eq!(db.lists().get(&list).await.unwrap().next_digest_number, 3);
}
#[tokio::test]
async fn renderer_cannot_silently_omit_or_duplicate_a_digest_subscriber() {
    let (db, list) = fixture("sqlite::memory:").await;
    collect(&db, &list, 100).await;
    assert!(
        db.digests()
            .flush(&list, 200, true, |_| Ok(vec![]))
            .await
            .is_err()
    );
    assert!(
        db.digests()
            .flush(&list, 200, true, |issue| {
                let mut outputs = render(issue)?;
                outputs[0].recipients.push("digest@example.invalid".into());
                Ok(outputs)
            })
            .await
            .is_err()
    );
    assert_eq!(
        db.digests().flush(&list, 201, true, render).await.unwrap(),
        1
    );
}
#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; unique disposable schema only"]
async fn postgres_isolated_digest_publish_rollback_and_single_winner() {
    let url = std::env::var("TEST_POSTGRES_URL").expect("TEST_POSTGRES_URL required");
    assert!(url.starts_with("postgres://") || url.starts_with("postgresql://"));
    assert!(!url.contains("options="));
    let admin = Database::connect(&url, 1).await.unwrap();
    let schema = format!("digests_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(admin.pool())
        .await
        .unwrap();
    let sep = if url.contains('?') { '&' } else { '?' };
    let isolated = format!("{url}{sep}options=-csearch_path%3D{schema}");
    let result=tokio::spawn(async move {
        let (db,list)=fixture(&isolated).await;
        collect(&db,&list,100).await;
        sqlx::query("CREATE FUNCTION fail_digest() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='digest.publish' THEN RAISE EXCEPTION 'fixture rejection'; END IF; RETURN NEW; END $$").execute(db.pool()).await.unwrap();
        sqlx::query("CREATE TRIGGER fail_digest BEFORE INSERT ON audit_log FOR EACH ROW EXECUTE FUNCTION fail_digest()").execute(db.pool()).await.unwrap();
        assert!(db.digests().flush(&list,200,true,render).await.is_err());
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM digest_issues").fetch_one(db.pool()).await.unwrap(),0);
        assert_eq!(db.lists().get(&list).await.unwrap().next_digest_number,1);
        sqlx::query("DROP TRIGGER fail_digest ON audit_log").execute(db.pool()).await.unwrap();
        let other=Database::connect(&isolated,1).await.unwrap();
        let a=db.digests();let b=other.digests();
        let (a,b)=tokio::join!(a.flush(&list,300,true,render),b.flush(&list,300,true,render));assert_eq!(a.unwrap()+b.unwrap(),1);
        db.pool().close().await;other.pool().close().await;
    }).await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(admin.pool())
        .await
        .unwrap();
    admin.pool().close().await;
    result.unwrap();
}
