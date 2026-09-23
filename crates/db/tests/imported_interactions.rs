//! What a reader left on another site's archive — votes, tags, a
//! category, a favourite — written onto this one's archive.
use listmngr_core::{ListId, MemberRole, SubscriptionMode};
use listmngr_db::archive::import::ImportItem;
use listmngr_db::{
    AuditContext, Database, ImportedAddress, ImportedInteractions, ImportedUser, NewList, NewMember,
};

const NOW: i64 = 1_700_000_000_000;

async fn fixture(db: &Database) -> ListId {
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    let list: ListId = "rust-users.example.invalid".parse().unwrap();
    db.lists()
        .create(NewList {
            list_id: list.clone(),
            display_name: "Rust".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    db.lists()
        .update(&list, &serde_json::json!({"archive_policy": "public"}))
        .await
        .unwrap();
    // Two archived posts of one thread, and one of another.
    let item = |hash: &str, thread: &str, subject: &str| ImportItem {
        hash: hash.into(),
        thread: thread.into(),
        parent: None,
        subject: subject.into(),
        body: "body".into(),
        sender_name: String::new(),
        sender_email: "alice@example.invalid".into(),
        date_ms: Some(NOW),
        created_at: NOW,
        attachments: Vec::new(),
        raw: b"From: alice@example.invalid\r\n\r\nbody\r\n".to_vec(),
    };
    db.archive()
        .import_batch(
            &list,
            &[
                item("ROOT1", "ROOT1", "Hello archive"),
                item("REPLY1", "ROOT1", "Re: Hello archive"),
                item("ROOT2", "ROOT2", "Another thread"),
            ],
            NOW,
        )
        .await
        .unwrap();
    // A reader with an account, and a member with none.
    db.users()
        .create_imported_with_context(
            ImportedUser {
                display_name: "Alice".into(),
                is_server_owner: false,
                locale: "en".into(),
                addresses: vec![ImportedAddress {
                    email: "alice@example.invalid".into(),
                    display_name: "Alice".into(),
                    verified: true,
                }],
                preferred: None,
            },
            &AuditContext::system(),
        )
        .await
        .unwrap();
    db.members()
        .subscribe_with_context(
            NewMember {
                list_id: list.clone(),
                email: "bob@example.invalid".into(),
                role: MemberRole::Member,
                subscription_mode: SubscriptionMode::AsAddress,
                display_name: String::new(),
            },
            true,
            &AuditContext::system(),
        )
        .await
        .unwrap();
    list
}

fn interactions(list: &ListId) -> ImportedInteractions<'_> {
    ImportedInteractions {
        list,
        votes: vec![
            ("ROOT1".into(), "alice@example.invalid".into(), 1),
            ("ROOT1".into(), "bob@example.invalid".into(), -1),
            ("MISSING".into(), "alice@example.invalid".into(), 1),
            ("ROOT2".into(), "nobody@example.invalid".into(), 1),
        ],
        tags: vec![
            (
                "ROOT1".into(),
                "release".into(),
                "alice@example.invalid".into(),
            ),
            ("ROOT1".into(), "bug".into(), "bob@example.invalid".into()),
            (
                "MISSING".into(),
                "ghost".into(),
                "alice@example.invalid".into(),
            ),
        ],
        categories: vec![
            ("ROOT1".into(), "announcements".into()),
            ("MISSING".into(), "announcements".into()),
        ],
        favorites: vec![
            ("ROOT2".into(), "alice@example.invalid".into()),
            ("ROOT2".into(), "nobody@example.invalid".into()),
        ],
        at: NOW,
    }
}

async fn scenario(db: &Database) {
    let list = fixture(db).await;
    let report = db
        .archive()
        .import_interactions(&interactions(&list), &AuditContext::system())
        .await
        .unwrap();
    assert_eq!(report.votes, 1, "{report:?}");
    assert_eq!(report.tags, 1);
    assert_eq!(report.categories, 1);
    assert_eq!(report.favorites, 1);
    // What could not be placed is counted, not written: a post this
    // archive does not have, and a reader with no account here (Bob is
    // a bare subscriber, so his vote and his tag stay behind).
    assert_eq!(report.skipped, 7);
    let meta = db
        .archive()
        .browser_thread_meta(&list, None, "ROOT1")
        .await
        .unwrap();
    let tags = meta
        .tags
        .iter()
        .map(|tag| tag.tag.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        tags,
        ["release"],
        "Bob has no account, so his tag stays behind"
    );
    assert_eq!(meta.category.as_deref(), Some("announcements"));
    let votes = db
        .archive()
        .browser_votes(&list, None, &["ROOT1".to_owned()])
        .await
        .unwrap();
    assert_eq!(votes.len(), 1);
    assert_eq!(votes[0].hash, "ROOT1");
    assert_eq!(votes[0].score, 1, "only the reader with an account");
    assert_eq!(
        db.archive().categories(&list).await.unwrap(),
        ["announcements"]
    );
    // The category the archive knows is listed once, and the favourite
    // belongs to the reader who has an account here.
    let favorites: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM archive_favorites")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(favorites, 1);
    // One audit event carries the report, and nothing was mailed.
    let diff: String =
        sqlx::query_scalar("SELECT diff FROM audit_log WHERE action='archive.import_interactions'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(diff.contains("\"votes\":1"), "{diff}");
    let queued: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(queued, 0);
    rerun(db, &list).await;
}

/// Running it again writes the same thing, not more.
async fn rerun(db: &Database, list: &ListId) {
    let again = db
        .archive()
        .import_interactions(&interactions(list), &AuditContext::system())
        .await
        .unwrap();
    assert_eq!(again.votes, 1, "{again:?}");
    let votes: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM archive_votes")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(votes, 1);
    let tags: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM archive_tags")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(tags, 1);
}

#[tokio::test]
async fn imported_interactions_land_on_the_posts_this_archive_has() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    scenario(&db).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_imported_interactions_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("imported_interactions")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 2).await.unwrap();
    scenario(&db).await;
    db.pool().close().await;
    schema.drop().await.unwrap();
}
