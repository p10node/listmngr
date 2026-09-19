//! Real production-binary end-to-end mail path: an actual `listmngr serve`
//! child process, a real bound ephemeral LMTP socket, a real SMTP sink
//! socket, and the real durable SQLite-backed queue/moderation DB — no
//! mocked runner or queue. See `docs/FEATURE_PARITY.md` for scope/evidence.
use base64::Engine as _;
use mail_auth::common::parse::TxtRecordParser as _;
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

#[derive(Debug)]
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

/// A DKIM key the fixture signs with, and the TXT record that verifies it.
struct DkimFixture {
    record: String,
}

const DKIM_SELECTOR: &str = "e2e";

/// Generate an RSA key with the system OpenSSL, as the DKIM unit tests do,
/// and return the TOML that makes the outgoing runner sign with it.
fn dkim_key(dir: &Path) -> (String, DkimFixture) {
    let key = dir.join("dkim-e2e.pem");
    assert!(
        StdCommand::new("openssl")
            .args([
                "genpkey",
                "-algorithm",
                "RSA",
                "-pkeyopt",
                "rsa_keygen_bits:2048",
                "-out"
            ])
            .arg(&key)
            .output()
            .unwrap()
            .status
            .success()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let public = StdCommand::new("openssl")
        .args(["pkey", "-in"])
        .arg(&key)
        .args(["-pubout", "-outform", "DER"])
        .output()
        .unwrap();
    let record = format!(
        "v=DKIM1; k=rsa; p={}",
        base64::engine::general_purpose::STANDARD.encode(public.stdout)
    );
    let toml = format!(
        "[[mta.dkim_signing]]\ndomain = \"e2e.example.invalid\"\nselector = \"{DKIM_SELECTOR}\"\nprivate_key_file = \"{}\"\n",
        key.display()
    );
    (toml, DkimFixture { record })
}

/// The child's configuration environment: the fixture database, the bound
/// ports, the plaintext trusted relay, Postfix maps in the fixture directory.
fn child_env(
    dir: &Path,
    web_port: u16,
    lmtp_port: u16,
    sink_port: u16,
    extra: &[(&str, &str)],
) -> Vec<(String, String)> {
    let mut env = base_env();
    let settings = [
        (
            "LISTMNGR__DATABASE__URL",
            format!("sqlite://{}/e2e.sqlite?mode=rwc", dir.display()),
        ),
        (
            "LISTMNGR__SITE__BASE_URL",
            "https://lists.e2e.invalid".into(),
        ),
        ("LISTMNGR__WEB__LISTEN", format!("127.0.0.1:{web_port}")),
        ("LISTMNGR__MTA__ENABLED", "true".into()),
        (
            "LISTMNGR__MTA__LMTP_LISTEN",
            format!("127.0.0.1:{lmtp_port}"),
        ),
        (
            "LISTMNGR__MTA__SMTP_RELAY",
            format!("127.0.0.1:{sink_port}"),
        ),
        ("LISTMNGR__MTA__SMTP_TLS", "plaintext_trusted_relay".into()),
        ("LISTMNGR__MTA__LOCAL_HOSTNAME", "e2e.invalid".into()),
        ("LISTMNGR__MTA__COMMAND_TIMEOUT_SECS", "5".into()),
        // The server publishes Postfix maps at startup and after list changes.
        ("LISTMNGR__MTA__INCOMING", "postfix".into()),
        (
            "LISTMNGR__MTA__MAP_DIRECTORY",
            dir.join("mta").display().to_string(),
        ),
        ("RUST_LOG", "warn".into()),
    ];
    env.extend(
        settings
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value)),
    );
    env.extend(
        extra
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned())),
    );
    env
}

impl Fixture {
    async fn start() -> Self {
        Self::start_with(false, &[]).await.0
    }

    /// Start the fixture; with `dkim` the server signs outgoing mail for the
    /// list domain and the matching public record is returned. `extra` adds
    /// configuration environment for the child.
    async fn start_with(dkim: bool, extra: &[(&str, &str)]) -> (Self, Option<DkimFixture>) {
        let dir = tempfile::tempdir().unwrap();
        let web_port = free_port();
        let lmtp_port = free_port();
        let (sink_port, sink, sink_task) = start_sink().await;
        let mut env = child_env(dir.path(), web_port, lmtp_port, sink_port, extra);
        let dkim = dkim.then(|| {
            let (toml, fixture) = dkim_key(dir.path());
            let path = dir.path().join("listmngr.toml");
            std::fs::write(&path, toml).unwrap();
            env.push(("LISTMNGR_CONFIG".into(), path.display().to_string()));
            fixture
        });
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

        (
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
            },
            dkim,
        )
    }

    /// `PATCH` the list configuration through the typed API.
    async fn patch_config(&self, body: Value) {
        let response = reqwest::Client::new()
            .patch(format!(
                "http://127.0.0.1:{}/api/v1/lists/{}/config",
                self.web_port, self.list
            ))
            .bearer_auth(&self.admin_token)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(serde_json::to_vec(&body).unwrap())
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "config patch failed: {} {}",
            response.status(),
            response.text().await.unwrap_or_default()
        );
    }

    /// Store an inline English template body on the list.
    async fn put_template(&self, name: &str, body: &str) {
        let response = reqwest::Client::new()
            .put(format!(
                "http://127.0.0.1:{}/api/v1/lists/{}/templates/{name}",
                self.web_port, self.list
            ))
            .bearer_auth(&self.admin_token)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(serde_json::to_vec(&serde_json::json!({"language": "en", "body": body})).unwrap())
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success(), "{}", response.status());
    }

    /// Ban a sender from the list through the API.
    async fn ban(&self, email: &str) {
        let response = reqwest::Client::new()
            .post(format!(
                "http://127.0.0.1:{}/api/v1/lists/{}/bans",
                self.web_port, self.list
            ))
            .bearer_auth(&self.admin_token)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(serde_json::to_vec(&serde_json::json!({"email": email})).unwrap())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::CREATED);
    }

    /// Wait for `predicate` over the sink's deliveries; returns the deliveries.
    async fn deliveries_when(
        &self,
        deadline: Duration,
        predicate: impl Fn(&[Delivery]) -> bool + Send + Sync,
    ) -> Vec<Delivery> {
        let satisfied = wait_until(deadline, || predicate(&self.sink.deliveries())).await;
        let deliveries = self.sink.deliveries();
        assert!(
            satisfied,
            "sink never satisfied the expectation: {deliveries:?}"
        );
        deliveries
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

/// The value of one header in a delivered message, unfolded.
fn header(data: &[u8], name: &str) -> Option<String> {
    listmngr_mail::header_value(data, name)
}

fn post(from: &str, subject: &str, body: &str) -> Vec<u8> {
    format!(
        "From: {from}\r\nTo: dev@e2e.example.invalid\r\nMessage-ID: <{}@example.invalid>\r\nSubject: {subject}\r\n\r\n{body}\r\n",
        uuid::Uuid::now_v7()
    )
    .into_bytes()
}

/// Phase 2 acceptance: the delivered copy carries the `List-*` headers, the
/// subject prefix and the footer, and its DKIM signature verifies against
/// the fixture's public key.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delivered_post_has_list_headers_prefix_footer_and_a_verifiable_dkim_signature() {
    let (fixture, dkim) = Fixture::start_with(true, &[]).await;
    let dkim = dkim.unwrap();
    fixture.add_member("author@e2e.example.invalid");
    fixture.add_member("reader@e2e.example.invalid");
    fixture
        .patch_config(serde_json::json!({"subject_prefix": "[Dev] "}))
        .await;
    fixture
        .put_template(
            "list:member:regular:footer",
            "-- \ne2e footer for $display_name\n",
        )
        .await;
    let raw = post("author@e2e.example.invalid", "hello list", "real body");
    let result = lmtp_deliver(
        fixture.lmtp_port,
        Some("author@e2e.example.invalid"),
        &["dev@e2e.example.invalid"],
        &raw,
    )
    .await;
    assert!(result.data_replies[0].starts_with("250"), "{result:?}");
    let deliveries = fixture
        .deliveries_when(Duration::from_secs(15), |all| {
            all.iter()
                .any(|d| d.rcpt_to.contains(&"reader@e2e.example.invalid".to_owned()))
        })
        .await;
    let delivered = deliveries
        .iter()
        .find(|d| d.rcpt_to.contains(&"reader@e2e.example.invalid".to_owned()))
        .unwrap();
    let data = &delivered.data;
    assert_eq!(header(data, "Subject").as_deref(), Some("[Dev] hello list"));
    assert_eq!(
        header(data, "List-Id").as_deref(),
        Some("<dev.e2e.example.invalid>")
    );
    assert_eq!(
        header(data, "List-Post").as_deref(),
        Some("<mailto:dev@e2e.example.invalid>")
    );
    // The shared (non-personalized) copy carries the mailto forms; the
    // per-recipient RFC 8058 pair is asserted on personalized copies below.
    let unsubscribe = header(data, "List-Unsubscribe").unwrap();
    assert!(
        unsubscribe.contains("<mailto:dev-leave@e2e.example.invalid>"),
        "{unsubscribe}"
    );
    assert_eq!(
        header(data, "List-Archive").as_deref(),
        Some("<https://lists.e2e.invalid/archives/list/dev.e2e.example.invalid/>")
    );
    assert_eq!(
        header(data, "Precedence").as_deref(),
        Some("list"),
        "{}",
        String::from_utf8_lossy(data)
    );
    let text = String::from_utf8_lossy(data);
    assert!(text.contains("real body"), "{text}");
    assert!(text.contains("e2e footer for Dev"), "{text}");
    let signature = header(data, "DKIM-Signature").expect("signed");
    assert!(
        signature.contains("d=e2e.example.invalid")
            && signature.contains(&format!("s={DKIM_SELECTOR}")),
        "{signature}"
    );
    // Verify the signature the way a receiving MTA would, with the public
    // record served from a seeded cache instead of DNS.
    let cache = listmngr_mail::authenticity::TxtCache::default();
    cache.seed(
        &format!("{DKIM_SELECTOR}._domainkey.e2e.example.invalid."),
        mail_auth::Txt::DomainKey(Arc::new(
            mail_auth::common::verify::DomainKey::parse(dkim.record.as_bytes()).unwrap(),
        )),
    );
    let verifier = listmngr_mail::authenticity::Verifier::system("mx.e2e.invalid")
        .unwrap()
        .with_txt_cache(cache);
    let verdict = verifier
        .verify(data, delivered.mail_from.as_deref(), None)
        .await;
    let results = verdict.header.expect("Authentication-Results");
    assert!(results.contains("dkim=pass"), "{results}");
}

/// Phase 2 acceptance: a banned sender is rejected with a notice and the
/// members never see the post.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn banned_sender_is_rejected_with_a_notice_and_never_delivered() {
    let fixture = Fixture::start().await;
    fixture.add_member("reader@e2e.example.invalid");
    fixture.ban("banned@attacker.invalid").await;
    let raw = post("banned@attacker.invalid", "spam", "buy now");
    let result = lmtp_deliver(
        fixture.lmtp_port,
        Some("banned@attacker.invalid"),
        &["dev@e2e.example.invalid"],
        &raw,
    )
    .await;
    assert!(result.data_replies[0].starts_with("250"), "{result:?}");
    let deliveries = fixture
        .deliveries_when(Duration::from_secs(15), |all| {
            all.iter()
                .any(|d| d.rcpt_to == ["banned@attacker.invalid".to_owned()])
        })
        .await;
    let notice = deliveries
        .iter()
        .find(|d| d.rcpt_to == ["banned@attacker.invalid".to_owned()])
        .unwrap();
    let text = String::from_utf8_lossy(&notice.data);
    assert!(text.contains("is banned from this list"), "{text}");
    assert_eq!(
        header(&notice.data, "Subject").as_deref(),
        Some("Request to mailing list \"Dev\" rejected")
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !fixture
            .sink
            .deliveries()
            .iter()
            .any(|d| d.rcpt_to.contains(&"reader@e2e.example.invalid".to_owned())),
        "{:?}",
        fixture.sink.deliveries()
    );
    assert_eq!(fixture.held_count().await, 0);
}

/// Phase 2 acceptance: a post over `max_message_size` is held, not delivered.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn oversized_post_is_held() {
    let fixture = Fixture::start().await;
    fixture.add_member("author@e2e.example.invalid");
    fixture.add_member("reader@e2e.example.invalid");
    fixture
        .patch_config(serde_json::json!({"max_message_size": 1}))
        .await;
    let raw = post("author@e2e.example.invalid", "big", &"x".repeat(3 * 1024));
    let result = lmtp_deliver(
        fixture.lmtp_port,
        Some("author@e2e.example.invalid"),
        &["dev@e2e.example.invalid"],
        &raw,
    )
    .await;
    assert!(result.data_replies[0].starts_with("250"), "{result:?}");
    let end = tokio::time::Instant::now() + Duration::from_secs(15);
    while fixture.held_count().await != 1 {
        assert!(tokio::time::Instant::now() < end, "the post was never held");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let entries = fixture.held_entries().await;
    assert!(
        entries[0]["reason"]
            .as_str()
            .unwrap()
            .contains("max_message_size"),
        "{entries:?}"
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !fixture
            .sink
            .deliveries()
            .iter()
            .any(|d| d.rcpt_to.contains(&"reader@e2e.example.invalid".to_owned()))
    );
}

/// Phase 2 acceptance: `personalize = full` with Mailman's
/// `verp_personalized_deliveries` sends one message per member with a VERP
/// envelope naming that member and the member's own one-click unsubscribe.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn personalize_full_sends_one_verp_message_per_member() {
    let (fixture, _) = Fixture::start_with(
        false,
        &[("LISTMNGR__MTA__VERP_PERSONALIZED_DELIVERIES", "true")],
    )
    .await;
    fixture.add_member("author@e2e.example.invalid");
    fixture.add_member("one@e2e.example.invalid");
    fixture.add_member("two@other.invalid");
    fixture
        .patch_config(serde_json::json!({"personalize": "full"}))
        .await;
    let raw = post("author@e2e.example.invalid", "personal", "for you");
    let result = lmtp_deliver(
        fixture.lmtp_port,
        Some("author@e2e.example.invalid"),
        &["dev@e2e.example.invalid"],
        &raw,
    )
    .await;
    assert!(result.data_replies[0].starts_with("250"), "{result:?}");
    let deliveries = fixture
        .deliveries_when(Duration::from_secs(15), |all| all.len() >= 3)
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let deliveries = if fixture.sink.deliveries().len() > deliveries.len() {
        fixture.sink.deliveries()
    } else {
        deliveries
    };
    assert_eq!(deliveries.len(), 3, "{deliveries:?}");
    let mut envelopes: Vec<(String, String)> = deliveries
        .iter()
        .map(|d| {
            assert_eq!(d.rcpt_to.len(), 1, "one recipient per message: {d:?}");
            (d.mail_from.clone().unwrap(), d.rcpt_to[0].clone())
        })
        .collect();
    envelopes.sort();
    assert_eq!(
        envelopes,
        [
            (
                "dev-bounces+author=e2e.example.invalid@e2e.example.invalid".to_owned(),
                "author@e2e.example.invalid".to_owned()
            ),
            (
                "dev-bounces+one=e2e.example.invalid@e2e.example.invalid".to_owned(),
                "one@e2e.example.invalid".to_owned()
            ),
            (
                "dev-bounces+two=other.invalid@e2e.example.invalid".to_owned(),
                "two@other.invalid".to_owned()
            ),
        ]
    );
    for delivery in &deliveries {
        assert_eq!(
            header(&delivery.data, "To").as_deref(),
            Some(delivery.rcpt_to[0].as_str()),
            "full personalization addresses each copy to its member"
        );
        assert_eq!(
            header(&delivery.data, "List-Unsubscribe-Post").as_deref(),
            Some("List-Unsubscribe=One-Click")
        );
        let unsubscribe = header(&delivery.data, "List-Unsubscribe").unwrap();
        assert!(
            unsubscribe.starts_with("<https://lists.e2e.invalid/")
                && unsubscribe.ends_with("<mailto:dev-leave@e2e.example.invalid>"),
            "{unsubscribe}"
        );
    }
}

/// Phase 2 acceptance: a post accepted while no server runs — the queue is
/// durable — is delivered exactly once when the server starts, and a
/// server killed with SIGKILL between the pipeline and the relay's answer
/// (see `durable_intake_survives_a_real_process_restart`) never
/// double-delivers.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn post_injected_while_stopped_is_delivered_exactly_once_after_start() {
    let mut fixture = Fixture::start().await;
    fixture.add_member("author@e2e.example.invalid");
    fixture.add_member("reader@e2e.example.invalid");
    let mut child = fixture.child.take().unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    let file = fixture.dir.path().join("stopped.eml");
    std::fs::write(
        &file,
        post("author@e2e.example.invalid", "while stopped", "queued"),
    )
    .unwrap();
    let job = run_cli(
        fixture.dir.path(),
        &fixture.env,
        &[
            "queue",
            "inject",
            &fixture.list,
            file.to_str().unwrap(),
            "--sender",
            "author@e2e.example.invalid",
        ],
        None,
    );
    assert_eq!(job["queue"], "in", "{job}");
    assert!(fixture.sink.deliveries().is_empty());
    let mut child = spawn_server(fixture.dir.path(), &fixture.env);
    wait_ready(fixture.web_port, &mut child).await;
    fixture.child = Some(child);
    fixture
        .deliveries_when(Duration::from_secs(15), |all| {
            all.iter()
                .any(|d| d.rcpt_to.contains(&"reader@e2e.example.invalid".to_owned()))
        })
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let to_reader = fixture
        .sink
        .deliveries()
        .into_iter()
        .filter(|d| d.rcpt_to.contains(&"reader@e2e.example.invalid".to_owned()))
        .count();
    assert_eq!(to_reader, 1, "exactly once");
}

/// The CSRF token of a rendered browser form.
fn csrf_of(html: &str) -> String {
    html.split("name=\"csrf\" value=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("a csrf field")
        .to_owned()
}

/// The browser session behind a real web login: the cookie the server set.
async fn web_login(fixture: &Fixture, email: &str, password: &str) -> String {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let base = format!("http://127.0.0.1:{}", fixture.web_port);
    let form = client
        .get(format!("{base}/web/login"))
        .send()
        .await
        .unwrap();
    let anonymous = form.headers()["set-cookie"].to_str().unwrap().to_owned();
    let csrf = csrf_of(&form.text().await.unwrap());
    let login = client
        .post(format!("{base}/web/login"))
        .header("origin", "https://lists.e2e.invalid")
        .header("cookie", &anonymous)
        .form(&[
            ("csrf", csrf.as_str()),
            ("email", email),
            ("password", password),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), 303, "login must redirect");
    login.headers()["set-cookie"].to_str().unwrap().to_owned()
}

/// The delivered copy of a web post, as the other member received it.
fn assert_web_post_delivery(data: &[u8]) {
    assert_eq!(
        header(data, "From").as_deref(),
        Some("\"Poster\" <poster@e2e.example.invalid>")
    );
    assert_eq!(
        header(data, "Subject").as_deref(),
        Some("[dev] Posted from the web")
    );
    assert_eq!(header(data, "User-Agent").as_deref(), Some("listmngr-web"));
    assert!(header(data, "Message-ID-Hash").is_some());
    assert_eq!(
        header(data, "List-Post").as_deref(),
        Some("<mailto:dev@e2e.example.invalid>")
    );
    assert!(
        String::from_utf8_lossy(data).contains("Written in the browser, delivered by the list."),
        "{}",
        String::from_utf8_lossy(data)
    );
}

/// The archive page once the archive runner has stored the post.
async fn wait_archived(client: &reqwest::Client, url: &str, cookie: &str, needle: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let page = client
            .get(url)
            .header("cookie", cookie)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        if page.contains(needle) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the web post never reached the archive: {page}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// P5-WEB-POST: a member signed in on the web writes a new thread; the
/// real server composes the message from the verified address, the `in`
/// runner admits it, the out runner delivers it to the other member over
/// the real SMTP socket, and the archive runner stores it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn web_post_is_delivered_and_archived() {
    let fixture = Fixture::start().await;
    // The account first, so the address is the account's; then the
    // membership through that address; then its verification.
    run_cli(
        fixture.dir.path(),
        &fixture.env,
        &[
            "user",
            "create",
            "poster@e2e.example.invalid",
            "--display-name",
            "Poster",
            "--password-stdin",
        ],
        Some("E2ePosterPassw0rd!\n"),
    );
    fixture.add_member("poster@e2e.example.invalid");
    fixture.add_member("reader@e2e.example.invalid");
    let db = listmngr_db::Database::connect(&fixture.db_url(), 2)
        .await
        .unwrap();
    db.addresses()
        .verify("poster@e2e.example.invalid", true)
        .await
        .unwrap();
    let cookie = web_login(&fixture, "poster@e2e.example.invalid", "E2ePosterPassw0rd!").await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let base = format!("http://127.0.0.1:{}", fixture.web_port);
    let form_url = format!("{base}/web/lists/{}/archive/post", fixture.list);
    let form = client
        .get(&form_url)
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(form.status(), 200);
    let html = form.text().await.unwrap();
    assert!(html.contains("poster@e2e.example.invalid"), "{html}");
    let csrf = csrf_of(&html);
    let posted = client
        .post(&form_url)
        .header("origin", "https://lists.e2e.invalid")
        .header("cookie", &cookie)
        .form(&[
            ("csrf", csrf.as_str()),
            ("reply", ""),
            ("subject", "Posted from the web"),
            ("body", "Written in the browser, delivered by the list."),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(posted.status(), 303, "{}", posted.text().await.unwrap());
    assert!(
        posted.headers()["location"]
            .to_str()
            .unwrap()
            .ends_with("/archive?saved=posted")
    );
    fixture
        .deliveries_when(Duration::from_secs(15), |all| {
            all.iter()
                .any(|d| d.rcpt_to.contains(&"reader@e2e.example.invalid".to_owned()))
        })
        .await;
    let delivery = fixture
        .sink
        .deliveries()
        .into_iter()
        .find(|d| d.rcpt_to.contains(&"reader@e2e.example.invalid".to_owned()))
        .unwrap();
    assert_web_post_delivery(&delivery.data);
    wait_archived(
        &client,
        &format!("{base}/web/lists/{}/archive", fixture.list),
        &cookie,
        "Posted from the web",
    )
    .await;
}

/// The list's roster of one role through the compatibility API.
async fn roster(fixture: &Fixture, role: &str) -> Vec<String> {
    let response = reqwest::Client::new()
        .get(format!(
            "http://127.0.0.1:{}/3.1/lists/{}/roster/{role}",
            fixture.web_port, fixture.list
        ))
        .bearer_auth(&fixture.admin_token)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{}", response.status());
    let body: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    body["entries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| entry["email"].as_str().map(str::to_owned))
        .collect()
}

/// A mail command as a subscriber's mail client would send it.
fn command(from: &str, local: &str, subject: &str) -> Vec<u8> {
    format!(
        "From: {from}\r\nTo: {local}@e2e.example.invalid\r\nMessage-ID: <{}@example.invalid>\r\nSubject: {subject}\r\n\r\n",
        uuid::Uuid::now_v7()
    )
    .into_bytes()
}

/// Deliver one mail command over the real LMTP socket and require the
/// durable acceptance.
async fn send_command(fixture: &Fixture, from: &str, local: &str, subject: &str) {
    let result = lmtp_deliver(
        fixture.lmtp_port,
        Some(from),
        &[&format!("{local}@e2e.example.invalid")],
        &command(from, local, subject),
    )
    .await;
    assert!(
        result.rcpt_replies[0].starts_with("250"),
        "{local}: {:?}",
        result.rcpt_replies
    );
    assert!(
        result.data_replies[0].starts_with("250"),
        "{local}: {:?}",
        result.data_replies
    );
}

/// The next mail to `to` whose subject starts with `subject`, counted
/// from `seen` deliveries; the sink is real, so it is waited for.
async fn next_mail_to(fixture: &Fixture, seen: usize, to: &str, subject: &str) -> Delivery {
    let matches = |delivery: &Delivery| {
        delivery.rcpt_to == [to.to_owned()]
            && header(&delivery.data, "Subject").is_some_and(|s| s.starts_with(subject))
    };
    let found = wait_until(Duration::from_secs(15), || {
        fixture.sink.deliveries().iter().skip(seen).any(matches)
    })
    .await;
    let deliveries = fixture.sink.deliveries();
    assert!(
        found,
        "no mail to {to} with subject {subject:?} after {seen}: {:?}",
        deliveries.iter().map(mail_summary).collect::<Vec<_>>()
    );
    deliveries.into_iter().skip(seen).find(matches).unwrap()
}

/// (envelope sender, recipients, subject) of one delivery, for a failure.
fn mail_summary(delivery: &Delivery) -> (Option<String>, Vec<String>, Option<String>) {
    (
        delivery.mail_from.clone(),
        delivery.rcpt_to.clone(),
        header(&delivery.data, "Subject"),
    )
}

/// The confirmation challenge: `Subject: confirm TOKEN`, `Reply-To` the
/// list's `-confirm` address, the token in the body, and — as every
/// generated notice here — a null reverse path and `Auto-Submitted`;
/// returns the token.
fn challenge_token(challenge: &Delivery, to: &str) -> String {
    let subject = header(&challenge.data, "Subject").unwrap();
    let token = subject
        .strip_prefix("confirm ")
        .unwrap_or_else(|| panic!("not a challenge: {subject}"))
        .to_owned();
    assert_eq!(token.len(), 43, "one base64url token: {subject}");
    assert_eq!(
        header(&challenge.data, "Reply-To").as_deref(),
        Some("dev-confirm@e2e.example.invalid")
    );
    assert_eq!(header(&challenge.data, "To").as_deref(), Some(to));
    assert_eq!(
        challenge.mail_from.as_deref(),
        Some(""),
        "a notice travels with the null reverse path"
    );
    assert_eq!(
        header(&challenge.data, "Auto-Submitted").as_deref(),
        Some("auto-generated")
    );
    let body = String::from_utf8_lossy(&challenge.data);
    assert!(
        body.contains(&format!("Token: {token}")),
        "the body names the token: {body}"
    );
    token
}

/// Phase 3 acceptance: the subscription flow over mail, end to end. A
/// stranger writes to `dev-join@`; the server mails a challenge whose
/// subject is `confirm TOKEN` with `Reply-To: dev-confirm@`; the reply,
/// subject intact, makes them a member and the welcome notice reaches
/// them. Then `dev-leave@`: a fresh challenge, the reply removes them and
/// the goodbye notice reaches them. A replay of a spent token changes
/// nothing. Every mail crosses the real LMTP socket in and the real SMTP
/// sink out.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn join_and_leave_by_mail_round_trip_their_confirmation_tokens() {
    let fixture = Fixture::start().await;
    fixture
        .patch_config(serde_json::json!({
            "send_welcome_message": true,
            "send_goodbye_message": true
        }))
        .await;
    let newbie = "newbie@e2e.example.invalid";

    // -join: the challenge, and no membership yet.
    send_command(&fixture, newbie, "dev-join", "subscribe").await;
    let challenge = next_mail_to(&fixture, 0, newbie, "confirm ").await;
    let join_token = challenge_token(&challenge, newbie);
    assert!(
        roster(&fixture, "member").await.is_empty(),
        "no member before the confirmation"
    );

    // The reply to the challenge: membership, then the welcome.
    let seen = fixture.sink.deliveries().len();
    send_command(
        &fixture,
        newbie,
        "dev-confirm",
        &format!("Re: confirm {join_token}"),
    )
    .await;
    let welcome = next_mail_to(
        &fixture,
        seen,
        newbie,
        "Welcome to the \"Dev\" mailing list",
    )
    .await;
    assert_eq!(roster(&fixture, "member").await, vec![newbie.to_owned()]);
    assert!(
        String::from_utf8_lossy(&welcome.data).contains("dev@e2e.example.invalid"),
        "the welcome names the list: {:?}",
        String::from_utf8_lossy(&welcome.data)
    );

    // The new member's post is delivered to them like any member's.
    let seen = fixture.sink.deliveries().len();
    let result = lmtp_deliver(
        fixture.lmtp_port,
        Some(newbie),
        &["dev@e2e.example.invalid"],
        &post(newbie, "first post", "hello from a confirmed member"),
    )
    .await;
    assert!(result.data_replies[0].starts_with("250"), "{result:?}");
    let copy = next_mail_to(&fixture, seen, newbie, "[dev] first post").await;
    assert!(String::from_utf8_lossy(&copy.data).contains("hello from a confirmed member"));

    // -leave: a fresh challenge; the spent join token is not it.
    let seen = fixture.sink.deliveries().len();
    send_command(&fixture, newbie, "dev-leave", "unsubscribe").await;
    let challenge = next_mail_to(&fixture, seen, newbie, "confirm ").await;
    let leave_token = challenge_token(&challenge, newbie);
    assert_ne!(leave_token, join_token, "a new token for a new request");
    assert_eq!(roster(&fixture, "member").await, vec![newbie.to_owned()]);

    // The reply: the membership ends, then the goodbye.
    let seen = fixture.sink.deliveries().len();
    send_command(
        &fixture,
        newbie,
        "dev-confirm",
        &format!("Re: confirm {leave_token}"),
    )
    .await;
    next_mail_to(
        &fixture,
        seen,
        newbie,
        "You have been unsubscribed from the Dev mailing list",
    )
    .await;
    assert!(roster(&fixture, "member").await.is_empty());

    // A replay of the spent join token: accepted at the socket (it is a
    // durable command), but no membership and no further mail.
    let seen = fixture.sink.deliveries().len();
    send_command(
        &fixture,
        newbie,
        "dev-confirm",
        &format!("Re: confirm {join_token}"),
    )
    .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(roster(&fixture, "member").await.is_empty());
    assert_eq!(
        fixture.sink.deliveries().len(),
        seen,
        "no mail answers a spent token: {:?}",
        fixture.sink.deliveries()[seen..]
            .iter()
            .map(mail_summary)
            .collect::<Vec<_>>()
    );
}
