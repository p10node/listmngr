//! Chaos (`P7-CHAOS`): the real binary against failures around a delivery
//! — the process killed in the middle of an SMTP transaction, the relay
//! down when a post arrives, and (on `PostgreSQL`) every database
//! connection cut under the runners — with the one rule that matters:
//! no post is lost and no recipient gets a second copy without an
//! operator saying so.
use serde_json::Value;
use std::net::TcpListener as StdTcpListener;
use std::path::Path;
use std::process::{Child, Command as StdCommand, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

const fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_listmngr")
}

const DOMAIN: &str = "chaos.example.invalid";
const LIST: &str = "dev.chaos.example.invalid";
const MEMBER: &str = "reader@chaos.example.invalid";
const AUTHOR: &str = "author@chaos.example.invalid";

fn free_port() -> u16 {
    StdTcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// One accepted message: envelope and bytes.
#[derive(Debug, Clone)]
struct Delivery {
    rcpt_to: Vec<String>,
    data: Vec<u8>,
}

/// A real SMTP relay on loopback with two faults on demand: `stall`
/// answers `354` and then never answers the message (the client hangs
/// mid-transaction), `refuse` is simply not listening.
#[derive(Clone)]
struct Sink {
    deliveries: Arc<Mutex<Vec<Delivery>>>,
    stall: Arc<AtomicBool>,
    stalled: Arc<tokio::sync::Semaphore>,
}

impl Sink {
    fn new() -> Self {
        Self {
            deliveries: Arc::default(),
            stall: Arc::new(AtomicBool::new(false)),
            stalled: Arc::new(tokio::sync::Semaphore::new(0)),
        }
    }
    fn deliveries(&self) -> Vec<Delivery> {
        self.deliveries.lock().unwrap().clone()
    }
    fn to_member(&self) -> usize {
        self.deliveries()
            .iter()
            .filter(|d| d.rcpt_to.iter().any(|r| r == MEMBER))
            .count()
    }
}

/// Listen on `port` with `sink`; the task ends when aborted.
fn listen(port: u16, sink: Sink) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let listener = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            tokio::spawn(relay(stream, sink.clone()));
        }
    })
}

async fn relay(stream: TcpStream, sink: Sink) {
    let (read_half, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    if writer
        .write_all(b"220 chaos-sink.invalid ESMTP\r\n")
        .await
        .is_err()
    {
        return;
    }
    let mut rcpt_to: Vec<String> = Vec::new();
    loop {
        let mut line = String::new();
        let Ok(n) = reader.read_line(&mut line).await else {
            return;
        };
        if n == 0 {
            return;
        }
        let upper = line.to_ascii_uppercase();
        if upper.starts_with("EHLO") || upper.starts_with("HELO") {
            let _ = writer
                .write_all(b"250-chaos-sink.invalid\r\n250 8BITMIME\r\n")
                .await;
        } else if upper.starts_with("MAIL FROM:") {
            rcpt_to.clear();
            let _ = writer.write_all(b"250 ok\r\n").await;
        } else if let Some(rest) = upper.strip_prefix("RCPT TO:") {
            let start = line.len() - rest.len();
            rcpt_to.push(
                line[start..]
                    .trim()
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_owned(),
            );
            let _ = writer.write_all(b"250 ok\r\n").await;
        } else if upper.starts_with("DATA") {
            if writer.write_all(b"354 go\r\n").await.is_err() {
                return;
            }
            let mut body = Vec::new();
            loop {
                let mut data_line = Vec::new();
                let Ok(n) = reader.read_until(b'\n', &mut data_line).await else {
                    return;
                };
                if n == 0 {
                    return;
                }
                if data_line == b".\r\n" || data_line == b".\n" {
                    break;
                }
                body.extend_from_slice(if data_line.starts_with(b"..") {
                    &data_line[1..]
                } else {
                    &data_line
                });
            }
            if sink.stall.load(Ordering::SeqCst) {
                // The message is in; the answer never comes. The client is
                // left mid-transaction until the test kills it.
                sink.stalled.add_permits(1);
                std::future::pending::<()>().await;
            }
            sink.deliveries.lock().unwrap().push(Delivery {
                rcpt_to: rcpt_to.clone(),
                data: body,
            });
            let _ = writer.write_all(b"250 2.0.0 accepted\r\n").await;
        } else if upper.starts_with("RSET") {
            rcpt_to.clear();
            let _ = writer.write_all(b"250 ok\r\n").await;
        } else if upper.starts_with("QUIT") {
            let _ = writer.write_all(b"221 bye\r\n").await;
            return;
        } else {
            let _ = writer.write_all(b"500 unrecognized\r\n").await;
        }
    }
}

/// One LMTP transaction to the binary's listener.
async fn lmtp_post(port: u16, subject: &str) {
    let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let (read_half, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    assert!(line.starts_with("220 "), "{line}");
    writer
        .write_all(b"LHLO chaos-client.invalid\r\n")
        .await
        .unwrap();
    loop {
        line.clear();
        reader.read_line(&mut line).await.unwrap();
        if line.starts_with("250 ") {
            break;
        }
    }
    for command in [
        format!("MAIL FROM:<{AUTHOR}>\r\n"),
        format!("RCPT TO:<dev@{DOMAIN}>\r\n"),
        "DATA\r\n".to_owned(),
    ] {
        writer.write_all(command.as_bytes()).await.unwrap();
        line.clear();
        reader.read_line(&mut line).await.unwrap();
        assert!(
            line.starts_with("250") || line.starts_with("354"),
            "{command}: {line}"
        );
    }
    let message = format!(
        "From: {AUTHOR}\r\nTo: dev@{DOMAIN}\r\nMessage-ID: <{}@{DOMAIN}>\r\nSubject: {subject}\r\n\r\n{subject}\r\n.\r\n",
        subject.replace(' ', "-")
    );
    writer.write_all(message.as_bytes()).await.unwrap();
    line.clear();
    reader.read_line(&mut line).await.unwrap();
    assert!(line.starts_with("250"), "DATA: {line}");
    writer.write_all(b"QUIT\r\n").await.unwrap();
}

fn env(dir: &Path, url: &str, web: u16, lmtp: u16, relay: u16) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(key, _)| !key.starts_with("LISTMNGR"))
        .collect();
    for (key, value) in [
        ("LISTMNGR__DATABASE__URL", url.to_owned()),
        (
            "LISTMNGR__SITE__BASE_URL",
            "https://lists.chaos.invalid".into(),
        ),
        ("LISTMNGR__WEB__LISTEN", format!("127.0.0.1:{web}")),
        ("LISTMNGR__MTA__ENABLED", "true".into()),
        ("LISTMNGR__MTA__LMTP_LISTEN", format!("127.0.0.1:{lmtp}")),
        ("LISTMNGR__MTA__SMTP_RELAY", format!("127.0.0.1:{relay}")),
        ("LISTMNGR__MTA__SMTP_TLS", "plaintext_trusted_relay".into()),
        ("LISTMNGR__MTA__LOCAL_HOSTNAME", "chaos.invalid".into()),
        ("LISTMNGR__MTA__COMMAND_TIMEOUT_SECS", "5".into()),
        ("LISTMNGR__MTA__RETRY_INITIAL_SECS", "1".into()),
        ("LISTMNGR__MTA__RETRY_MAX_SECS", "2".into()),
        ("LISTMNGR__MTA__INCOMING", "postfix".into()),
        (
            "LISTMNGR__MTA__MAP_DIRECTORY",
            dir.join("mta").display().to_string(),
        ),
        ("RUST_LOG", "info".into()),
    ] {
        env.push((key.to_owned(), value));
    }
    env
}

fn cli(dir: &Path, env: &[(String, String)], args: &[&str]) -> Value {
    let mut cmd = StdCommand::new(binary());
    cmd.args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in env {
        cmd.env(key, value);
    }
    let output = cmd.output().unwrap();
    assert!(
        output.status.success(),
        "cli {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    serde_json::from_str(stdout.trim()).unwrap_or_else(|_| Value::String(stdout.trim().to_owned()))
}

fn spawn(dir: &Path, env: &[(String, String)]) -> Child {
    let mut cmd = StdCommand::new(binary());
    cmd.arg("serve").current_dir(dir);
    for (stderr, name) in [(false, "server.log"), (true, "server.err.log")] {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(name))
            .unwrap();
        if stderr {
            cmd.stderr(file);
        } else {
            cmd.stdout(file);
        }
    }
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.spawn().unwrap()
}

async fn wait_ready(web: u16, child: &mut Child) {
    let client = reqwest::Client::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            panic!("server exited before readiness: {status:?}");
        }
        if let Ok(response) = client
            .get(format!("http://127.0.0.1:{web}/readyz"))
            .timeout(Duration::from_millis(500))
            .send()
            .await
            && response.status().is_success()
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "readiness timed out"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn until(deadline: Duration, predicate: impl Fn() -> bool) -> bool {
    let end = tokio::time::Instant::now() + deadline;
    loop {
        if predicate() {
            return true;
        }
        if tokio::time::Instant::now() >= end {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// A site with one list and one member on `url`, its server running
/// against a relay at `relay`.
struct Site {
    dir: tempfile::TempDir,
    env: Vec<(String, String)>,
    web: u16,
    lmtp: u16,
    relay: u16,
    sink: Sink,
    child: Option<Child>,
    db: listmngr_db::Database,
}

impl Site {
    async fn start(
        url: Option<String>,
        relay_up: bool,
    ) -> (Self, Option<tokio::task::JoinHandle<()>>) {
        let dir = tempfile::tempdir().unwrap();
        let url = url
            .unwrap_or_else(|| format!("sqlite://{}/chaos.sqlite?mode=rwc", dir.path().display()));
        let (web, lmtp, relay) = (free_port(), free_port(), free_port());
        let env = env(dir.path(), &url, web, lmtp, relay);
        let sink = Sink::new();
        let listener = relay_up.then(|| listen(relay, sink.clone()));
        cli(dir.path(), &env, &["migrate"]);
        cli(dir.path(), &env, &["domains", "add", DOMAIN]);
        cli(
            dir.path(),
            &env,
            &["lists", "create", LIST, "--display-name", "Dev"],
        );
        cli(dir.path(), &env, &["members", "add", LIST, MEMBER]);
        cli(dir.path(), &env, &["members", "add", LIST, AUTHOR]);
        let mut child = spawn(dir.path(), &env);
        wait_ready(web, &mut child).await;
        let db = listmngr_db::Database::connect(&url, 2).await.unwrap();
        (
            Self {
                dir,
                env,
                web,
                lmtp,
                relay,
                sink,
                child: Some(child),
                db,
            },
            listener,
        )
    }

    fn kill(&mut self) {
        let mut child = self.child.take().expect("running");
        child.kill().unwrap();
        child.wait().unwrap();
    }

    async fn restart(&mut self) {
        let mut child = spawn(self.dir.path(), &self.env);
        wait_ready(self.web, &mut child).await;
        self.child = Some(child);
    }

    async fn out_jobs(&self) -> Vec<(String, String, i64)> {
        sqlx::query_as("SELECT id,state,attempts FROM queue_jobs WHERE queue='out' ORDER BY id")
            .fetch_all(self.db.pool())
            .await
            .unwrap()
    }

    async fn recipient_status(&self, job: &str) -> Vec<(String, String)> {
        sqlx::query_as(
            "SELECT email,status FROM delivery_recipients WHERE job_id=$1 ORDER BY email",
        )
        .bind(job)
        .fetch_all(self.db.pool())
        .await
        .unwrap()
    }
}

impl Drop for Site {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Poll `probe` until it answers `true`, up to `deadline`.
async fn eventually<F, Fut>(deadline: Duration, probe: F) -> bool
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let end = tokio::time::Instant::now() + deadline;
    loop {
        if probe().await {
            return true;
        }
        if tokio::time::Instant::now() >= end {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// The server dies while the relay holds its message: the attempt is
/// quarantined as ambiguous, nothing is resent on its own, and the
/// operator's `resolve --outcome retry` delivers exactly once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_runner_killed_mid_smtp_leaves_the_attempt_ambiguous_until_an_operator_retries() {
    let (mut site, listener) = Site::start(None, true).await;
    site.sink.stall.store(true, Ordering::SeqCst);
    lmtp_post(site.lmtp, "killed mid flight").await;
    tokio::time::timeout(Duration::from_secs(20), site.sink.stalled.acquire())
        .await
        .expect("the out runner never reached the relay")
        .unwrap()
        .forget();
    site.kill();
    site.sink.stall.store(false, Ordering::SeqCst);
    assert!(
        site.sink.deliveries().is_empty(),
        "nothing was accepted before the kill"
    );
    let jobs = site.out_jobs().await;
    assert_eq!(jobs.len(), 1, "{jobs:?}");
    let job = jobs[0].0.clone();
    let before = site.recipient_status(&job).await;
    assert!(
        before
            .iter()
            .any(|(email, status)| email == MEMBER && status == "ambiguous"),
        "the in-flight recipient is quarantined: {before:?}"
    );
    site.restart().await;
    // The lease outlives the dead process (20 s); the next claim finds
    // the recipient quarantined and has nothing to send.
    let settled = eventually(Duration::from_secs(90), || async {
        site.out_jobs()
            .await
            .iter()
            .all(|(_, state, _)| state == "done")
    })
    .await;
    let jobs = site.out_jobs().await;
    assert!(settled, "{jobs:?}");
    let after = site.recipient_status(&job).await;
    assert!(
        after
            .iter()
            .any(|(email, status)| email == MEMBER && status == "ambiguous"),
        "{after:?}"
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(site.sink.to_member(), 0, "no copy without an operator");
    let recipients = cli(site.dir.path(), &site.env, &["queue", "recipients", &job]);
    assert!(recipients.to_string().contains("ambiguous"), "{recipients}");
    cli(
        site.dir.path(),
        &site.env,
        &[
            "queue",
            "resolve",
            &job,
            MEMBER,
            "--outcome",
            "retry",
            "--reason",
            "relay log shows no acceptance",
            "--acknowledge-duplicate-risk",
        ],
    );
    assert!(
        until(Duration::from_secs(30), || site.sink.to_member() == 1).await,
        "{:?}",
        site.sink.deliveries()
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(site.sink.to_member(), 1, "exactly once");
    let statuses = site.recipient_status(&job).await;
    assert!(
        statuses
            .iter()
            .any(|(email, status)| email == MEMBER && status == "sent"),
        "{statuses:?}"
    );
    drop(listener);
}

/// The relay is down when the post arrives: the job waits with backoff,
/// nothing is lost, and when the relay comes up the member gets one copy.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_relay_that_is_down_delays_the_delivery_and_never_doubles_it() {
    let (site, _) = Site::start(None, false).await;
    lmtp_post(site.lmtp, "relay down").await;
    let retried = eventually(Duration::from_secs(30), || async {
        site.out_jobs()
            .await
            .iter()
            .any(|(_, state, attempts)| state == "ready" && *attempts >= 1)
    })
    .await;
    let jobs = site.out_jobs().await;
    assert!(retried, "a refused connection schedules a retry: {jobs:?}");
    assert!(site.sink.deliveries().is_empty());
    let listener = listen(site.relay, site.sink.clone());
    assert!(
        until(Duration::from_secs(30), || site.sink.to_member() == 1).await,
        "{:?}",
        site.sink.deliveries()
    );
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(
        site.sink.to_member(),
        1,
        "exactly once after the relay returns"
    );
    let jobs = site.out_jobs().await;
    assert!(jobs.iter().all(|(_, state, _)| state == "done"), "{jobs:?}");
    let delivered = site.sink.deliveries();
    assert!(
        delivered
            .iter()
            .any(|d| d.data.windows(10).any(|w| w == b"relay down")),
        "{delivered:?}"
    );
    drop(listener);
}

/// Every database connection of the server is cut under the runners:
/// the pool reconnects, the server stays ready, and the next post is
/// delivered once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_connections_cut_under_the_runners_are_recovered() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("chaos")
        .await
        .unwrap();
    let url = schema.url.clone();
    let outcome = tokio::spawn(async move {
        let (site, listener) = Site::start(Some(url), true).await;
        lmtp_post(site.lmtp, "before the cut").await;
        assert!(until(Duration::from_secs(30), || site.sink.to_member() == 1).await);
        // Terminate every other backend of this role: the server's pool.
        let cut: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM (SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE usename = current_user AND pid <> pg_backend_pid()) t",
        )
        .fetch_one(site.db.pool())
        .await
        .unwrap();
        assert!(cut >= 1, "the server held connections to cut");
        let ready = eventually(Duration::from_secs(30), || async {
            reqwest::Client::new()
                .get(format!("http://127.0.0.1:{}/readyz", site.web))
                .timeout(Duration::from_millis(800))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
        })
        .await;
        assert!(ready, "the server is ready again after its connections were cut");
        lmtp_post(site.lmtp, "after the cut").await;
        assert!(
            until(Duration::from_secs(60), || site.sink.to_member() == 2).await,
            "{:?}",
            site.sink.deliveries()
        );
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert_eq!(site.sink.to_member(), 2, "one copy each");
        let jobs = site.out_jobs().await;
        assert!(jobs.iter().all(|(_, state, _)| state == "done"), "{jobs:?}");
        site.db.pool().close().await;
        drop(listener);
    })
    .await;
    schema.drop().await.unwrap();
    outcome.unwrap();
}
