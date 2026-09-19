//! Phase 5 acceptance against a real `HyperKitty`: the Message-ID-Hash and
//! the threading of one month of a public list, as `HyperKitty` itself
//! reports them, compared with what this archive computes from the same
//! messages.
//!
//! The fixture holds, for every message of `mailman-users@mailman3.org`
//! in March 2025, the reference headers from `HyperKitty`'s own mbox export
//! (`Message-ID`, `In-Reply-To`, `References`, `Date`) and `HyperKitty`'s
//! answers from its REST API for that message (`message_id_hash`, the
//! thread it filed it under, the parent it chose). No body, name or
//! address is kept. The API record was fetched by the hash this archive
//! computed, so a record coming back at all is already the hash agreeing;
//! the test checks the fields as well.
use listmngr_archive::mbox::{Reader, import, write_message};
use listmngr_db::{Database, NewList};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Deserialize)]
struct Fixture {
    #[allow(dead_code)]
    source: String,
    messages: Vec<Message>,
}

#[derive(Deserialize)]
struct Message {
    #[serde(rename = "message_id")]
    id: String,
    in_reply_to: Vec<String>,
    references: Vec<String>,
    date: String,
    hyperkitty: HyperKitty,
}

#[derive(Deserialize)]
struct HyperKitty {
    hash: String,
    thread: String,
    parent: Option<String>,
}

fn fixture() -> Fixture {
    serde_json::from_str(include_str!(
        "fixtures/hyperkitty/mailman-users-2025-03.json"
    ))
    .unwrap()
}

#[test]
fn every_message_id_hash_agrees_with_hyperkitty() {
    let fixture = fixture();
    assert_eq!(fixture.messages.len(), 138);
    for message in &fixture.messages {
        let ours = listmngr_mail::message_id_hash(&message.id).unwrap();
        assert_eq!(
            ours, message.hyperkitty.hash,
            "Message-ID-Hash of {}",
            message.id
        );
    }
}

/// The month as an mbox, in `HyperKitty`'s export order, with a stub body
/// and sender so the archive stores it.
fn month_as_mbox(fixture: &Fixture) -> Vec<u8> {
    use std::fmt::Write as _;
    let mut out = Vec::new();
    for message in &fixture.messages {
        let mut raw = format!(
            "Message-ID: {}\r\nDate: {}\r\nFrom: Someone <someone@example.invalid>\r\nSubject: fixture\r\n",
            message.id, message.date
        );
        if let Some(first) = message.in_reply_to.first() {
            write!(raw, "In-Reply-To: {first}\r\n").unwrap();
        }
        if !message.references.is_empty() {
            write!(raw, "References: {}\r\n", message.references.join(" ")).unwrap();
        }
        raw.push_str("Content-Type: text/plain\r\n\r\n(body withheld)\r\n");
        write_message(&mut out, raw.as_bytes()).unwrap();
    }
    out
}

async fn archive_with_the_month() -> (Database, Vec<(String, String, Option<String>)>) {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: "users.example.invalid".parse().unwrap(),
            display_name: "users".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    db.lists()
        .update(&list.id, &serde_json::json!({"archive_policy": "public"}))
        .await
        .unwrap();
    let fixture = fixture();
    let mbox = month_as_mbox(&fixture);
    let outcome = import(
        &db,
        &list.id,
        Reader::new(std::io::Cursor::new(&mbox[..])),
        500,
        1_000,
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(outcome.imported, 138, "{outcome:?}");
    // What the reader sees: the parent only when it is itself archived.
    let mut seen = Vec::new();
    for message in db
        .archive()
        .read_browser(&list.id, None, None, "", 500, 0)
        .await
        .unwrap()
    {
        seen.push((message.hash, message.thread, message.parent));
    }
    (db, seen)
}

#[tokio::test]
async fn threading_agrees_with_hyperkitty_after_a_real_import() {
    let fixture = fixture();
    let expected: BTreeMap<&str, &HyperKitty> = fixture
        .messages
        .iter()
        .map(|m| (m.hyperkitty.hash.as_str(), &m.hyperkitty))
        .collect();
    let archived: BTreeSet<&str> = expected.keys().copied().collect();
    let (_db, seen) = archive_with_the_month().await;
    assert_eq!(seen.len(), 138);
    let mut parent_differences = Vec::new();
    let mut parents_outside_the_month = 0;
    let mut thread_differences = Vec::new();
    for (hash, thread, parent) in &seen {
        let hk = expected[hash.as_str()];
        if parent.as_deref() != hk.parent.as_deref() {
            // HyperKitty holds the whole archive, so a reply to February
            // has a parent there and none in a month imported alone.
            match &hk.parent {
                Some(outside) if parent.is_none() && !archived.contains(outside.as_str()) => {
                    parents_outside_the_month += 1;
                }
                _ => parent_differences.push((hash.clone(), parent.clone(), hk.parent.clone())),
            }
        }
        if *thread != hk.thread {
            thread_differences.push((hash.clone(), thread.clone(), hk.thread.clone()));
        }
    }
    eprintln!(
        "parents: {} agree, {} with the HyperKitty parent outside the month",
        138 - parents_outside_the_month - parent_differences.len(),
        parents_outside_the_month
    );
    assert!(
        parent_differences.is_empty(),
        "parents differ from HyperKitty within the month: {parent_differences:?}"
    );
    // HyperKitty files a reply to February under a root it holds and this
    // month alone cannot; such a reply starts its own thread here. Every
    // thread whose HyperKitty root is in the month must agree.
    let unexplained: Vec<_> = thread_differences
        .iter()
        .filter(|(_, _, hk)| archived.contains(hk.as_str()))
        .collect();
    eprintln!(
        "threads: {} agree, {} differ ({} with the HyperKitty root outside the month)",
        138 - thread_differences.len(),
        thread_differences.len(),
        thread_differences.len() - unexplained.len()
    );
    assert!(
        unexplained.is_empty(),
        "threads differ from HyperKitty within the month: {unexplained:?}"
    );
}
