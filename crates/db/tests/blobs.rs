//! The message store behind `message_blobs`: `db` keeps the bytes in the
//! row (the default), `fs` and `s3` keep them outside with the row as the
//! reference, and every reader and writer goes through `BlobStore` — so a
//! queued message comes back whole whichever backend holds it, the same
//! bytes twice are one object and one row, the task sweep collects the
//! objects no row names any more once they are older than the grace,
//! `migrate` moves the bytes a row still holds into the configured store
//! and `check` names every row whose bytes are nowhere.
use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::{Router, extract::State};
use chrono::{DateTime, Duration, Utc};
use listmngr_db::Database;
use listmngr_db::blobs::{BlobStore, S3Settings, sigv4};
use listmngr_db::mail_queue::{NewMessage, Queue};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const DAY_MS: i64 = 86_400_000;
const ACCESS: &str = "AKIAIOSFODNN7EXAMPLE";
const SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
const REGION: &str = "us-east-1";

async fn fixture(store: BlobStore) -> Database {
    let db = Database::connect("sqlite::memory:", 1)
        .await
        .unwrap()
        .with_message_store(store);
    db.migrate().await.unwrap();
    db
}

fn message(body: &str) -> NewMessage {
    NewMessage {
        raw: format!("From: a@example.invalid\r\nSubject: t\r\n\r\n{body}\r\n").into_bytes(),
        external_id: format!("<{}@example.invalid>", body.len()),
        context: "{}".into(),
        queue: Queue::In,
        max_attempts: 3,
    }
}

async fn row_raw(db: &Database, key: &str) -> Vec<u8> {
    sqlx::query_scalar("SELECT raw FROM message_blobs WHERE store_key=$1")
        .bind(key)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

async fn rows(db: &Database) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM message_blobs")
        .fetch_one(db.pool())
        .await
        .unwrap()
}

fn object_path(root: &Path, key: &str) -> PathBuf {
    root.join(&key[..2]).join(&key[2..4]).join(key)
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// An object written straight into the `fs` store, `age` old.
fn plant(root: &Path, bytes: &[u8], age: Duration) -> String {
    let key = BlobStore::key(bytes);
    let path = object_path(root, &key);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, bytes).unwrap();
    let modified = std::time::SystemTime::now() - age.to_std().unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    key
}

#[tokio::test]
async fn fs_keeps_the_bytes_outside_the_row_and_reads_them_back() {
    let root = tempfile::tempdir().unwrap();
    let db = fixture(BlobStore::fs(root.path())).await;
    let input = message("hello");
    let key = BlobStore::key(&input.raw);
    let job = db.mail_queue().enqueue(input.clone(), 1_000).await.unwrap();
    let path = object_path(root.path(), &key);
    assert_eq!(std::fs::read(&path).unwrap(), input.raw);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert!(
        row_raw(&db, &key).await.is_empty(),
        "the row is the reference, not the bytes"
    );
    let stored = db.mail_queue().message(job.message_id).await.unwrap();
    assert_eq!(stored.raw, input.raw);
    assert_eq!(stored.store_key, key);
    // The same bytes again: one row, one object, two messages.
    db.mail_queue().enqueue(input, 2_000).await.unwrap();
    assert_eq!(rows(&db).await, 1);
    assert_eq!(
        std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
        1,
        "no temporary file left behind"
    );
    assert_eq!(db.blobs().name(), "fs");
}

#[tokio::test]
async fn the_sweep_collects_orphan_objects_after_the_grace_period() {
    let root = tempfile::tempdir().unwrap();
    let db = fixture(BlobStore::fs(root.path())).await;
    let kept = message("kept");
    let kept_key = BlobStore::key(&kept.raw);
    db.mail_queue().enqueue(kept, now_ms()).await.unwrap();
    let old = plant(root.path(), b"an old orphan", Duration::hours(2));
    let fresh = plant(root.path(), b"a fresh orphan", Duration::zero());
    let stray = root.path().join("ab").join("cd").join("not-a-key");
    std::fs::create_dir_all(stray.parent().unwrap()).unwrap();
    std::fs::write(&stray, b"somebody else's file").unwrap();
    let summary = db.tasks().sweep(now_ms(), DAY_MS).await.unwrap();
    assert_eq!(summary.collected_blobs, 1, "{summary:?}");
    assert!(summary.changed());
    assert!(!object_path(root.path(), &old).exists(), "the old orphan");
    assert!(
        object_path(root.path(), &fresh).exists(),
        "the fresh one waits"
    );
    assert!(
        object_path(root.path(), &kept_key).exists(),
        "the referenced one stays"
    );
    assert!(stray.exists(), "a file that is not a key is not ours");
    let again = db.tasks().sweep(now_ms(), DAY_MS).await.unwrap();
    assert_eq!(again.collected_blobs, 0);
    let audited: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log WHERE action='task.sweep' AND target_id='blobs'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(audited, 1, "one audited batch");
}

#[tokio::test]
async fn migrate_moves_bytes_a_row_still_holds_and_check_names_what_is_missing() {
    let root = tempfile::tempdir().unwrap();
    let db = fixture(BlobStore::db()).await;
    let input = message("in the row");
    let key = BlobStore::key(&input.raw);
    let job = db.mail_queue().enqueue(input.clone(), 1_000).await.unwrap();
    assert_eq!(row_raw(&db, &key).await, input.raw);
    assert_eq!(db.blobs().name(), "db");
    // The store switched to fs on the same database: the row's bytes are
    // served until they are moved.
    let moved = db.clone().with_message_store(BlobStore::fs(root.path()));
    assert_eq!(
        moved
            .mail_queue()
            .message(job.message_id)
            .await
            .unwrap()
            .raw,
        input.raw
    );
    let report = moved.blobs().check(&moved).await.unwrap();
    assert_eq!((report.rows, report.in_rows, report.in_store), (1, 1, 0));
    assert_eq!(moved.blobs().migrate_rows(&moved).await.unwrap(), 1);
    assert_eq!(
        std::fs::read(object_path(root.path(), &key)).unwrap(),
        input.raw
    );
    assert!(row_raw(&moved, &key).await.is_empty());
    assert_eq!(
        moved
            .mail_queue()
            .message(job.message_id)
            .await
            .unwrap()
            .raw,
        input.raw
    );
    assert_eq!(moved.blobs().migrate_rows(&moved).await.unwrap(), 0);
    let report = moved.blobs().check(&moved).await.unwrap();
    assert_eq!((report.rows, report.in_rows, report.in_store), (1, 0, 1));
    assert!(report.missing.is_empty());
    std::fs::remove_file(object_path(root.path(), &key)).unwrap();
    let report = moved.blobs().check(&moved).await.unwrap();
    assert_eq!(report.missing, vec![key.clone()]);
    assert!(moved.mail_queue().message(job.message_id).await.is_err());
    // Back on the row-only store the emptied row is what `check` reports.
    let report = db.blobs().check(&db).await.unwrap();
    assert_eq!(report.missing, vec![key]);
}

/// A bucket of the test's own: objects with their modification time, the
/// request lines seen, and how many requests carried a signature that did
/// not verify against the known secret — those are refused with `403`.
/// An object's bytes and when it was last modified.
type Object = (Vec<u8>, DateTime<Utc>);

#[derive(Default)]
struct Bucket {
    objects: BTreeMap<String, Object>,
    requests: Vec<String>,
    bad_signatures: usize,
}

type Shared = Arc<Mutex<Bucket>>;

const PAGE: usize = 2;

fn verified(method: &Method, uri: &Uri, headers: &HeaderMap, body: &[u8]) -> bool {
    let Some(authorization) = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Some(signed) = authorization
        .split("SignedHeaders=")
        .nth(1)
        .and_then(|rest| rest.split(',').next())
    else {
        return false;
    };
    let signed_headers: Vec<(&str, &str)> = signed
        .split(';')
        .map(|name| {
            (
                name,
                headers
                    .get(name)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or(""),
            )
        })
        .collect();
    let payload = headers
        .get("x-amz-content-sha256")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if payload != sigv4::sha256_hex(body) {
        return false;
    }
    let Some(at) = headers
        .get("x-amz-date")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| chrono::NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%SZ").ok())
    else {
        return false;
    };
    let query: Vec<(String, String)> = uri
        .query()
        .unwrap_or("")
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let decode = |text: &str| {
                percent_encoding::percent_decode_str(text)
                    .decode_utf8_lossy()
                    .into_owned()
            };
            (decode(key), decode(value))
        })
        .collect();
    let query: Vec<(&str, &str)> = query
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let expected = sigv4::sign(
        &sigv4::Credentials {
            access_key_id: ACCESS.into(),
            secret_access_key: SECRET.into(),
        },
        REGION,
        method.as_str(),
        uri.path(),
        &query,
        &signed_headers,
        payload,
        at.and_utc(),
    );
    expected.authorization == authorization
}

fn list_xml(bucket: &Bucket, prefix: &str, after: Option<&str>) -> String {
    use std::fmt::Write as _;
    let matching: Vec<(&String, &Object)> = bucket
        .objects
        .iter()
        .filter(|(key, _)| key.starts_with(prefix))
        .filter(|(key, _)| after.is_none_or(|after| key.as_str() > after))
        .collect();
    let page = &matching[..matching.len().min(PAGE)];
    let truncated = matching.len() > PAGE;
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Name>mail</Name>",
    );
    let _ = write!(
        xml,
        "<Prefix>{prefix}</Prefix><KeyCount>{}</KeyCount><MaxKeys>{PAGE}</MaxKeys><IsTruncated>{truncated}</IsTruncated>",
        page.len()
    );
    if truncated {
        let _ = write!(
            xml,
            "<NextContinuationToken>{}</NextContinuationToken>",
            page.last().unwrap().0
        );
    }
    for (key, (bytes, at)) in page {
        let _ = write!(
            xml,
            "<Contents><Key>{key}</Key><LastModified>{}</LastModified><ETag>\"x\"</ETag><Size>{}</Size><StorageClass>STANDARD</StorageClass></Contents>",
            at.format("%Y-%m-%dT%H:%M:%S%.3fZ"),
            bytes.len()
        );
    }
    xml.push_str("</ListBucketResult>");
    xml
}

// One request at a time, the lock held for the whole of it on purpose.
#[allow(clippy::significant_drop_tightening)]
async fn handle(
    State(shared): State<Shared>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, Vec<u8>) {
    let mut bucket = shared.lock().unwrap();
    bucket.requests.push(format!("{method} {uri}"));
    if !verified(&method, &uri, &headers, &body) {
        bucket.bad_signatures += 1;
        return (
            StatusCode::FORBIDDEN,
            b"<Error><Code>SignatureDoesNotMatch</Code></Error>".to_vec(),
        );
    }
    let path = uri.path();
    let Some(rest) = path.strip_prefix("/mail") else {
        return (
            StatusCode::NOT_FOUND,
            b"<Error><Code>NoSuchBucket</Code></Error>".to_vec(),
        );
    };
    let key = rest.trim_start_matches('/');
    if key.is_empty() {
        let query: BTreeMap<String, String> = uri
            .query()
            .unwrap_or("")
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .map(|(key, value)| {
                (
                    key.to_owned(),
                    percent_encoding::percent_decode_str(value)
                        .decode_utf8_lossy()
                        .into_owned(),
                )
            })
            .collect();
        if method != Method::GET || query.get("list-type").map(String::as_str) != Some("2") {
            return (StatusCode::BAD_REQUEST, Vec::new());
        }
        let xml = list_xml(
            &bucket,
            query.get("prefix").map_or("", String::as_str),
            query.get("continuation-token").map(String::as_str),
        );
        return (StatusCode::OK, xml.into_bytes());
    }
    match method {
        Method::PUT => {
            bucket
                .objects
                .insert(key.to_owned(), (body.to_vec(), Utc::now()));
            (StatusCode::OK, Vec::new())
        }
        Method::GET => bucket.objects.get(key).map_or_else(
            || {
                (
                    StatusCode::NOT_FOUND,
                    b"<Error><Code>NoSuchKey</Code></Error>".to_vec(),
                )
            },
            |(bytes, _)| (StatusCode::OK, bytes.clone()),
        ),
        Method::HEAD => {
            if bucket.objects.contains_key(key) {
                (StatusCode::OK, Vec::new())
            } else {
                (StatusCode::NOT_FOUND, Vec::new())
            }
        }
        Method::DELETE => {
            bucket.objects.remove(key);
            (StatusCode::NO_CONTENT, Vec::new())
        }
        _ => (StatusCode::METHOD_NOT_ALLOWED, Vec::new()),
    }
}

async fn fake_s3() -> (u16, Shared) {
    let shared: Shared = Arc::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = Router::new().fallback(handle).with_state(shared.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (port, shared)
}

fn settings(port: u16, secret: &str) -> S3Settings {
    S3Settings {
        bucket: "mail".into(),
        region: REGION.into(),
        endpoint: Some(format!("http://127.0.0.1:{port}")),
        prefix: "messages/".into(),
        credentials: sigv4::Credentials {
            access_key_id: ACCESS.into(),
            secret_access_key: secret.into(),
        },
    }
}

#[tokio::test]
async fn s3_puts_gets_lists_and_deletes_with_signed_requests() {
    let (port, fake) = fake_s3().await;
    let db = fixture(BlobStore::s3(settings(port, SECRET)).unwrap()).await;
    assert_eq!(db.blobs().name(), "s3");
    let input = message("in the bucket");
    let key = BlobStore::key(&input.raw);
    let job = db
        .mail_queue()
        .enqueue(input.clone(), now_ms())
        .await
        .unwrap();
    {
        let fake = fake.lock().unwrap();
        assert_eq!(
            fake.objects.get(&format!("messages/{key}")).map(|o| &o.0),
            Some(&input.raw)
        );
        assert_eq!(fake.bad_signatures, 0, "{:?}", fake.requests);
    }
    assert!(row_raw(&db, &key).await.is_empty());
    assert_eq!(
        db.mail_queue().message(job.message_id).await.unwrap().raw,
        input.raw
    );
    db.mail_queue().enqueue(input, now_ms()).await.unwrap();
    assert_eq!(rows(&db).await, 1);
    // Three old orphans and a fresh one, listed two per page; an object
    // outside the prefix and one that is not a key are nobody's business.
    let old = Utc::now() - Duration::hours(2);
    let mut orphans = Vec::new();
    {
        let mut fake = fake.lock().unwrap();
        for body in [b"one".as_slice(), b"two", b"three"] {
            let orphan = BlobStore::key(body);
            fake.objects
                .insert(format!("messages/{orphan}"), (body.to_vec(), old));
            orphans.push(orphan);
        }
        let fresh = BlobStore::key(b"fresh");
        fake.objects
            .insert(format!("messages/{fresh}"), (b"fresh".to_vec(), Utc::now()));
        fake.objects
            .insert("elsewhere/x".into(), (b"x".to_vec(), old));
        fake.objects
            .insert("messages/readme.txt".into(), (b"x".to_vec(), old));
        fake.requests.clear();
    }
    let summary = db.tasks().sweep(now_ms(), DAY_MS).await.unwrap();
    assert_eq!(summary.collected_blobs, 3, "{summary:?}");
    {
        let fake = fake.lock().unwrap();
        let keys: Vec<&String> = fake.objects.keys().collect();
        assert_eq!(keys.len(), 4, "{keys:?}");
        assert!(
            orphans
                .iter()
                .all(|orphan| !fake.objects.contains_key(&format!("messages/{orphan}")))
        );
        assert!(fake.objects.contains_key("elsewhere/x"));
        assert!(fake.objects.contains_key("messages/readme.txt"));
        assert!(
            fake.requests
                .iter()
                .any(|line| line.contains("continuation-token")),
            "{:?}",
            fake.requests
        );
        assert_eq!(fake.bad_signatures, 0, "{:?}", fake.requests);
    }
    let report = db.blobs().check(&db).await.unwrap();
    assert_eq!((report.rows, report.in_rows, report.in_store), (1, 0, 1));
    assert!(report.missing.is_empty());
    // A wrong secret is refused by the target, and the refusal is the
    // caller's error: nothing is queued.
    let wrong = fixture(BlobStore::s3(settings(port, "not-the-secret")).unwrap()).await;
    assert!(
        wrong
            .mail_queue()
            .enqueue(message("refused"), now_ms())
            .await
            .is_err()
    );
    assert_eq!(rows(&wrong).await, 0);
    assert_eq!(fake.lock().unwrap().bad_signatures, 1);
}

#[test]
fn the_s3_settings_resolve_hosts_and_paths() {
    let virtual_host = BlobStore::s3(S3Settings {
        endpoint: None,
        prefix: String::new(),
        ..settings(0, SECRET)
    })
    .unwrap();
    assert_eq!(virtual_host.name(), "s3");
    assert!(
        BlobStore::s3(S3Settings {
            endpoint: Some("ftp://x".into()),
            ..settings(0, SECRET)
        })
        .is_err()
    );
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_message_store_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("blobs")
        .await
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let db = Database::connect(&schema.url, 2)
        .await
        .unwrap()
        .with_message_store(BlobStore::fs(root.path()));
    db.migrate().await.unwrap();
    let input = message("on postgres");
    let key = BlobStore::key(&input.raw);
    let job = db
        .mail_queue()
        .enqueue(input.clone(), now_ms())
        .await
        .unwrap();
    assert!(row_raw(&db, &key).await.is_empty());
    assert_eq!(
        db.mail_queue().message(job.message_id).await.unwrap().raw,
        input.raw
    );
    let old = plant(root.path(), b"old orphan on postgres", Duration::hours(2));
    let summary = db.tasks().sweep(now_ms(), DAY_MS).await.unwrap();
    assert_eq!(summary.collected_blobs, 1);
    assert!(!object_path(root.path(), &old).exists());
    assert!(object_path(root.path(), &key).exists());
    let report = db.blobs().check(&db).await.unwrap();
    assert_eq!((report.rows, report.in_rows, report.in_store), (1, 0, 1));
    assert_eq!(db.blobs().migrate_rows(&db).await.unwrap(), 0);
    db.pool().close().await;
    schema.drop().await.unwrap();
}
