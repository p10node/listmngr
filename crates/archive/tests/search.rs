//! The search index as a unit: scoping by list, every word required,
//! sender and thread and date filters, replacement on re-add, and the
//! p95 bound over a hundred thousand posts (a manual benchmark).
use listmngr_archive::search::{Document, Query, SearchIndex};
use std::time::Instant;

fn document(
    list: &str,
    hash: &str,
    subject: &str,
    body: &str,
    sender: &str,
    date_ms: i64,
) -> Document {
    Document {
        list: list.into(),
        hash: hash.into(),
        thread: format!("thread-{}", &hash[..1]),
        subject: subject.into(),
        body: body.into(),
        sender_name: sender.split('@').next().unwrap_or_default().into(),
        sender_email: sender.into(),
        date_ms,
    }
}

const fn query<'a>(list: &'a str, text: &'a str) -> Query<'a> {
    Query {
        list,
        text,
        thread: None,
        since_ms: None,
        until_ms: None,
        limit: 20,
        offset: 0,
    }
}

fn seeded() -> (tempfile::TempDir, SearchIndex) {
    let dir = tempfile::tempdir().unwrap();
    let index = SearchIndex::open(dir.path()).unwrap();
    let mut writer = index.writer().unwrap();
    for doc in [
        document(
            "dev.example.org",
            "a1",
            "Release plan",
            "The release ships on Friday with the installer.",
            "alice@example.org",
            1_000,
        ),
        document(
            "dev.example.org",
            "a2",
            "Re: Release plan",
            "Friday works; the installer needs one more test.",
            "bob@example.org",
            2_000,
        ),
        document(
            "dev.example.org",
            "b3",
            "Lunch",
            "Sandwiches at noon.",
            "carol@example.org",
            3_000,
        ),
        document(
            "ops.example.org",
            "c4",
            "Release plan",
            "Ops copies the release to the mirrors on Friday.",
            "dave@example.org",
            4_000,
        ),
    ] {
        writer.add(&doc).unwrap();
    }
    writer.commit().unwrap();
    index.reload().unwrap();
    (dir, index)
}

#[test]
fn searches_are_scoped_to_a_list_and_require_every_word() {
    let (_dir, index) = seeded();
    let hits = index.search(&query("dev.example.org", "release")).unwrap();
    assert_eq!(hits.total, 2);
    let hashes: Vec<&str> = hits.hits.iter().map(|h| h.hash.as_str()).collect();
    assert!(
        hashes.contains(&"a1") && hashes.contains(&"a2"),
        "{hashes:?}"
    );
    assert!(
        index
            .search(&query("ops.example.org", "release"))
            .unwrap()
            .hits
            .iter()
            .all(|h| h.hash == "c4")
    );
    assert_eq!(
        index
            .search(&query("dev.example.org", "release installer test"))
            .unwrap()
            .total,
        1
    );
    assert_eq!(
        index
            .search(&query("dev.example.org", "sandwiches"))
            .unwrap()
            .hits[0]
            .hash,
        "b3"
    );
    assert_eq!(
        index
            .search(&query("dev.example.org", "mirrors"))
            .unwrap()
            .total,
        0,
        "another list's post"
    );
    assert_eq!(
        index
            .search(&query("dev.example.org", "alice"))
            .unwrap()
            .hits[0]
            .hash,
        "a1",
        "the sender matches"
    );
    assert!(
        index
            .search(&query("dev.example.org", "release AND OR ) ("))
            .is_ok(),
        "punctuation is not a syntax error"
    );
    assert!(index.search(&query("dev.example.org", "   ")).is_err());
    let hit = &index
        .search(&query("dev.example.org", "lunch"))
        .unwrap()
        .hits[0];
    assert_eq!(
        (hit.subject.as_str(), hit.thread.as_str(), hit.date_ms),
        ("Lunch", "thread-b", 3_000)
    );
}

#[test]
fn thread_and_date_filters_and_paging() {
    let (_dir, index) = seeded();
    let mut q = query("dev.example.org", "friday");
    q.thread = Some("thread-a");
    assert_eq!(index.search(&q).unwrap().total, 2);
    q.thread = Some("thread-b");
    assert_eq!(index.search(&q).unwrap().total, 0);
    let mut q = query("dev.example.org", "friday");
    q.since_ms = Some(1_500);
    assert_eq!(index.search(&q).unwrap().hits[0].hash, "a2");
    q.since_ms = None;
    q.until_ms = Some(1_500);
    assert_eq!(index.search(&q).unwrap().hits[0].hash, "a1");
    let mut q = query("dev.example.org", "friday");
    q.limit = 1;
    let first = index.search(&q).unwrap();
    assert_eq!((first.hits.len(), first.total), (1, 2));
    q.offset = 1;
    let second = index.search(&q).unwrap();
    assert_eq!(second.hits.len(), 1);
    assert_ne!(first.hits[0].hash, second.hits[0].hash);
}

#[test]
fn re_adding_a_post_replaces_it_and_removals_apply() {
    let (dir, index) = seeded();
    let mut writer = index.writer().unwrap();
    writer
        .add(&document(
            "dev.example.org",
            "a1",
            "Release plan",
            "The release slipped to Monday.",
            "alice@example.org",
            1_000,
        ))
        .unwrap();
    writer.commit().unwrap();
    index.reload().unwrap();
    assert_eq!(
        index
            .search(&query("dev.example.org", "friday"))
            .unwrap()
            .total,
        1
    );
    assert_eq!(
        index
            .search(&query("dev.example.org", "monday"))
            .unwrap()
            .total,
        1
    );
    assert_eq!(index.len().unwrap(), 4, "replaced, not duplicated");
    writer.remove("dev.example.org", "b3");
    writer.remove_list("ops.example.org");
    writer.commit().unwrap();
    index.reload().unwrap();
    assert_eq!(index.len().unwrap(), 2);
    assert_eq!(
        index
            .search(&query("ops.example.org", "release"))
            .unwrap()
            .total,
        0
    );
    drop(writer);
    // The index survives reopening, and a second writer is refused while one is held.
    let again = SearchIndex::open(dir.path()).unwrap();
    assert_eq!(again.len().unwrap(), 2);
    let held = again.writer().unwrap();
    assert!(again.writer().is_err());
    drop(held);
}

#[test]
fn commits_batch_by_count_and_age() {
    let (_dir, index) = seeded();
    let mut writer = index.writer().unwrap();
    writer
        .add(&document(
            "dev.example.org",
            "z9",
            "Late",
            "late post",
            "z@example.org",
            9,
        ))
        .unwrap();
    assert!(
        !writer
            .commit_if_due(100, std::time::Duration::from_secs(60))
            .unwrap()
    );
    assert_eq!(writer.pending(), 1);
    assert!(
        writer
            .commit_if_due(1, std::time::Duration::from_secs(60))
            .unwrap()
    );
    assert_eq!(writer.pending(), 0);
    index.reload().unwrap();
    assert_eq!(
        index
            .search(&query("dev.example.org", "late"))
            .unwrap()
            .total,
        1
    );
}

/// The plan's bound: p95 under 100 ms over a hundred thousand posts.
#[test]
#[ignore = "benchmark: indexes 100k posts; run by hand and record the numbers"]
fn search_p95_is_under_100ms_over_100k_posts() {
    let dir = tempfile::tempdir().unwrap();
    let index = SearchIndex::open(dir.path()).unwrap();
    let words = [
        "release",
        "installer",
        "friday",
        "mirror",
        "sandwich",
        "meeting",
        "budget",
        "kernel",
        "patch",
        "regression",
        "outage",
        "schedule",
        "invoice",
        "review",
        "deploy",
        "backup",
    ];
    let mut writer = index.writer().unwrap();
    let started = Instant::now();
    for i in 0..100_000_u64 {
        let a = words[(i % 16) as usize];
        let b = words[((i / 16) % 16) as usize];
        let c = words[((i / 256) % 16) as usize];
        let list = if i % 4 == 0 {
            "ops.example.org"
        } else {
            "dev.example.org"
        };
        writer
            .add(&document(
                list,
                &format!("h{i}"),
                &format!("{a} {b} {i}"),
                &format!(
                    "Post {i} about the {a} and the {b}, also the {c}. {}",
                    "filler text ".repeat(20)
                ),
                &format!("user{}@example.org", i % 1000),
                i64::try_from(i).unwrap_or(i64::MAX).saturating_mul(1000),
            ))
            .unwrap();
        if i % 5000 == 4999 {
            writer.commit().unwrap();
        }
    }
    writer.commit().unwrap();
    index.reload().unwrap();
    let indexed = started.elapsed();
    let mut samples = Vec::with_capacity(200);
    for i in 0..200_usize {
        let text = format!("{} {}", words[i % 16], words[(i * 7) % 16]);
        let t = Instant::now();
        let results = index.search(&query("dev.example.org", &text)).unwrap();
        samples.push(t.elapsed());
        assert!(results.total > 0, "{text}");
    }
    samples.sort();
    let p95 = samples[189];
    let p50 = samples[99];
    println!(
        "SEARCH BENCH: indexed 100000 posts in {indexed:?}; 200 queries p50={p50:?} p95={p95:?} max={:?}",
        samples[199]
    );
    assert!(p95 < std::time::Duration::from_millis(100), "p95 {p95:?}");
}
