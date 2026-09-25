//! The three remote archivers behind a list's `list_archivers` toggles:
//! `prototype` drops the archived copy into a maildir, `mhonarc` pipes it
//! to an operator-configured command, and `mail-archive` sends a copy to
//! the public service through the outbound queue, queued inside the
//! archive's own transaction. A toggle that is off, an unconfigured
//! server, an archive policy of `never` and — for `mail-archive` — a
//! private list each forward nothing.
use crate::archive::forward_to_archivers;
use listmngr_archive::archivers::Settings;
use listmngr_db::mail_queue::{MessageId, NewMessage, Queue};
use listmngr_db::{Database, NewList};
use std::path::Path;

const ADDRESS: &str = "archive@mail-archive.invalid";

#[tokio::test]
async fn remote_archivers_follow_the_list_toggles_and_the_server_configuration() {
    let db = Database::connect("sqlite::memory:", 1)
        .await
        .unwrap()
        .with_mail_archive_address(ADDRESS);
    db.migrate().await.unwrap();
    matrix(db).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_remote_archivers_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("runners_remote_archivers")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 3)
        .await
        .unwrap()
        .with_mail_archive_address(ADDRESS);
    db.migrate().await.unwrap();
    matrix(db).await;
    schema.drop().await.unwrap();
}

async fn seed(db: &Database) {
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    for (name, policy) in [
        ("dev", "public"),
        ("quiet", "never"),
        ("inner", "private"),
        ("plain", "public"),
    ] {
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
    // `plain` keeps every archiver off; the others switch all three on.
    for name in ["dev", "quiet", "inner"] {
        let list = format!("{name}.example.invalid").parse().unwrap();
        for archiver in ["prototype", "mhonarc", "mail-archive"] {
            db.lists()
                .set_archiver(&list, archiver, true)
                .await
                .unwrap();
        }
    }
}

async fn archived(db: &Database, list: &str, id: &str) -> MessageId {
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: format!("Message-ID: {id}\r\nFrom: A <a@example.invalid>\r\nSubject: Quarterly numbers\r\nContent-Type: text/plain\r\n\r\nThe quarterly numbers are in.\r\n").into_bytes(),
                external_id: id.into(),
                context: serde_json::json!({"version":1,"list_id":list}).to_string(),
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
    listmngr_archive::process(db, &lease, 101).await.unwrap();
    lease.job.message_id
}

/// A shell script standing in for `MHonArc`: it writes its standard input
/// to the file named by its first argument and appends the second.
fn mhonarc_script(root: &Path) -> String {
    let path = root.join("mhonarc.sh");
    std::fs::write(
        &path,
        "#!/bin/sh\ncat > \"$1\"\nprintf '%s\\n' \"$2\" >> \"$1\"\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    path.display().to_string()
}

fn settings(root: &Path, script: &str) -> Settings {
    Settings {
        mhonarc: vec![
            "/bin/sh".into(),
            script.into(),
            root.join("mhonarc-$hash.out").display().to_string(),
            "$listname".into(),
        ],
        prototype: root.join("prototype").display().to_string(),
        hyperkitty_url: String::new(),
        hyperkitty_key: String::new(),
    }
}

/// What a `HyperKitty` of the test's own received: the headers and body
/// of each post to `/api/mailman/archive`.
type Received = std::sync::Arc<std::sync::Mutex<Vec<(axum::http::HeaderMap, Vec<u8>)>>>;

async fn hyperkitty_target(key: &'static str) -> (u16, Received) {
    use axum::{Router, extract::State, http::HeaderMap, routing::post};
    let received: Received = std::sync::Arc::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = Router::new()
        .route(
            "/hyperkitty/api/mailman/archive",
            post(
                move |State(received): State<Received>, headers: HeaderMap, body: axum::body::Bytes| async move {
                    let authorized = headers
                        .get("authorization")
                        .and_then(|value| value.to_str().ok())
                        == Some(&format!("Token {key}"));
                    received.lock().unwrap().push((headers, body.to_vec()));
                    if authorized {
                        (
                            axum::http::StatusCode::OK,
                            r#"{"url": "https://hk.example.invalid/hyperkitty/list/dev@example.invalid/message/HASH/"}"#,
                        )
                    } else {
                        (axum::http::StatusCode::UNAUTHORIZED, "<h1>Authorization Required</h1>")
                    }
                },
            ),
        )
        .with_state(received.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (port, received)
}

/// The `hyperkitty` archiver posts the published copy, keyed, to the
/// configured `HyperKitty` for a list that switched it on; a wrong key is
/// refused and logged; a list without the toggle posts nothing.
#[tokio::test]
async fn the_hyperkitty_archiver_posts_the_published_copy_with_the_key() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    seed(&db).await;
    db.lists()
        .set_archiver(&"dev.example.invalid".parse().unwrap(), "hyperkitty", true)
        .await
        .unwrap();
    let (port, received) = hyperkitty_target("archiver-key").await;
    let settings = |key: &str| Settings {
        mhonarc: Vec::new(),
        prototype: String::new(),
        hyperkitty_url: format!("http://127.0.0.1:{port}/hyperkitty/"),
        hyperkitty_key: key.into(),
    };
    let message = archived(&db, "dev.example.invalid", "<hk-1@example.invalid>").await;
    let names = forward_to_archivers(&db, message, &settings("archiver-key")).await;
    assert_eq!(names, ["hyperkitty"]);
    let posts = received.lock().unwrap().clone();
    assert_eq!(posts.len(), 1);
    let (headers, body) = &posts[0];
    assert_eq!(headers["authorization"], "Token archiver-key");
    let content_type = headers["content-type"].to_str().unwrap().to_owned();
    let boundary = content_type
        .strip_prefix("multipart/form-data; boundary=")
        .expect("a multipart form");
    let body_text = String::from_utf8_lossy(body);
    assert!(
        body_text.contains(&format!("--{boundary}\r\n")),
        "{body_text}"
    );
    assert!(
        body_text.contains("name=\"mlist\"\r\n\r\ndev@example.invalid\r\n"),
        "{body_text}"
    );
    assert!(
        body_text.contains("name=\"message\"; filename=\"")
            && body_text.contains(".eml\"\r\nContent-Type: message/rfc822"),
        "{body_text}"
    );
    assert!(
        body_text.contains("Subject: [dev] Quarterly numbers")
            || body_text.contains("Subject: Quarterly numbers"),
        "the published copy: {body_text}"
    );
    assert!(
        body_text.ends_with(&format!("\r\n--{boundary}--\r\n")),
        "{body_text}"
    );
    // A wrong key: refused, logged, not among the names.
    let message = archived(&db, "dev.example.invalid", "<hk-2@example.invalid>").await;
    assert!(
        forward_to_archivers(&db, message, &settings("wrong-key"))
            .await
            .is_empty()
    );
    assert_eq!(received.lock().unwrap().len(), 2);
    // A list without the toggle, and one whose archive is `never`: nothing.
    let message = archived(&db, "plain.example.invalid", "<hk-3@example.invalid>").await;
    assert!(
        forward_to_archivers(&db, message, &settings("archiver-key"))
            .await
            .is_empty()
    );
    let message = archived(&db, "quiet.example.invalid", "<hk-4@example.invalid>").await;
    assert!(
        forward_to_archivers(&db, message, &settings("archiver-key"))
            .await
            .is_empty()
    );
    assert_eq!(received.lock().unwrap().len(), 2);
}

async fn out_recipients(db: &Database) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT r.email FROM delivery_recipients r JOIN queue_jobs j ON j.id=r.job_id WHERE j.queue='out' ORDER BY r.email",
    )
    .fetch_all(db.pool())
    .await
    .unwrap()
}

async fn audit_count(db: &Database, action: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action=$1")
        .bind(action)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

async fn matrix(db: Database) {
    seed(&db).await;
    let root = tempfile::tempdir().unwrap();
    let script = mhonarc_script(root.path());
    let settings = settings(root.path(), &script);
    nothing_to_forward(&db, root.path(), &settings).await;
    a_private_list_stays_off_the_public_service(&db, &settings).await;
    a_public_list_reaches_every_archiver(&db, root.path(), &settings).await;
    let stored = archived(&db, "dev.example.invalid", "<u@example.invalid>").await;
    assert!(
        forward_to_archivers(&db, stored, &Settings::default())
            .await
            .is_empty(),
        "an empty command and an empty maildir path switch the archivers off"
    );
}

/// A list with every archiver off, and one that keeps no archive.
async fn nothing_to_forward(db: &Database, root: &Path, settings: &Settings) {
    let plain = archived(db, "plain.example.invalid", "<p@example.invalid>").await;
    let quiet = archived(db, "quiet.example.invalid", "<n@example.invalid>").await;
    assert!(out_recipients(db).await.is_empty());
    assert_eq!(audit_count(db, "archive.archiver").await, 0);
    assert!(forward_to_archivers(db, plain, settings).await.is_empty());
    assert!(forward_to_archivers(db, quiet, settings).await.is_empty());
    assert!(!root.join("prototype").exists());
}

/// mail-archive is a public service, so a private list never reaches it;
/// its own archivers still run.
async fn a_private_list_stays_off_the_public_service(db: &Database, settings: &Settings) {
    let stored = archived(db, "inner.example.invalid", "<i@example.invalid>").await;
    assert!(out_recipients(db).await.is_empty());
    assert_eq!(audit_count(db, "archive.archiver").await, 0);
    assert_eq!(
        forward_to_archivers(db, stored, settings).await,
        vec!["mhonarc", "prototype"]
    );
}

/// The whole path for a public list, and a replay that overwrites rather
/// than duplicates.
async fn a_public_list_reaches_every_archiver(db: &Database, root: &Path, settings: &Settings) {
    let hash = listmngr_mail::message_id_hash("<q@example.invalid>").unwrap();
    let stored = archived(db, "dev.example.invalid", "<q@example.invalid>").await;
    // mail-archive: queued for the service as the archive is stored, in
    // that transaction, with its audit event.
    assert_eq!(out_recipients(db).await, vec![ADDRESS.to_owned()]);
    assert_eq!(audit_count(db, "archive.archiver").await, 1);
    assert_eq!(
        forward_to_archivers(db, stored, settings).await,
        vec!["mhonarc", "prototype"]
    );
    let maildir = root
        .join("prototype")
        .join("dev.example.invalid")
        .join("new")
        .join(&hash);
    let dropped = std::fs::read_to_string(&maildir).unwrap();
    assert!(
        dropped.contains("Subject: [dev] Quarterly numbers"),
        "the archived copy, cooked: {dropped}"
    );
    assert!(
        dropped.contains("The quarterly numbers are in."),
        "{dropped}"
    );
    let piped = std::fs::read_to_string(root.join(format!("mhonarc-{hash}.out"))).unwrap();
    assert!(piped.contains("The quarterly numbers are in."), "{piped}");
    assert!(
        piped.trim_end().ends_with("dev.example.invalid"),
        "the command's arguments carry $listname: {piped}"
    );
    assert_eq!(
        forward_to_archivers(db, stored, settings).await,
        vec!["mhonarc", "prototype"]
    );
    assert_eq!(std::fs::read_to_string(&maildir).unwrap(), dropped);
    assert!(
        !root
            .join("prototype")
            .join("dev.example.invalid")
            .join("tmp")
            .join(&hash)
            .exists(),
        "the staged copy is renamed into new/, never left in tmp/"
    );
}
