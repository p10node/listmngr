//! `HyperKitty`'s archive read from its own database and written onto
//! this one.
//!
//! `fixtures/hyperkitty/hyperkitty.db` is a real `HyperKitty` 1.3.12
//! database, built by its own Django migrations and filled through its
//! own models (`tests/compat/generate_hyperkitty.py`): three posts in
//! two threads, three votes, two tags, a category and a favourite.
use listmngr_core::{ListId, MemberRole, SubscriptionMode};
use listmngr_db::archive::import::ImportItem;
use listmngr_db::{AuditContext, Database, ImportedAddress, ImportedUser, NewList, NewMember};
use listmngr_import::hyperkitty::{apply, fetch};
use std::path::PathBuf;

const NOW: i64 = 1_700_000_000_000;
/// The Message-ID-Hashes `HyperKitty` gave the fixture's posts.
const ROOT1: &str = "WYKGK4F2CNJFZTD2CVSSNJYJ3EP4JHZU";
const REPLY1: &str = "L33UPQU2GXSBPUQOMJYW2I7F77HOTD7Z";
const ROOT2: &str = "2MFWINCCDIFKOCUBTRMVNENN437Z53YU";

fn database_url() -> String {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hyperkitty/hyperkitty.db");
    format!("sqlite://{}?mode=ro", path.display())
}

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
    // The posts as the mbox import brings them, under the same hashes.
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
                item(ROOT1, ROOT1, "Hello archive"),
                item(REPLY1, ROOT1, "Re: Hello archive"),
                item(ROOT2, ROOT2, "Another thread"),
            ],
            NOW,
        )
        .await
        .unwrap();
    for email in ["alice@example.invalid", "bob@example.invalid"] {
        db.users()
            .create_imported_with_context(
                ImportedUser {
                    display_name: String::new(),
                    is_server_owner: false,
                    locale: "en".into(),
                    addresses: vec![ImportedAddress {
                        email: email.into(),
                        display_name: String::new(),
                        verified: true,
                    }],
                    preferred: None,
                },
                &AuditContext::system(),
            )
            .await
            .unwrap();
    }
    db.members()
        .subscribe_with_context(
            NewMember {
                list_id: list.clone(),
                email: "carol@elsewhere.invalid".into(),
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

#[tokio::test]
async fn a_real_hyperkitty_archive_reads_into_its_interactions() {
    let archives = fetch(&database_url(), None).await.unwrap();
    assert_fixture(&archives);
    // One list only, and a list `HyperKitty` does not archive is nothing.
    let only: ListId = "rust-users.example.invalid".parse().unwrap();
    assert_eq!(fetch(&database_url(), Some(&only)).await.unwrap().len(), 1);
    let other: ListId = "announce.other.invalid".parse().unwrap();
    assert!(
        fetch(&database_url(), Some(&other))
            .await
            .unwrap()
            .is_empty()
    );
}

/// The same archive built by the same script into `HyperKitty` on
/// PostgreSQL, whose integer and small-integer columns are typed.
#[tokio::test]
#[ignore = "requires HYPERKITTY_DB_URL (a disposable HyperKitty database on PostgreSQL)"]
async fn hyperkitty_reads_a_real_database() {
    let url = std::env::var("HYPERKITTY_DB_URL").expect("HYPERKITTY_DB_URL");
    assert_fixture(&fetch(&url, None).await.unwrap());
}

fn assert_fixture(archives: &[listmngr_import::hyperkitty::Archive]) {
    assert_eq!(archives.len(), 1);
    let archive = &archives[0];
    assert_eq!(archive.list_id.as_str(), "rust-users.example.invalid");
    assert_eq!(archive.messages, 3);
    assert_eq!(archive.threads, 2);
    // HyperKitty's own Message-ID-Hash is the hash this archive uses.
    let mut votes = archive.votes.clone();
    votes.sort();
    assert_eq!(
        votes,
        vec![
            (ROOT2.to_owned(), "alice@example.invalid".to_owned(), 1),
            (ROOT1.to_owned(), "alice@example.invalid".to_owned(), 1),
            (ROOT1.to_owned(), "bob@example.invalid".to_owned(), -1),
        ]
    );
    let mut tags = archive.tags.clone();
    tags.sort();
    assert_eq!(
        tags,
        vec![
            (
                ROOT1.to_owned(),
                "bug".to_owned(),
                "bob@example.invalid".to_owned()
            ),
            (
                ROOT1.to_owned(),
                "release".to_owned(),
                "alice@example.invalid".to_owned()
            ),
        ]
    );
    assert_eq!(
        archive.categories,
        vec![(ROOT1.to_owned(), "announcements".to_owned())]
    );
    assert_eq!(
        archive.favorites,
        vec![(ROOT2.to_owned(), "alice@example.invalid".to_owned())]
    );
}

#[tokio::test]
async fn an_unreadable_hyperkitty_database_is_an_error() {
    let error = fetch("sqlite:///nonexistent/hyperkitty.db?mode=ro", None)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("database"), "{error}");
    let error = fetch(
        "postgres://hyperkitty:hunter2hunter2@127.0.0.1:1/hyperkitty",
        None,
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(!error.contains("hunter2hunter2"), "{error}");
}

async fn scenario(db: &Database) {
    let list = fixture(db).await;
    let archive = fetch(&database_url(), Some(&list)).await.unwrap().remove(0);
    let report = apply(db, &archive, &AuditContext::system(), NOW)
        .await
        .unwrap();
    assert_eq!(report.votes, 3, "{report:?}");
    assert_eq!(report.tags, 2);
    assert_eq!(report.categories, 1);
    assert_eq!(report.favorites, 1);
    assert_eq!(report.skipped, 0);
    let meta = db
        .archive()
        .browser_thread_meta(&list, None, ROOT1)
        .await
        .unwrap();
    let mut tags = meta
        .tags
        .iter()
        .map(|tag| tag.tag.clone())
        .collect::<Vec<_>>();
    tags.sort();
    assert_eq!(tags, ["bug", "release"]);
    assert_eq!(meta.category.as_deref(), Some("announcements"));
    let votes = db
        .archive()
        .browser_votes(&list, None, &[ROOT1.to_owned(), ROOT2.to_owned()])
        .await
        .unwrap();
    let score = |hash: &str| {
        votes
            .iter()
            .find(|vote| vote.hash == hash)
            .map(|vote| vote.score)
            .unwrap()
    };
    assert_eq!(score(ROOT1), 0, "one up and one down");
    assert_eq!(score(ROOT2), 1);
    let favorites: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM archive_favorites")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(favorites, 1);
    // Nothing was mailed, and the import is audited once.
    let queued: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(queued, 0);
    let audited: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log WHERE action='archive.import_interactions'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(audited, 1);
}

#[tokio::test]
async fn a_real_hyperkitty_archive_applies_on_sqlite() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    scenario(&db).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_hyperkitty_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("hyperkitty")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 2).await.unwrap();
    scenario(&db).await;
    db.pool().close().await;
    schema.drop().await.unwrap();
}
