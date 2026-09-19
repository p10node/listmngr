//! `listmngr archive import` and `archive export`: an mbox into a list's
//! archive with threads, attachments and generated ids, skipped on a
//! second run; the archive back out whole, by thread or by month, plain
//! or gzipped; a list without an archive refuses; and the plan's bound of
//! a hundred thousand posts in under ten minutes (a manual benchmark).
use assert_cmd::Command;
use listmngr_archive::mbox::{Reader, write_message};
use listmngr_db::{Database, NewList};
use predicates::prelude::*;
use std::io::{Read as _, Write as _};
use std::path::Path;

fn command(root: &Path, url: &str) -> Command {
    let mut command = Command::cargo_bin("listmngr").unwrap();
    command
        .env_clear()
        .current_dir(root)
        .env("LISTMNGR__DATABASE__URL", url);
    command
}

async fn setup_db(url: &str) -> Database {
    let db = Database::connect(url, 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    for (name, policy) in [("dev", "public"), ("quiet", "never")] {
        let list = db
            .lists()
            .create(NewList {
                list_id: format!("{name}.example.invalid").parse().unwrap(),
                display_name: name.into(),
                style: "legacy-default".into(),
            })
            .await
            .unwrap();
        db.lists()
            .update(&list.id, &serde_json::json!({"archive_policy": policy}))
            .await
            .unwrap();
    }
    db
}

fn setup(root: &Path) -> String {
    let url = format!("sqlite://{}?mode=rwc", root.join("fixture.db").display());
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(async { drop(setup_db(&url).await) });
    url
}

/// Four messages: a root and a reply in January, a February post with an
/// attachment, and one without a Message-ID.
fn fixture_mbox() -> Vec<u8> {
    let mut out = Vec::new();
    write_message(&mut out, b"Message-ID: <a1@example.invalid>\r\nFrom: Alice <alice@example.invalid>\r\nDate: Mon, 01 Jan 2024 10:00:00 +0000\r\nSubject: Kickoff\r\nContent-Type: text/plain\r\n\r\nFrom the start.\r\n").unwrap();
    write_message(&mut out, b"Message-ID: <a2@example.invalid>\r\nIn-Reply-To: <a1@example.invalid>\r\nFrom: Bob <bob@example.invalid>\r\nDate: Mon, 01 Jan 2024 11:00:00 +0000\r\nSubject: Re: Kickoff\r\nContent-Type: text/plain\r\n\r\nAgreed.\r\n").unwrap();
    write_message(&mut out, b"Message-ID: <b1@example.invalid>\r\nFrom: Carol <carol@example.invalid>\r\nDate: Thu, 15 Feb 2024 09:00:00 +0000\r\nSubject: Notes\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=parts\r\n\r\n--parts\r\nContent-Type: text/plain\r\n\r\nSee attached.\r\n--parts\r\nContent-Type: text/plain; name=notes.txt\r\nContent-Disposition: attachment; filename=notes.txt\r\n\r\nthe notes\r\n--parts--\r\n").unwrap();
    write_message(&mut out, b"From: Dave <dave@example.invalid>\r\nDate: Fri, 01 Mar 2024 08:00:00 +0000\r\nSubject: No id\r\nContent-Type: text/plain\r\n\r\nWithout a Message-ID.\r\n").unwrap();
    out
}

fn count_messages(bytes: &[u8]) -> usize {
    Reader::new(std::io::Cursor::new(bytes)).count()
}

fn gunzip(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .read_to_end(&mut out)
        .unwrap();
    out
}

#[test]
fn import_stores_threads_attachments_and_generated_ids_then_skips_repeats() {
    let root = tempfile::tempdir().unwrap();
    let url = setup(root.path());
    let mbox = root.path().join("archive.mbox");
    std::fs::write(&mbox, fixture_mbox()).unwrap();
    command(root.path(), &url)
        .args(["archive", "import", "dev.example.invalid"])
        .arg(&mbox)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "imported 4 messages into dev.example.invalid (0 skipped)",
        ));
    command(root.path(), &url)
        .args(["archive", "import", "dev.example.invalid"])
        .arg(&mbox)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "imported 0 messages into dev.example.invalid (4 skipped)",
        ));
    // A gzipped mbox reads the same; a list without an archive refuses.
    let gz = root.path().join("archive.mbox.gz");
    let mut encoder = flate2::write::GzEncoder::new(
        std::fs::File::create(&gz).unwrap(),
        flate2::Compression::default(),
    );
    encoder.write_all(&fixture_mbox()).unwrap();
    encoder.finish().unwrap();
    command(root.path(), &url)
        .args(["archive", "import", "dev.example.invalid"])
        .arg(&gz)
        .assert()
        .success()
        .stdout(predicate::str::contains("(4 skipped)"));
    command(root.path(), &url)
        .args(["archive", "import", "quiet.example.invalid"])
        .arg(&mbox)
        .assert()
        .failure()
        .stderr(predicate::str::contains("CLI-VALIDATION"));
    // The policy is read before the file, so an mbox holding nothing the
    // importer can store still refuses rather than reporting success.
    let empty = root.path().join("empty.mbox");
    std::fs::write(&empty, b"").unwrap();
    command(root.path(), &url)
        .args(["archive", "import", "quiet.example.invalid"])
        .arg(&empty)
        .assert()
        .failure()
        .stderr(predicate::str::contains("CLI-VALIDATION"));
    command(root.path(), &url)
        .args(["archive", "import", "absent.example.invalid"])
        .arg(&mbox)
        .assert()
        .failure()
        .stderr(predicate::str::contains("CLI-NOT-FOUND"));
    assert_stored(&url);
}

/// What the three imports of the same four messages left behind.
fn assert_stored(url: &str) {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let db = Database::connect(url, 1).await.unwrap();
        let list = "dev.example.invalid".parse().unwrap();
        let root_hash = listmngr_mail::message_id_hash("<a1@example.invalid>").unwrap();
        let thread = db
            .archive()
            .read_browser_thread(&list, None, &root_hash)
            .await
            .unwrap();
        assert_eq!(thread.len(), 2, "the reply joined its root's thread");
        // An imported post is read back through the same publication as an
        // archived one, so the list's subject prefix is on it.
        let reply = thread
            .iter()
            .find(|m| m.subject == "[dev] Re: Kickoff")
            .unwrap();
        assert_eq!(reply.parent.as_deref(), Some(root_hash.as_str()));
        assert_eq!(reply.sender_email, "bob@example.invalid");
        let notes_hash = listmngr_mail::message_id_hash("<b1@example.invalid>").unwrap();
        let notes = db
            .archive()
            .read_browser_message(&list, None, &notes_hash)
            .await
            .unwrap();
        assert_eq!(notes.attachments.len(), 1);
        assert_eq!(notes.attachments[0].filename, "notes.txt");
        let all = db
            .archive()
            .read_browser(&list, None, None, "", 20, 0)
            .await
            .unwrap();
        assert_eq!(all.len(), 4);
        let generated = all.iter().find(|m| m.subject == "[dev] No id").unwrap();
        let stored: String =
            sqlx::query_scalar("SELECT raw_b64 FROM archive_messages WHERE list_id=$1 AND hash=$2")
                .bind("dev.example.invalid")
                .bind(&generated.hash)
                .fetch_one(db.pool())
                .await
                .unwrap();
        let stored = String::from_utf8(
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, stored).unwrap(),
        )
        .unwrap();
        assert!(
            stored.starts_with("Message-ID: <import."),
            "a generated id is written into the stored copy: {stored:.80}"
        );
        let audits: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='archive.import'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(audits, 3, "one audit event per batch, including empty ones");
    });
}

#[test]
fn export_writes_everything_a_thread_or_a_month_plain_or_gzipped() {
    let root = tempfile::tempdir().unwrap();
    let url = setup(root.path());
    let mbox = root.path().join("archive.mbox");
    std::fs::write(&mbox, fixture_mbox()).unwrap();
    command(root.path(), &url)
        .args(["archive", "import", "dev.example.invalid"])
        .arg(&mbox)
        .assert()
        .success();
    let whole = root.path().join("whole.mbox");
    command(root.path(), &url)
        .args(["archive", "export", "dev.example.invalid", "--output"])
        .arg(&whole)
        .assert()
        .success()
        .stderr(predicate::str::contains("exported 4 messages"));
    let exported = std::fs::read(&whole).unwrap();
    assert_eq!(count_messages(&exported), 4);
    assert!(
        String::from_utf8_lossy(&exported).contains("\n>From the start.\r\n"),
        "mboxrd quoting on the way out"
    );
    let root_hash = listmngr_mail::message_id_hash("<a1@example.invalid>").unwrap();
    let stdout = command(root.path(), &url)
        .args([
            "archive",
            "export",
            "dev.example.invalid",
            "--thread",
            &root_hash,
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(count_messages(&stdout), 2);
    let month = root.path().join("feb.mbox.gz");
    command(root.path(), &url)
        .args([
            "archive",
            "export",
            "dev.example.invalid",
            "--month",
            "2024-02",
            "--gzip",
            "--output",
        ])
        .arg(&month)
        .assert()
        .success()
        .stderr(predicate::str::contains("exported 1 messages"));
    let plain = gunzip(&std::fs::read(&month).unwrap());
    assert_eq!(count_messages(&plain), 1);
    // The export is the archive's published copy, so it carries the list's
    // subject prefix, exactly as a browser download of the same month does.
    assert!(String::from_utf8_lossy(&plain).contains("Subject: [dev] Notes"));
    command(root.path(), &url)
        .args([
            "archive",
            "export",
            "dev.example.invalid",
            "--month",
            "2024-13",
        ])
        .assert()
        .failure();
}

/// The plan's bound: a hundred thousand posts imported in under ten
/// minutes on a development machine.
#[test]
#[ignore = "benchmark: imports 100k posts; run by hand and record the numbers"]
fn import_100k_posts_in_under_ten_minutes() {
    let root = tempfile::tempdir().unwrap();
    let url = setup(root.path());
    let mbox = root.path().join("big.mbox");
    let mut file = std::io::BufWriter::new(std::fs::File::create(&mbox).unwrap());
    let filler = "The quick brown fox jumps over the lazy dog. ".repeat(12);
    for i in 0..100_000_u32 {
        let parent = if i % 4 == 0 {
            String::new()
        } else {
            format!("In-Reply-To: <m{}@example.invalid>\r\n", i - (i % 4))
        };
        let day = 1 + (i % 28);
        let month = 1 + (i / 28 % 12);
        let year = 2015 + (i / 336 % 10);
        let raw = format!(
            "Message-ID: <m{i}@example.invalid>\r\n{parent}From: User {u} <user{u}@example.invalid>\r\nDate: {day:02} {month:02} {year} 10:00:00 +0000\r\nSubject: Post {i} about topic {t}\r\nContent-Type: text/plain\r\n\r\n{filler}\r\nPost number {i}.\r\n",
            u = i % 500,
            t = i % 97,
            day = day,
            month = [
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"
            ][(month - 1) as usize],
        );
        write_message(&mut file, raw.as_bytes()).unwrap();
    }
    file.flush().unwrap();
    drop(file);
    let started = std::time::Instant::now();
    command(root.path(), &url)
        .timeout(std::time::Duration::from_secs(1200))
        .args(["archive", "import", "dev.example.invalid"])
        .arg(&mbox)
        .assert()
        .success()
        .stdout(predicate::str::contains("imported 100000 messages"));
    let elapsed = started.elapsed();
    println!(
        "IMPORT BENCH: 100000 posts ({} MiB mbox) imported in {elapsed:?}",
        std::fs::metadata(&mbox).unwrap().len() / 1024 / 1024
    );
    assert!(
        elapsed < std::time::Duration::from_secs(600),
        "import took {elapsed:?}"
    );
}
