//! Phase 6 acceptance (`P6-ACCEPTANCE`): the real binary brings a real
//! site here from the fixtures in the tree — Mailman 3.3.10's own
//! database and message store (`import3 --db`), the archive as `HyperKitty`
//! exports it (`archive import`), what readers left on `HyperKitty`
//! (`import3 --hyperkitty`), a Mailman 2.1 `config.pck` for the same list
//! (`import21`) — serves it, and answers `HyperKitty`'s own permalinks with
//! `HyperKitty`'s own hashes. On `SQLite`, and on `PostgreSQL` when
//! `TEST_POSTGRES_URL` names a disposable server.
use assert_cmd::Command;
use listmngr_db::Database;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const LIST: &str = "rust-users.example.invalid";
const ADDRESS: &str = "rust-users@example.invalid";

/// The fixture's three posts, with `HyperKitty`'s hashes.
const POSTS: [(&str, &str); 3] = [
    ("WYKGK4F2CNJFZTD2CVSSNJYJ3EP4JHZU", "Hello archive"),
    ("L33UPQU2GXSBPUQOMJYW2I7F77HOTD7Z", "Re: Hello archive"),
    ("2MFWINCCDIFKOCUBTRMVNENN437Z53YU", "Another thread"),
];

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../import/tests/fixtures")
}

fn command(dir: &Path, url: &str) -> Command {
    let mut command = Command::cargo_bin("listmngr").unwrap();
    command
        .env_clear()
        .current_dir(dir)
        .env("LISTMNGR__DATABASE__URL", url);
    command
}

fn report(output: &std::process::Output) -> serde_json::Value {
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(output.stdout.trim_ascii()).unwrap()
}

/// The fixture's three posts as `HyperKitty`'s mbox export writes them.
fn hyperkitty_mbox() -> Vec<u8> {
    use listmngr_archive::mbox::write_message;
    let mut mbox = Vec::new();
    for message in [
        "Message-ID: <root-1@example.invalid>\r\nFrom: alice@example.invalid\r\nDate: Mon, 21 Sep 2026 09:00:00 +0000\r\nSubject: Hello archive\r\n\r\nThe first post.\r\n",
        "Message-ID: <reply-1@example.invalid>\r\nIn-Reply-To: <root-1@example.invalid>\r\nFrom: bob@example.invalid\r\nDate: Mon, 21 Sep 2026 10:00:00 +0000\r\nSubject: Re: Hello archive\r\n\r\nA reply.\r\n",
        "Message-ID: <root-2@example.invalid>\r\nFrom: carol@elsewhere.invalid\r\nDate: Tue, 22 Sep 2026 08:30:00 +0000\r\nSubject: Another thread\r\n\r\nA second thread.\r\n",
    ] {
        write_message(&mut mbox, message.as_bytes()).unwrap();
    }
    mbox
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A child process killed when the test ends, panicking or not.
struct Guard(std::process::Child);

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn serve(dir: &Path, url: &str, port: u16) -> Guard {
    std::process::Command::new(assert_cmd::cargo::cargo_bin("listmngr"))
        .env_clear()
        .env("RUST_LOG", "info")
        .env("LISTMNGR__DATABASE__URL", url)
        .env("LISTMNGR__WEB__LISTEN", format!("127.0.0.1:{port}"))
        .env(
            "LISTMNGR__SITE__BASE_URL",
            format!("http://127.0.0.1:{port}"),
        )
        .current_dir(dir)
        .arg("serve")
        .stdout(std::process::Stdio::null())
        .stderr(std::fs::File::create(dir.join("serve.log")).unwrap())
        .spawn()
        .map(Guard)
        .unwrap()
}

async fn wait_for(client: &reqwest::Client, origin: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(response) = client.get(format!("{origin}/healthz")).send().await
            && response.status().is_success()
        {
            return;
        }
        assert!(Instant::now() < deadline, "the server never answered");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Step 1: The Mailman 3 core: its database and message store.
fn import_core(dir: &Path, url: &str) {
    let mailman_db = format!(
        "sqlite://{}?mode=ro",
        fixtures().join("mailman3/mailman.db").display()
    );
    let site = report(
        &command(dir, url)
            .args(["import3", "--db", &mailman_db, "--var-dir"])
            .arg(fixtures().join("mailman3/var"))
            .output()
            .unwrap(),
    );
    assert_eq!(site["domains"], 2, "{site}");
    assert_eq!(site["lists"], 2);
    assert_eq!(site["users"], 9);
    assert_eq!(site["members"], 4);
    assert_eq!(site["held"], 1);
    assert_eq!(site["requests"], 1);
    assert_eq!(site["skipped"], 0);
}

/// Step 2: The archive, as `HyperKitty` exports it.
fn import_archive(dir: &Path, url: &str) {
    let mbox = dir.join("rust-users.mbox");
    std::fs::write(&mbox, hyperkitty_mbox()).unwrap();
    let output = command(dir, url)
        .args(["archive", "import", LIST])
        .arg(&mbox)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("imported 3 messages"),
        "{output:?}"
    );
}

/// Step 3: What readers left on `HyperKitty`.
fn import_readers(dir: &Path, url: &str) {
    let hyperkitty = format!(
        "sqlite://{}?mode=ro",
        fixtures().join("hyperkitty/hyperkitty.db").display()
    );
    let readers = report(
        &command(dir, url)
            .args(["import3", "--hyperkitty", &hyperkitty])
            .output()
            .unwrap(),
    );
    assert_eq!(readers["hyperkitty"]["list_id"], LIST, "{readers}");
    assert_eq!(readers["hyperkitty"]["votes"], 3);
    assert_eq!(readers["hyperkitty"]["tags"], 2);
    assert_eq!(readers["hyperkitty"]["categories"], 1);
    assert_eq!(readers["hyperkitty"]["favorites"], 1);
    assert_eq!(readers["hyperkitty"]["skipped"], 0);
}

/// Step 4: The same list's Mailman 2.1 configuration on top. The core already
/// held the pickle's bans and most of its roster (the same site's history
/// twice): nothing is doubled, what is new comes.
fn import_legacy(dir: &Path, url: &str) {
    let legacy = report(
        &command(dir, url)
            .args(["import21", LIST])
            .arg(fixtures().join("mailman21-full.pck"))
            .output()
            .unwrap(),
    );
    assert_eq!(legacy["bans"], 0, "{legacy}");
    assert_eq!(legacy["header_matches"], 2, "{legacy}");
    assert!(legacy["settings"].as_u64().unwrap() > 50, "{legacy}");
    let subscribed = ["members", "owners", "moderators", "nonmembers", "skipped"]
        .iter()
        .map(|key| legacy[key].as_u64().unwrap())
        .sum::<u64>();
    assert_eq!(subscribed, 5 + 2 + 1 + 3, "{legacy}");
    assert_eq!(legacy["skipped"], 6, "{legacy}");
}

/// What the database holds now: the 2.1 settings on the list the 3.3
/// core brought, its archive with `HyperKitty`'s hashes; then the archive
/// opened, so `HyperKitty`'s public permalinks can be followed without a
/// session (the core kept it private).
async fn check_database(url: &str) {
    let db = Database::connect(url, 1).await.unwrap();
    let list = db.lists().get(&LIST.parse().unwrap()).await.unwrap();
    assert_eq!(list.subject_prefix, "[Rust] ");
    assert_eq!(list.preferred_language, "vi");
    let meta = db
        .archive()
        .browser_thread_meta(&LIST.parse().unwrap(), None, POSTS[0].0)
        .await
        .unwrap();
    assert_eq!(meta.category.as_deref(), Some("announcements"));
    assert_eq!(meta.tags.len(), 2);
    let count = |sql: &'static str| {
        let db = db.clone();
        async move {
            sqlx::query_scalar::<_, i64>(sql)
                .bind(LIST)
                .fetch_one(db.pool())
                .await
                .unwrap()
        }
    };
    assert_eq!(
        count("SELECT COUNT(*) FROM archive_messages WHERE list_id=$1").await,
        3
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM bans WHERE list_id=$1").await,
        2,
        "the 2.1 bans, once"
    );
    db.lists()
        .update(
            &LIST.parse().unwrap(),
            &serde_json::json!({"archive_policy": "public"}),
        )
        .await
        .unwrap();
    db.pool().close().await;
}

/// A `308` and where to.
async fn redirected(client: &reqwest::Client, url: &str) -> String {
    let response = client.get(url).send().await.unwrap();
    assert_eq!(response.status(), 308, "{url}");
    response
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned()
}

/// The page at `path`: its status and body.
async fn page(client: &reqwest::Client, origin: &str, path: &str) -> (u16, String) {
    let response = client.get(format!("{origin}{path}")).send().await.unwrap();
    let status = response.status().as_u16();
    (status, response.text().await.unwrap())
}

/// Step 5: Served: `HyperKitty`'s permalinks, under both prefixes, lead to the
/// posts with their subjects.
async fn walk_permalinks(client: &reqwest::Client, origin: &str) {
    for (hash, subject) in POSTS {
        for prefix in ["archives", "hyperkitty"] {
            let target = redirected(
                client,
                &format!("{origin}/{prefix}/list/{ADDRESS}/message/{hash}/"),
            )
            .await;
            assert_eq!(target, format!("/web/lists/{LIST}/archive?message={hash}"));
            let (status, body) = page(client, origin, &target).await;
            assert_eq!(status, 200, "{target}");
            assert!(
                body.contains(subject),
                "{target}: {subject} not on the page"
            );
        }
    }
}

/// The thread, the month, the index and `Postorius`'s list page; a name
/// that is not a list, and a hash the archive lacks.
async fn walk_other_urls(client: &reqwest::Client, origin: &str) {
    let thread = redirected(
        client,
        &format!("{origin}/archives/list/{ADDRESS}/thread/{}/", POSTS[0].0),
    )
    .await;
    assert_eq!(
        thread,
        format!("/web/lists/{LIST}/archive/thread/{}", POSTS[0].0)
    );
    let (status, body) = page(client, origin, &thread).await;
    assert_eq!(status, 200);
    assert!(
        body.contains("Hello archive") && body.contains("Re: Hello archive"),
        "{body}"
    );
    assert_eq!(
        redirected(client, &format!("{origin}/archives/list/{LIST}/2026/9/")).await,
        format!("/web/lists/{LIST}/archive/threads/2026/9")
    );
    assert_eq!(
        redirected(client, &format!("{origin}/hyperkitty/list/{ADDRESS}/")).await,
        format!("/web/lists/{LIST}/archive")
    );
    assert_eq!(
        page(client, origin, &format!("/web/lists/{LIST}/archive"))
            .await
            .0,
        200
    );
    assert_eq!(
        redirected(client, &format!("{origin}/postorius/lists/{LIST}/")).await,
        format!("/web/lists/{LIST}")
    );
    let nowhere = client
        .get(format!(
            "{origin}/archives/list/nodomain/message/{}/",
            POSTS[0].0
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(nowhere.status(), 404);
    let unknown = redirected(
        client,
        &format!("{origin}/archives/list/{ADDRESS}/message/NOTAHASH/"),
    )
    .await;
    assert_eq!(
        page(client, origin, &unknown).await.0,
        404,
        "a hash the archive lacks"
    );
}

/// The whole migration on `url`, then the site served and `HyperKitty`'s
/// URLs answered.
async fn scenario(dir: &Path, url: &str) {
    command(dir, url).arg("migrate").assert().success();
    import_core(dir, url);
    import_archive(dir, url);
    import_readers(dir, url);
    import_legacy(dir, url);
    check_database(url).await;
    let port = free_port();
    let origin = format!("http://127.0.0.1:{port}");
    let child = serve(dir, url, port);
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    wait_for(&client, &origin).await;
    walk_permalinks(&client, &origin).await;
    walk_other_urls(&client, &origin).await;
    drop(child);
}

#[tokio::test]
async fn a_mailman_site_moves_here_and_its_hyperkitty_links_still_resolve() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("site.db").display());
    scenario(dir.path(), &url).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_phase6_acceptance_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("phase6")
        .await
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let url = schema.url.clone();
    let path = dir.path().to_path_buf();
    let result = tokio::spawn(async move { scenario(&path, &url).await }).await;
    schema.drop().await.unwrap();
    result.unwrap();
}
