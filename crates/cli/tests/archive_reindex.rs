//! `listmngr archive reindex`: the search index rebuilt from every archived
//! post into `[archive] index_path` or `--index`.
use assert_cmd::Command;
use listmngr_archive::search::{Query, SearchIndex};
use listmngr_db::mail_queue::{NewMessage, Queue};
use listmngr_db::{Database, NewList};
use predicates::prelude::*;
use std::path::Path;

fn command(root: &Path, url: &str) -> Command {
    let mut command = Command::cargo_bin("listmngr").unwrap();
    command
        .env_clear()
        .current_dir(root)
        .env("LISTMNGR__DATABASE__URL", url);
    command
}

fn setup(root: &Path) -> String {
    let url = format!("sqlite://{}?mode=rwc", root.join("fixture.db").display());
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let db = Database::connect(&url, 1).await.unwrap();
        db.migrate().await.unwrap();
        db.domains()
            .create("example.invalid", "", None)
            .await
            .unwrap();
        db.lists()
            .create(NewList {
                list_id: "dev.example.invalid".parse().unwrap(),
                display_name: "Dev".into(),
                style: "legacy-default".into(),
            })
            .await
            .unwrap();
        for (id, subject, body) in [
            ("<one@example.invalid>", "Release plan", "The release ships on Friday."),
            ("<two@example.invalid>", "Lunch", "Sandwiches at noon."),
        ] {
            db.mail_queue()
                .enqueue(
                    NewMessage {
                        raw: format!("Message-ID: {id}\r\nFrom: A <a@example.invalid>\r\nSubject: {subject}\r\nContent-Type: text/plain\r\n\r\n{body}\r\n").into_bytes(),
                        external_id: id.into(),
                        context: serde_json::json!({"version":1,"list_id":"dev.example.invalid"}).to_string(),
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
        }
    });
    url
}

#[test]
fn reindex_builds_the_index_from_the_archive() {
    let root = tempfile::tempdir().unwrap();
    let url = setup(root.path());
    let index_dir = root.path().join("search-index");
    command(root.path(), &url)
        .args(["archive", "reindex", "--index"])
        .arg(&index_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("indexed 2 messages into"));
    let index = SearchIndex::open(&index_dir).unwrap();
    assert_eq!(index.len().unwrap(), 2);
    let hits = index
        .search(&Query {
            list: "dev.example.invalid",
            text: "friday release",
            thread: None,
            since_ms: None,
            until_ms: None,
            limit: 10,
            offset: 0,
        })
        .unwrap();
    assert_eq!(hits.total, 1);
    assert_eq!(
        hits.hits[0].subject, "[dev] Release plan",
        "the cooked subject"
    );
    drop(index);
    // A second run rebuilds rather than duplicates.
    command(root.path(), &url)
        .args(["archive", "reindex", "--index"])
        .arg(&index_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("indexed 2 messages"));
    assert_eq!(SearchIndex::open(&index_dir).unwrap().len().unwrap(), 2);
    // The configured path is the default.
    let configured = root.path().join("configured-index");
    command(root.path(), &url)
        .env(
            "LISTMNGR__ARCHIVE__INDEX_PATH",
            configured.to_str().unwrap(),
        )
        .args(["archive", "reindex"])
        .assert()
        .success()
        .stdout(predicate::str::contains(configured.to_str().unwrap()));
    assert!(SearchIndex::exists(&configured));
}
