//! Load (`P7-LOAD`): the real binary with a list of many members and a
//! burst of posts through LMTP, delivered to a loopback relay that
//! counts recipients — throughput and queue latency measured, never
//! asserted against a number: the plan's targets (10k members, 1k posts
//! an hour) are what the numbers are read against, by hand.
//!
//! `LOAD_MEMBERS` (default 10000) and `LOAD_POSTS` (default 100) size
//! the run; the summary is one JSON line on stdout.
// Counts become rates and percentile ranks: the casts are the point.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
use std::net::TcpListener as StdTcpListener;
use std::path::Path;
use std::process::{Child, Command as StdCommand, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

const fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_listmngr")
}

const DOMAIN: &str = "load.example.invalid";
const LIST: &str = "big.load.example.invalid";
const AUTHOR: &str = "author@load.example.invalid";

fn free_port() -> u16 {
    StdTcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn knob(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// One accepted SMTP transaction: when, how many recipients, which post.
#[derive(Debug, Clone)]
struct Accepted {
    at: Instant,
    recipients: usize,
    post: usize,
}

#[derive(Clone, Default)]
struct Sink {
    accepted: Arc<Mutex<Vec<Accepted>>>,
}

impl Sink {
    fn accepted(&self) -> Vec<Accepted> {
        self.accepted.lock().unwrap().clone()
    }
    fn recipients(&self) -> usize {
        self.accepted().iter().map(|a| a.recipients).sum()
    }
}

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

/// The post number a message carries in its subject (`load N`, behind
/// whatever prefix the list added).
fn post_number(body: &[u8]) -> usize {
    let text = String::from_utf8_lossy(body);
    text.lines()
        .find(|line| line.to_ascii_lowercase().starts_with("subject:"))
        .and_then(|line| line.split_whitespace().last())
        .and_then(|last| last.parse().ok())
        .unwrap_or(0)
}

async fn relay(stream: TcpStream, sink: Sink) {
    let (read_half, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    if writer
        .write_all(b"220 load-sink.invalid ESMTP\r\n")
        .await
        .is_err()
    {
        return;
    }
    let mut recipients = 0usize;
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
                .write_all(b"250-load-sink.invalid\r\n250 8BITMIME\r\n")
                .await;
        } else if upper.starts_with("MAIL FROM:") {
            recipients = 0;
            let _ = writer.write_all(b"250 ok\r\n").await;
        } else if upper.starts_with("RCPT TO:") {
            recipients += 1;
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
                body.extend_from_slice(&data_line);
            }
            sink.accepted.lock().unwrap().push(Accepted {
                at: Instant::now(),
                recipients,
                post: post_number(&body),
            });
            let _ = writer.write_all(b"250 2.0.0 accepted\r\n").await;
        } else if upper.starts_with("RSET") {
            recipients = 0;
            let _ = writer.write_all(b"250 ok\r\n").await;
        } else if upper.starts_with("QUIT") {
            let _ = writer.write_all(b"221 bye\r\n").await;
            return;
        } else {
            let _ = writer.write_all(b"500 unrecognized\r\n").await;
        }
    }
}

/// One LMTP post numbered `n`; returns when the server has answered `250`
/// for `DATA` (the message is durable).
async fn lmtp_post(port: u16, n: usize) {
    let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let (read_half, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    writer
        .write_all(b"LHLO load-client.invalid\r\n")
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
        format!("RCPT TO:<big@{DOMAIN}>\r\n"),
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
        "From: {AUTHOR}\r\nTo: big@{DOMAIN}\r\nMessage-ID: <load-{n}@{DOMAIN}>\r\nSubject: load {n}\r\n\r\nPost number {n} of the load run.\r\n.\r\n"
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
            "https://lists.load.invalid".into(),
        ),
        ("LISTMNGR__WEB__LISTEN", format!("127.0.0.1:{web}")),
        ("LISTMNGR__MTA__ENABLED", "true".into()),
        ("LISTMNGR__MTA__LMTP_LISTEN", format!("127.0.0.1:{lmtp}")),
        ("LISTMNGR__MTA__SMTP_RELAY", format!("127.0.0.1:{relay}")),
        ("LISTMNGR__MTA__SMTP_TLS", "plaintext_trusted_relay".into()),
        ("LISTMNGR__MTA__LOCAL_HOSTNAME", "load.invalid".into()),
        ("LISTMNGR__MTA__COMMAND_TIMEOUT_SECS", "30".into()),
        ("LISTMNGR__MTA__INCOMING", "postfix".into()),
        (
            "LISTMNGR__MTA__MAP_DIRECTORY",
            dir.join("mta").display().to_string(),
        ),
        ("RUST_LOG", "warn".into()),
    ] {
        env.push((key.to_owned(), value));
    }
    env
}

fn cli(dir: &Path, env: &[(String, String)], args: &[&str]) {
    let mut cmd = StdCommand::new(binary());
    cmd.args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
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
}

fn spawn(dir: &Path, env: &[(String, String)]) -> Child {
    let mut cmd = StdCommand::new(binary());
    cmd.arg("serve").current_dir(dir);
    for (stderr, name) in [(false, "server.log"), (true, "server.err.log")] {
        let file = std::fs::File::create(dir.join(name)).unwrap();
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
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
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

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[rank.min(sorted.len() - 1)]
}

/// Members subscribed, posts through LMTP, recipients accepted by the
/// relay: throughput and latency, printed as JSON.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "benchmark: 10k members × 100 posts by default; run by hand and record the numbers"]
async fn a_large_list_takes_a_burst_of_posts_and_the_numbers_are_recorded() {
    let members = knob("LOAD_MEMBERS", 10_000);
    let posts = knob("LOAD_POSTS", 100);
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}/load.sqlite?mode=rwc", dir.path().display());
    let (web, lmtp, relay) = (free_port(), free_port(), free_port());
    let env = env(dir.path(), &url, web, lmtp, relay);
    cli(dir.path(), &env, &["migrate"]);
    cli(dir.path(), &env, &["domains", "add", DOMAIN]);
    cli(
        dir.path(),
        &env,
        &["lists", "create", LIST, "--display-name", "Big"],
    );
    cli(dir.path(), &env, &["members", "add", LIST, AUTHOR]);
    // The roster through the repository, one transaction per member as
    // the API would do, without ten thousand process starts.
    let db = listmngr_db::Database::connect(&url, 2).await.unwrap();
    let list: listmngr_core::ListId = LIST.parse().unwrap();
    let subscribing = Instant::now();
    for n in 0..members {
        db.members()
            .create(listmngr_db::NewMember {
                list_id: list.clone(),
                email: format!("member{n}@{DOMAIN}"),
                display_name: String::new(),
                role: listmngr_core::MemberRole::Member,
                subscription_mode: listmngr_core::SubscriptionMode::AsAddress,
            })
            .await
            .unwrap();
    }
    let subscribe_secs = subscribing.elapsed().as_secs_f64();
    let sink = Sink::default();
    let _listener = listen(relay, sink.clone());
    let mut child = spawn(dir.path(), &env);
    wait_ready(web, &mut child).await;

    // The burst: posts one after another, each durable before the next.
    let started = Instant::now();
    let mut accepted_at = Vec::with_capacity(posts);
    for n in 1..=posts {
        lmtp_post(lmtp, n).await;
        accepted_at.push(Instant::now());
    }
    let intake_secs = started.elapsed().as_secs_f64();
    // The author is a member too: every post reaches members + 1 mailboxes.
    let expected = posts * (members + 1);
    let deadline = Instant::now() + Duration::from_secs(60 * 60);
    while sink.recipients() < expected {
        assert!(
            Instant::now() < deadline,
            "{} of {expected} recipients after an hour",
            sink.recipients()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let delivered_secs = started.elapsed().as_secs_f64();
    let accepted = sink.accepted();
    // Queue latency per post: from the LMTP `250` to the first relay
    // acceptance of that post, and to the last.
    let mut first_ms = Vec::with_capacity(posts);
    let mut last_ms = Vec::with_capacity(posts);
    for n in 1..=posts {
        let at = accepted_at[n - 1];
        let times: Vec<Instant> = accepted
            .iter()
            .filter(|a| a.post == n)
            .map(|a| a.at)
            .collect();
        if let (Some(first), Some(last)) = (times.iter().min(), times.iter().max()) {
            first_ms.push(first.saturating_duration_since(at).as_secs_f64() * 1000.0);
            last_ms.push(last.saturating_duration_since(at).as_secs_f64() * 1000.0);
        }
    }
    first_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    last_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let summary = serde_json::json!({
        "members": members,
        "posts": posts,
        "subscribe_secs": subscribe_secs,
        "intake_secs": intake_secs,
        "intake_posts_per_sec": posts as f64 / intake_secs,
        "delivered_secs": delivered_secs,
        "recipients": sink.recipients(),
        "smtp_transactions": accepted.len(),
        "recipients_per_sec": sink.recipients() as f64 / delivered_secs,
        "posts_per_hour_at_this_rate": posts as f64 / delivered_secs * 3600.0,
        "first_recipient_latency_ms": {"p50": percentile(&first_ms, 0.5), "p95": percentile(&first_ms, 0.95), "max": first_ms.last().copied().unwrap_or(0.0)},
        "last_recipient_latency_ms": {"p50": percentile(&last_ms, 0.5), "p95": percentile(&last_ms, 0.95), "max": last_ms.last().copied().unwrap_or(0.0)},
    });
    println!("{summary}");
    assert_eq!(sink.recipients(), expected, "every recipient exactly once");
    child.kill().unwrap();
    child.wait().unwrap();
    db.pool().close().await;
}
