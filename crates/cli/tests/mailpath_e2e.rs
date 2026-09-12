//! Real production-binary end-to-end mail path: an actual `listmngr serve`
//! child process, a real bound ephemeral LMTP socket, a real SMTP sink
//! socket, and the real durable SQLite-backed queue/moderation DB — no
//! mocked runner or queue. See `docs/FEATURE_PARITY.md` for scope/evidence.
use serde_json::Value;
use std::net::TcpListener as StdTcpListener;
use std::path::Path;
use std::process::{Child, Command as StdCommand, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

const fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_listmngr")
}

fn free_port() -> u16 {
    let listener = StdTcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn base_env() -> Vec<(String, String)> {
    std::env::vars()
        .filter(|(key, _)| !key.starts_with("LISTMNGR"))
        .collect()
}

// ---- A minimal, real, multi-connection SMTP sink (not a stub queue/runner:
// it exercises the real out-runner -> real socket -> real SMTP client path). ----

#[derive(Debug, Clone)]
struct Delivery {
    mail_from: Option<String>,
    rcpt_to: Vec<String>,
    data: Vec<u8>,
}

#[derive(Clone)]
struct Sink {
    deliveries: Arc<Mutex<Vec<Delivery>>>,
    defer_data: Arc<std::sync::atomic::AtomicBool>,
    deferred: Arc<tokio::sync::Semaphore>,
}
impl Sink {
    fn deliveries(&self) -> Vec<Delivery> {
        self.deliveries.lock().unwrap().clone()
    }
}

async fn start_sink() -> (u16, Sink, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let sink = Sink {
        deliveries: Arc::new(Mutex::new(Vec::new())),
        defer_data: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        deferred: Arc::new(tokio::sync::Semaphore::new(0)),
    };
    let accept_sink = sink.clone();
    let task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            tokio::spawn(handle_smtp_connection(stream, accept_sink.clone()));
        }
    });
    (port, sink, task)
}

async fn handle_smtp_connection(stream: TcpStream, sink: Sink) {
    let (read_half, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    if writer
        .write_all(b"220 e2e-sink.invalid ESMTP\r\n")
        .await
        .is_err()
    {
        return;
    }
    let mut mail_from: Option<String> = None;
    let mut rcpt_to: Vec<String> = Vec::new();
    loop {
        let mut line = String::new();
        let Ok(n) = reader.read_line(&mut line).await else {
            return;
        };
        if n == 0 {
            return;
        }
        // Uppercase the *untrimmed* line so its length (and therefore every
        // byte offset computed from it below) stays aligned with `line`.
        let upper = line.to_ascii_uppercase();
        if upper.starts_with("EHLO") || upper.starts_with("LHLO") {
            let _ = writer
                .write_all(b"250-e2e-sink.invalid\r\n250 8BITMIME\r\n")
                .await;
        } else if let Some(rest) = upper.strip_prefix("MAIL FROM:") {
            let start = line.len() - rest.len();
            let value = line[start..].trim();
            mail_from = Some(
                value
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_owned(),
            );
            rcpt_to.clear();
            let _ = writer.write_all(b"250 ok\r\n").await;
        } else if let Some(rest) = upper.strip_prefix("RCPT TO:") {
            let start = line.len() - rest.len();
            let value = line[start..].trim();
            rcpt_to.push(
                value
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_owned(),
            );
            let _ = writer.write_all(b"250 ok\r\n").await;
        } else if upper.starts_with("DATA") {
            if sink.defer_data.load(std::sync::atomic::Ordering::SeqCst) {
                let _ = writer
                    .write_all(b"451 4.3.0 controlled pre-DATA deferral\r\n")
                    .await;
                sink.deferred.add_permits(1);
                return;
            }
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
                let bytes: &[u8] = if data_line.starts_with(b"..") {
                    &data_line[1..]
                } else {
                    &data_line
                };
                body.extend_from_slice(bytes);
            }
            sink.deliveries.lock().unwrap().push(Delivery {
                mail_from: mail_from.clone(),
                rcpt_to: rcpt_to.clone(),
                data: body,
            });
            let _ = writer.write_all(b"250 2.0.0 accepted\r\n").await;
        } else if upper.starts_with("RSET") {
            mail_from = None;
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

// ---- A real, raw LMTP client (not the transport's own test suite: this is
// an independent client speaking the wire protocol to the running binary). ----

struct LmtpResult {
    rcpt_replies: Vec<String>,
    data_replies: Vec<String>,
}

async fn lmtp_deliver(
    port: u16,
    mail_from: Option<&str>,
    rcpts: &[&str],
    data: &[u8],
) -> LmtpResult {
    let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let (read_half, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    let mut greeting = String::new();
    reader.read_line(&mut greeting).await.unwrap();
    assert!(
        greeting.starts_with("220 "),
        "unexpected greeting: {greeting}"
    );
    writer
        .write_all(b"LHLO e2e-client.invalid\r\n")
        .await
        .unwrap();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        if line.starts_with("250 ") {
            break;
        }
        assert!(line.starts_with("250-"), "unexpected LHLO reply: {line}");
    }
    let from = mail_from.map_or_else(|| "<>".to_owned(), |value| format!("<{value}>"));
    writer
        .write_all(format!("MAIL FROM:{from}\r\n").as_bytes())
        .await
        .unwrap();
    let mut mail_reply = String::new();
    reader.read_line(&mut mail_reply).await.unwrap();
    assert!(
        mail_reply.starts_with("250"),
        "MAIL FROM rejected: {mail_reply}"
    );

    let mut rcpt_replies = Vec::with_capacity(rcpts.len());
    for rcpt in rcpts {
        writer
            .write_all(format!("RCPT TO:<{rcpt}>\r\n").as_bytes())
            .await
            .unwrap();
        let mut reply = String::new();
        reader.read_line(&mut reply).await.unwrap();
        rcpt_replies.push(reply);
    }

    let accepted = rcpt_replies
        .iter()
        .filter(|reply| reply.starts_with("250"))
        .count();
    let mut data_replies = Vec::new();
    if accepted > 0 {
        writer.write_all(b"DATA\r\n").await.unwrap();
        let mut data_start = String::new();
        reader.read_line(&mut data_start).await.unwrap();
        assert!(
            data_start.starts_with("354"),
            "DATA not accepted: {data_start}"
        );
        writer.write_all(data).await.unwrap();
        if !data.ends_with(b"\n") {
            writer.write_all(b"\r\n").await.unwrap();
        }
        writer.write_all(b".\r\n").await.unwrap();
        for _ in 0..accepted {
            let mut reply = String::new();
            reader.read_line(&mut reply).await.unwrap();
            data_replies.push(reply);
        }
    }
    writer.write_all(b"QUIT\r\n").await.unwrap();
    LmtpResult {
        rcpt_replies,
        data_replies,
    }
}

async fn wait_until<F: Fn() -> bool>(deadline: Duration, predicate: F) -> bool {
    let end = tokio::time::Instant::now() + deadline;
    loop {
        if predicate() {
            return true;
        }
        if tokio::time::Instant::now() >= end {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ---- Production binary lifecycle ----

fn run_cli(dir: &Path, env: &[(String, String)], args: &[&str], stdin: Option<&str>) -> Value {
    let mut cmd = StdCommand::new(binary());
    cmd.args(args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in env {
        cmd.env(key, value);
    }
    let mut child = cmd.spawn().unwrap();
    if let Some(input) = stdin {
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    } else {
        drop(child.stdin.take());
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "cli {args:?} failed (exit {:?}): {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    serde_json::from_str(stdout.trim()).unwrap_or_else(|_| Value::String(stdout.trim().to_owned()))
}

fn spawn_server(dir: &Path, env: &[(String, String)]) -> Child {
    let mut cmd = StdCommand::new(binary());
    cmd.arg("serve").current_dir(dir);
    // Per-fixture logs cannot collide across parallel tests. Only disposable
    // fixture data is used by these child processes.
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

async fn wait_ready(web_port: u16, child: &mut Child) {
    let client = reqwest::Client::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            panic!("server exited before readiness: {status:?}");
        }
        if let Ok(response) = client
            .get(format!("http://127.0.0.1:{web_port}/readyz"))
            .timeout(Duration::from_millis(500))
            .send()
            .await
            && response.status().is_success()
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "server readiness timed out"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    env: Vec<(String, String)>,
    web_port: u16,
    lmtp_port: u16,
    sink: Sink,
    sink_task: tokio::task::JoinHandle<()>,
    child: Option<Child>,
    admin_token: String,
    list: String,
}

impl Fixture {
    async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let web_port = free_port();
        let lmtp_port = free_port();
        let (sink_port, sink, sink_task) = start_sink().await;
        let mut env = base_env();
        env.push((
            "LISTMNGR__DATABASE__URL".into(),
            format!("sqlite://{}/e2e.sqlite?mode=rwc", dir.path().display()),
        ));
        env.push((
            "LISTMNGR__WEB__LISTEN".into(),
            format!("127.0.0.1:{web_port}"),
        ));
        env.push(("LISTMNGR__MTA__ENABLED".into(), "true".into()));
        env.push((
            "LISTMNGR__MTA__LMTP_LISTEN".into(),
            format!("127.0.0.1:{lmtp_port}"),
        ));
        env.push((
            "LISTMNGR__MTA__SMTP_RELAY".into(),
            format!("127.0.0.1:{sink_port}"),
        ));
        env.push((
            "LISTMNGR__MTA__SMTP_TLS".into(),
            "plaintext_trusted_relay".into(),
        ));
        env.push(("LISTMNGR__MTA__LOCAL_HOSTNAME".into(), "e2e.invalid".into()));
        env.push(("LISTMNGR__MTA__COMMAND_TIMEOUT_SECS".into(), "5".into()));
        // The server publishes Postfix maps at startup and after list changes.
        env.push(("LISTMNGR__MTA__INCOMING".into(), "postfix".into()));
        env.push((
            "LISTMNGR__MTA__MAP_DIRECTORY".into(),
            dir.path().join("mta").display().to_string(),
        ));
        env.push(("RUST_LOG".into(), "warn".into()));

        run_cli(dir.path(), &env, &["migrate"], None);
        run_cli(
            dir.path(),
            &env,
            &["domains", "add", "e2e.example.invalid"],
            None,
        );
        let list = "dev.e2e.example.invalid";
        run_cli(
            dir.path(),
            &env,
            &["lists", "create", list, "--display-name", "Dev"],
            None,
        );
        let user = run_cli(
            dir.path(),
            &env,
            &[
                "user",
                "create",
                "admin@e2e.example.invalid",
                "--display-name",
                "Admin",
                "--password-stdin",
                "--server-owner",
            ],
            Some("E2ePassw0rd!Sentinel\n"),
        );
        let user_id = user["id"].as_str().unwrap().to_owned();
        let admin_token = run_cli(
            dir.path(),
            &env,
            &["token", "create", &user_id, "e2e", "--scopes", "admin"],
            None,
        );
        let admin_token = match admin_token {
            Value::String(token) => token,
            other => other.to_string(),
        };

        let mut child = spawn_server(dir.path(), &env);
        wait_ready(web_port, &mut child).await;

        Self {
            dir,
            env,
            web_port,
            lmtp_port,
            sink,
            sink_task,
            child: Some(child),
            admin_token,
            list: list.to_owned(),
        }
    }

    fn add_member(&self, email: &str) -> Value {
        run_cli(
            self.dir.path(),
            &self.env,
            &["members", "add", &self.list, email],
            None,
        )
    }

    async fn set_member_delivery_status(&self, member_id: &str, status: &str) {
        let client = reqwest::Client::new();
        let response = client
            .put(format!(
                "http://127.0.0.1:{}/api/v1/members/{member_id}/preferences",
                self.web_port
            ))
            .bearer_auth(&self.admin_token)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(serde_json::to_vec(&serde_json::json!({ "delivery_status": status })).unwrap())
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "set preferences failed: {}",
            response.status()
        );
    }

    async fn held_count(&self) -> u64 {
        let client = reqwest::Client::new();
        let response = client
            .get(format!(
                "http://127.0.0.1:{}/3.1/lists/{}/held/count",
                self.web_port, self.list
            ))
            .bearer_auth(&self.admin_token)
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        let bytes = response.bytes().await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        body["count"].as_u64().unwrap()
    }

    async fn held_entries(&self) -> Vec<Value> {
        let client = reqwest::Client::new();
        let response = client
            .get(format!(
                "http://127.0.0.1:{}/3.1/lists/{}/held",
                self.web_port, self.list
            ))
            .bearer_auth(&self.admin_token)
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        let bytes = response.bytes().await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        body["entries"].as_array().cloned().unwrap_or_default()
    }

    async fn moderate(&self, held_id: &str, action: &str, token: &str) -> reqwest::StatusCode {
        let client = reqwest::Client::new();
        let response = client
            .post(format!(
                "http://127.0.0.1:{}/3.1/lists/{}/held/{held_id}",
                self.web_port, self.list
            ))
            .bearer_auth(token)
            .form(&[("action", action)])
            .send()
            .await
            .unwrap();
        response.status()
    }

    async fn restart_and_wait_ready(&mut self) {
        let mut child = self.child.take().expect("server already stopped");
        child.kill().unwrap();
        child.wait().unwrap();
        let mut child = spawn_server(self.dir.path(), &self.env);
        wait_ready(self.web_port, &mut child).await;
        self.child = Some(child);
    }

    fn db_url(&self) -> String {
        format!("sqlite://{}/e2e.sqlite?mode=rwc", self.dir.path().display())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.sink_task.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn member_delivery_reaches_enabled_member_and_excludes_disabled_member() {
    let fixture = Fixture::start().await;
    // Startup published the MTA maps for the list created before it ran.
    let transport =
        std::fs::read_to_string(fixture.dir.path().join("mta/current/transport.regexp")).unwrap();
    assert!(
        transport.contains(&format!(
            "/^dev@e2e\\.example\\.invalid$/ lmtp:[127.0.0.1]:{}\n",
            fixture.lmtp_port
        )),
        "{transport}"
    );
    let sender = fixture.add_member("sender@e2e.example.invalid");
    let _ = sender;
    let enabled = fixture.add_member("enabled@e2e.example.invalid");
    let disabled = fixture.add_member("disabled@e2e.example.invalid");
    fixture
        .set_member_delivery_status(disabled["id"].as_str().unwrap(), "by_bounces")
        .await;

    let posting_address = "dev@e2e.example.invalid".to_owned();
    let raw =
        b"From: sender@e2e.example.invalid\r\nTo: dev@e2e.example.invalid\r\nMessage-ID: <e2e-member@example.invalid>\r\nSubject: hello list\r\n\r\nreal body\r\n";
    let result = lmtp_deliver(
        fixture.lmtp_port,
        Some("sender@e2e.example.invalid"),
        &[&posting_address],
        raw,
    )
    .await;
    assert!(
        result.rcpt_replies[0].starts_with("250"),
        "got {:?}",
        result.rcpt_replies
    );
    assert!(
        result.data_replies[0].starts_with("250"),
        "durable intake must accept before 250: {:?}",
        result.data_replies
    );

    let delivered = wait_until(Duration::from_secs(10), || {
        fixture.sink.deliveries().iter().any(|delivery| {
            delivery
                .rcpt_to
                .contains(&"enabled@e2e.example.invalid".to_owned())
        })
    })
    .await;
    assert!(delivered, "enabled member never received the post");

    // Give any (incorrect) delivery to the disabled member a further bounded
    // window to appear, then assert it never does.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let deliveries = fixture.sink.deliveries();
    assert!(
        !deliveries.iter().any(|delivery| delivery
            .rcpt_to
            .contains(&"disabled@e2e.example.invalid".to_owned())),
        "disabled member must never receive delivery, got {deliveries:?}"
    );
    let enabled_deliveries: Vec<_> = deliveries
        .iter()
        .filter(|delivery| {
            delivery
                .rcpt_to
                .contains(&"enabled@e2e.example.invalid".to_owned())
        })
        .collect();
    assert_eq!(
        enabled_deliveries.len(),
        1,
        "must be delivered exactly once"
    );
    assert!(
        String::from_utf8_lossy(&enabled_deliveries[0].data).contains("real body"),
        "delivered body must contain the real content"
    );
    assert_eq!(
        enabled_deliveries[0].mail_from.as_deref(),
        Some("dev-bounces@e2e.example.invalid"),
        "envelope sender must be the list's own bounces address, not the original poster"
    );
    let _ = enabled;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nonmember_post_is_held_not_delivered() {
    let fixture = Fixture::start().await;
    fixture.add_member("member@e2e.example.invalid");
    let posting_address = "dev@e2e.example.invalid".to_owned();
    let raw =
        b"From: outsider@attacker.invalid\r\nTo: dev@e2e.example.invalid\r\nMessage-ID: <e2e-nonmember@example.invalid>\r\nSubject: outside post\r\n\r\nbody\r\n";
    let result = lmtp_deliver(
        fixture.lmtp_port,
        Some("outsider@attacker.invalid"),
        &[&posting_address],
        raw,
    )
    .await;
    assert!(result.data_replies[0].starts_with("250"));

    let mut count = 0;
    for _ in 0..100 {
        count = fixture.held_count().await;
        if count == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(count, 1, "nonmember post must be held exactly once");

    tokio::time::sleep(Duration::from_millis(500)).await;
    // The held post itself is never delivered; the only traffic is the
    // Mailman hold notice back to the poster (respond_to_post_requests).
    let deliveries = fixture.sink.deliveries();
    assert!(
        deliveries
            .iter()
            .all(|delivery| delivery.rcpt_to == ["outsider@attacker.invalid"]),
        "a held message must never be delivered: {deliveries:?}"
    );
    assert!(
        deliveries
            .iter()
            .all(|delivery| String::from_utf8_lossy(&delivery.data)
                .contains("awaits moderator approval")),
        "only the hold notice may leave: {deliveries:?}"
    );
}

/// A real, second user's token with a deliberately wrong scope (`lists:read`,
/// not `moderation`): the CLI only issues unscoped/admin-style tokens, so
/// this is the simplest real way to prove scope enforcement end-to-end over HTTP.
fn create_weak_scoped_token(fixture: &Fixture) -> String {
    let other_user = run_cli(
        fixture.dir.path(),
        &fixture.env,
        &[
            "user",
            "create",
            "other-owner@e2e.example.invalid",
            "--display-name",
            "Other",
            "--password-stdin",
        ],
        Some("AnotherPassw0rd!Sentinel\n"),
    );
    let other_user_id = other_user["id"].as_str().unwrap();
    let token = run_cli(
        fixture.dir.path(),
        &fixture.env,
        &[
            "token",
            "create",
            other_user_id,
            "weak",
            "--scopes",
            "lists:read",
        ],
        None,
    );
    match token {
        Value::String(token) => token,
        other => other.to_string(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unauthorized_cross_list_action_is_rejected_and_authorized_accept_delivers_once() {
    let fixture = Fixture::start().await;
    let member = fixture.add_member("member@e2e.example.invalid");
    let posting_address = "dev@e2e.example.invalid".to_owned();
    let raw =
        b"From: outsider@attacker.invalid\r\nTo: dev@e2e.example.invalid\r\nMessage-ID: <e2e-held-accept@example.invalid>\r\nSubject: needs approval\r\n\r\nbody\r\n";
    lmtp_deliver(
        fixture.lmtp_port,
        Some("outsider@attacker.invalid"),
        &[&posting_address],
        raw,
    )
    .await;

    let mut entries = Vec::new();
    for _ in 0..100 {
        entries = fixture.held_entries().await;
        if !entries.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(entries.len(), 1);
    let held_id = entries[0]["request_id"].as_str().unwrap().to_owned();

    let weak_token = create_weak_scoped_token(&fixture);
    let status = fixture.moderate(&held_id, "accept", &weak_token).await;
    assert_eq!(status, reqwest::StatusCode::FORBIDDEN);
    assert_eq!(
        fixture.held_count().await,
        1,
        "unauthorized action must not write"
    );

    let status = fixture
        .moderate(&held_id, "accept", &fixture.admin_token)
        .await;
    assert_eq!(status, reqwest::StatusCode::NO_CONTENT);

    let delivered = wait_until(Duration::from_secs(10), || {
        fixture.sink.deliveries().iter().any(|delivery| {
            delivery
                .rcpt_to
                .contains(&"member@e2e.example.invalid".to_owned())
        })
    })
    .await;
    assert!(delivered, "accepted held message must be delivered");

    // Replay: a second accept call must fail and never duplicate delivery.
    let status = fixture
        .moderate(&held_id, "accept", &fixture.admin_token)
        .await;
    assert!(
        !status.is_success(),
        "replayed accept must not succeed, got {status}"
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    let count = fixture
        .sink
        .deliveries()
        .iter()
        .filter(|delivery| {
            delivery
                .rcpt_to
                .contains(&"member@e2e.example.invalid".to_owned())
        })
        .count();
    assert_eq!(count, 1, "replay must never duplicate delivery");
    let _ = member;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn durable_intake_survives_a_real_process_restart() {
    let mut fixture = Fixture::start().await;
    fixture.add_member("member@e2e.example.invalid");
    let posting_address = "dev@e2e.example.invalid".to_owned();
    let raw = b"From: member@e2e.example.invalid\r\nTo: dev@e2e.example.invalid\r\nMessage-ID: <e2e-restart@example.invalid>\r\nSubject: restart test\r\n\r\nbody\r\n";
    // A real SMTP boundary defers before accepting any DATA. The durable
    // out job must be pending, not an already-delivered message row.
    fixture
        .sink
        .defer_data
        .store(true, std::sync::atomic::Ordering::SeqCst);
    lmtp_deliver(
        fixture.lmtp_port,
        Some("member@e2e.example.invalid"),
        &[&posting_address],
        raw,
    )
    .await;

    let db = listmngr_db::Database::connect(&fixture.db_url(), 2)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), fixture.sink.deferred.acquire())
        .await
        .expect("out runner never reached controlled relay")
        .unwrap()
        .forget();
    let mut found = false;
    for _ in 0..100 {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM queue_jobs WHERE queue='out' AND state='ready'",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        if count >= 1 {
            found = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if !found {
        let jobs: Vec<(String, String, i64, String)> =
            sqlx::query_as("SELECT queue,state,attempts,last_error FROM queue_jobs ORDER BY queue")
                .fetch_all(db.pool())
                .await
                .unwrap();
        let recipients: Vec<(String, String)> =
            sqlx::query_as("SELECT status,detail FROM delivery_recipients")
                .fetch_all(db.pool())
                .await
                .unwrap();
        panic!(
            "durable pending out job must exist before restart; jobs={jobs:?}; recipients={recipients:?}; deliveries={}; stdout={}; stderr={}",
            fixture.sink.deliveries().len(),
            std::fs::read_to_string(fixture.dir.path().join("server.log")).unwrap(),
            std::fs::read_to_string(fixture.dir.path().join("server.err.log")).unwrap(),
        );
    }
    assert!(
        fixture.sink.deliveries().is_empty(),
        "pre-restart delivery invalidates the proof"
    );
    let stored: Vec<u8> =
        sqlx::query_scalar("SELECT raw FROM messages JOIN message_blobs USING (store_key)")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(stored, raw);

    fixture.restart_and_wait_ready().await;
    assert!(
        fixture.sink.deliveries().is_empty(),
        "gate must remain closed through restart"
    );
    fixture
        .sink
        .defer_data
        .store(false, std::sync::atomic::Ordering::SeqCst);

    let delivered = wait_until(Duration::from_secs(15), || {
        fixture.sink.deliveries().iter().any(|delivery| {
            delivery
                .rcpt_to
                .contains(&"member@e2e.example.invalid".to_owned())
        })
    })
    .await;
    assert!(
        delivered,
        "durably queued delivery must complete after restart against the same DB"
    );
    assert_eq!(fixture.sink.deliveries().len(), 1);
    let stored_after: Vec<u8> =
        sqlx::query_scalar("SELECT raw FROM messages JOIN message_blobs USING (store_key)")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(stored_after, raw);
    db.pool().close().await;
}
