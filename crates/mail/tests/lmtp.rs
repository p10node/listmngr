use listmngr_mail::lmtp::{
    LmtpHandler, Protocol, RecipientOutcome, serve_session, serve_session_as,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

type Delivery = (Option<String>, Vec<String>, Vec<u8>);

#[derive(Debug, Clone)]
struct FakeHandler {
    max_message_bytes: usize,
    max_recipients: usize,
    known: Vec<String>,
    delivered: Arc<Mutex<Vec<Delivery>>>,
    /// When set, `deliver` returns exactly this many outcomes regardless of
    /// `recipients.len()`, simulating a misbehaving handler (T8 cardinality).
    result_count_override: Option<usize>,
    command_timeout: Duration,
    /// The outcome every recipient gets from `deliver`.
    outcome: (u16, &'static str),
}
impl FakeHandler {
    fn new() -> Self {
        Self {
            max_message_bytes: 1024,
            max_recipients: 3,
            known: vec!["list@example.invalid".into()],
            delivered: Arc::new(Mutex::new(Vec::new())),
            result_count_override: None,
            command_timeout: Duration::from_secs(5),
            outcome: (250, "2.1.5 delivered"),
        }
    }
    fn deliveries(&self) -> Vec<Delivery> {
        self.delivered.lock().unwrap().clone()
    }
}
impl LmtpHandler for FakeHandler {
    fn local_hostname(&self) -> &'static str {
        "mx.example.invalid"
    }
    fn max_message_bytes(&self) -> usize {
        self.max_message_bytes
    }
    fn max_recipients(&self) -> usize {
        self.max_recipients
    }
    fn command_timeout(&self) -> Duration {
        self.command_timeout
    }
    async fn accept_recipient(&mut self, address: &str) -> Result<(), String> {
        if self.known.iter().any(|k| k == address) {
            Ok(())
        } else {
            Err(format!("unknown recipient {address}"))
        }
    }
    async fn deliver(
        &mut self,
        mail_from: Option<&str>,
        recipients: &[String],
        data: &[u8],
    ) -> Vec<RecipientOutcome> {
        self.delivered.lock().unwrap().push((
            mail_from.map(str::to_owned),
            recipients.to_vec(),
            data.to_vec(),
        ));
        let count = self.result_count_override.unwrap_or(recipients.len());
        (0..count)
            .map(|_| RecipientOutcome {
                code: self.outcome.0,
                detail: self.outcome.1.into(),
            })
            .collect()
    }
}

fn session_pair() -> (tokio::io::DuplexStream, tokio::task::JoinHandle<()>) {
    session_pair_with(FakeHandler::new())
}

fn session_pair_with(
    mut handler: FakeHandler,
) -> (tokio::io::DuplexStream, tokio::task::JoinHandle<()>) {
    let (client, server) = tokio::io::duplex(8192);
    let task = tokio::spawn(async move {
        serve_session(server, &mut handler).await.unwrap();
    });
    (client, task)
}

fn session_pair_inspectable(
    handler: FakeHandler,
) -> (
    tokio::io::DuplexStream,
    FakeHandler,
    tokio::task::JoinHandle<()>,
) {
    let inspect = handler.clone();
    let (client, task) = session_pair_with(handler);
    (client, inspect, task)
}

async fn read_reply(client: &mut tokio::io::DuplexStream) -> String {
    let mut buf = vec![0u8; 4096];
    let n = tokio::time::timeout(Duration::from_secs(2), client.read(&mut buf))
        .await
        .expect("read timeout")
        .unwrap();
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

async fn send_line(client: &mut tokio::io::DuplexStream, line: &str) {
    client.write_all(line.as_bytes()).await.unwrap();
    client.write_all(b"\r\n").await.unwrap();
}

#[tokio::test]
async fn happy_path_delivers_to_a_known_recipient_and_resets_after_data() {
    let (mut client, task) = session_pair();
    assert!(read_reply(&mut client).await.starts_with("220 "));
    send_line(&mut client, "LHLO client.example.invalid").await;
    let ehlo = read_reply(&mut client).await;
    assert!(ehlo.starts_with("250-mx.example.invalid"));
    assert!(ehlo.contains("SIZE 1024"));
    assert!(ehlo.contains("8BITMIME"));
    send_line(&mut client, "MAIL FROM:<alice@example.invalid>").await;
    assert!(read_reply(&mut client).await.starts_with("250 "));
    send_line(&mut client, "RCPT TO:<list@example.invalid>").await;
    assert!(read_reply(&mut client).await.starts_with("250 "));
    send_line(&mut client, "DATA").await;
    assert!(read_reply(&mut client).await.starts_with("354 "));
    client
        .write_all(b"Subject: hi\r\n\r\nbody\r\n.\r\n")
        .await
        .unwrap();
    let reply = read_reply(&mut client).await;
    assert!(reply.starts_with("250 2.1.5 delivered"));
    // Transaction resets: DATA without a new MAIL/RCPT must be rejected.
    send_line(&mut client, "DATA").await;
    assert!(read_reply(&mut client).await.starts_with("503 "));
    send_line(&mut client, "QUIT").await;
    assert!(read_reply(&mut client).await.starts_with("221 "));
    task.await.unwrap();
}

#[tokio::test]
async fn unknown_and_unsupported_recipients_are_rejected_before_data() {
    let (mut client, task) = session_pair();
    read_reply(&mut client).await;
    send_line(&mut client, "LHLO client.example.invalid").await;
    read_reply(&mut client).await;
    send_line(&mut client, "MAIL FROM:<alice@example.invalid>").await;
    read_reply(&mut client).await;
    send_line(&mut client, "RCPT TO:<nobody@example.invalid>").await;
    assert!(read_reply(&mut client).await.starts_with("550 "));
    // No recipient was accepted, so DATA must be refused, never routed anywhere.
    send_line(&mut client, "DATA").await;
    assert!(read_reply(&mut client).await.starts_with("503 "));
    send_line(&mut client, "QUIT").await;
    read_reply(&mut client).await;
    task.await.unwrap();
}

#[tokio::test]
async fn null_reverse_path_is_accepted_as_a_distinct_sender() {
    let (mut client, task) = session_pair();
    read_reply(&mut client).await;
    send_line(&mut client, "LHLO client.example.invalid").await;
    read_reply(&mut client).await;
    send_line(&mut client, "MAIL FROM:<>").await;
    assert!(read_reply(&mut client).await.starts_with("250 "));
    send_line(&mut client, "RCPT TO:<list@example.invalid>").await;
    read_reply(&mut client).await;
    send_line(&mut client, "DATA").await;
    read_reply(&mut client).await;
    client
        .write_all(b"Subject: x\r\n\r\nbody\r\n.\r\n")
        .await
        .unwrap();
    read_reply(&mut client).await;
    send_line(&mut client, "QUIT").await;
    read_reply(&mut client).await;
    task.await.unwrap();
}

#[tokio::test]
async fn oversized_message_fails_closed_without_calling_deliver() {
    let mut handler = FakeHandler::new();
    handler.max_message_bytes = 8;
    let (mut client, task) = session_pair_with(handler);
    read_reply(&mut client).await;
    send_line(&mut client, "LHLO client.example.invalid").await;
    read_reply(&mut client).await;
    send_line(&mut client, "MAIL FROM:<alice@example.invalid>").await;
    read_reply(&mut client).await;
    send_line(&mut client, "RCPT TO:<list@example.invalid>").await;
    read_reply(&mut client).await;
    send_line(&mut client, "DATA").await;
    read_reply(&mut client).await;
    client
        .write_all(b"this body is far larger than the configured cap\r\n.\r\n")
        .await
        .unwrap();
    assert!(read_reply(&mut client).await.starts_with("552 "));
    send_line(&mut client, "QUIT").await;
    read_reply(&mut client).await;
    task.await.unwrap();
}

#[tokio::test]
async fn recipient_limit_is_enforced_before_accept_recipient_runs() {
    let mut handler = FakeHandler::new();
    handler.max_recipients = 1;
    handler.known = vec!["a@example.invalid".into(), "b@example.invalid".into()];
    let (mut client, task) = session_pair_with(handler);
    read_reply(&mut client).await;
    send_line(&mut client, "LHLO client.example.invalid").await;
    read_reply(&mut client).await;
    send_line(&mut client, "MAIL FROM:<alice@example.invalid>").await;
    read_reply(&mut client).await;
    send_line(&mut client, "RCPT TO:<a@example.invalid>").await;
    assert!(read_reply(&mut client).await.starts_with("250 "));
    send_line(&mut client, "RCPT TO:<b@example.invalid>").await;
    assert!(read_reply(&mut client).await.starts_with("452 "));
    send_line(&mut client, "QUIT").await;
    read_reply(&mut client).await;
    task.await.unwrap();
}

#[tokio::test]
async fn dot_unstuffing_and_binary_bytes_are_preserved_exactly() {
    let (mut client, handler, task) = session_pair_inspectable(FakeHandler::new());
    read_reply(&mut client).await;
    send_line(&mut client, "LHLO client.example.invalid").await;
    read_reply(&mut client).await;
    send_line(&mut client, "MAIL FROM:<alice@example.invalid>").await;
    read_reply(&mut client).await;
    send_line(&mut client, "RCPT TO:<list@example.invalid>").await;
    read_reply(&mut client).await;
    send_line(&mut client, "DATA").await;
    read_reply(&mut client).await;
    // A leading-dot line is stuffed as "..line" on the wire and must unstuff to ".line".
    client
        .write_all(b"Subject: x\r\n\r\n..stuffed line\r\nbinary:\x00\xff\r\n.\r\n")
        .await
        .unwrap();
    assert!(read_reply(&mut client).await.starts_with("250 "));
    send_line(&mut client, "QUIT").await;
    read_reply(&mut client).await;
    task.await.unwrap();
    // The handler must have received the exact unstuffed, binary-safe bytes:
    // the wire's leading ".." collapses to a single "." and \x00\xff pass through untouched.
    let deliveries = handler.deliveries();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(
        deliveries[0].2,
        b"Subject: x\r\n\r\n.stuffed line\r\nbinary:\x00\xff\r\n".to_vec()
    );
    assert_eq!(deliveries[0].0.as_deref(), Some("alice@example.invalid"));
    assert_eq!(deliveries[0].1, vec!["list@example.invalid".to_owned()]);
}

#[tokio::test]
async fn data_phase_stall_after_start_times_out_and_closes_instead_of_hanging() {
    let mut handler = FakeHandler::new();
    handler.command_timeout = Duration::from_millis(150);
    let (mut client, task) = session_pair_with(handler);
    read_reply(&mut client).await;
    send_line(&mut client, "LHLO client.example.invalid").await;
    read_reply(&mut client).await;
    send_line(&mut client, "MAIL FROM:<alice@example.invalid>").await;
    read_reply(&mut client).await;
    send_line(&mut client, "RCPT TO:<list@example.invalid>").await;
    read_reply(&mut client).await;
    send_line(&mut client, "DATA").await;
    assert!(read_reply(&mut client).await.starts_with("354 "));
    // Send a partial header line, then go silent: never complete the message.
    client.write_all(b"Subject: partial").await.unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(2), read_reply(&mut client))
        .await
        .expect("a stalled DATA phase must not hang the session forever");
    assert!(reply.starts_with("421 "), "got {reply:?}");
    // The server must close the session, not keep waiting indefinitely.
    let mut trailing = [0u8; 8];
    let closed = tokio::time::timeout(Duration::from_secs(1), client.read(&mut trailing))
        .await
        .expect("session must close after a DATA timeout");
    assert_eq!(closed.unwrap(), 0, "connection must be closed (EOF)");
    task.await.unwrap();
}

#[tokio::test]
async fn oversized_drain_also_respects_the_data_deadline() {
    let mut handler = FakeHandler::new();
    handler.max_message_bytes = 8;
    handler.command_timeout = Duration::from_millis(150);
    let (mut client, task) = session_pair_with(handler);
    read_reply(&mut client).await;
    send_line(&mut client, "LHLO client.example.invalid").await;
    read_reply(&mut client).await;
    send_line(&mut client, "MAIL FROM:<alice@example.invalid>").await;
    read_reply(&mut client).await;
    send_line(&mut client, "RCPT TO:<list@example.invalid>").await;
    read_reply(&mut client).await;
    send_line(&mut client, "DATA").await;
    read_reply(&mut client).await;
    // A complete, CRLF-terminated line that alone exceeds the tiny configured
    // cap: this must actually flip the reader into its oversized/discard
    // state (a partial, unterminated line never would, since `read_data` only
    // measures whole lines). Only after that does it go silent without ever
    // sending the terminating dot, so the discard/drain phase itself is what
    // must still respect the overall deadline.
    client
        .write_all(b"this line alone exceeds the cap\r\n")
        .await
        .unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(2), read_reply(&mut client))
        .await
        .expect("an unterminated oversized DATA must not hang the session forever");
    assert!(reply.starts_with("421 "), "got {reply:?}");
    task.await.unwrap();
}

fn spawn_session<H: LmtpHandler + 'static>(
    mut handler: H,
) -> (
    tokio::io::DuplexStream,
    tokio::task::JoinHandle<std::io::Result<()>>,
) {
    let (client, server) = tokio::io::duplex(8192);
    let task = tokio::spawn(async move { serve_session(server, &mut handler).await });
    (client, task)
}

#[derive(Clone)]
struct HangingHandler {
    command_timeout: Duration,
    hang_accept: bool,
    hang_deliver: bool,
}
impl HangingHandler {
    const fn accept_hangs() -> Self {
        Self {
            command_timeout: Duration::from_millis(150),
            hang_accept: true,
            hang_deliver: false,
        }
    }
    const fn deliver_hangs() -> Self {
        Self {
            command_timeout: Duration::from_millis(150),
            hang_accept: false,
            hang_deliver: true,
        }
    }
}
impl LmtpHandler for HangingHandler {
    fn local_hostname(&self) -> &'static str {
        "mx.example.invalid"
    }
    fn max_message_bytes(&self) -> usize {
        1024
    }
    fn max_recipients(&self) -> usize {
        3
    }
    fn command_timeout(&self) -> Duration {
        self.command_timeout
    }
    async fn accept_recipient(&mut self, _address: &str) -> Result<(), String> {
        if self.hang_accept {
            std::future::pending::<()>().await;
        }
        Ok(())
    }
    async fn deliver(
        &mut self,
        _mail_from: Option<&str>,
        _recipients: &[String],
        _data: &[u8],
    ) -> Vec<RecipientOutcome> {
        if self.hang_deliver {
            std::future::pending::<()>().await;
        }
        Vec::new()
    }
}

#[tokio::test]
async fn a_reply_write_the_peer_never_reads_does_not_hang_the_session_forever() {
    let mut handler = FakeHandler::new();
    handler.command_timeout = Duration::from_millis(150);
    let (client, server) = tokio::io::duplex(8);
    let task: tokio::task::JoinHandle<std::io::Result<()>> =
        tokio::spawn(async move { serve_session(server, &mut handler).await });
    // Never read: the server's very first write (the 220 greeting) cannot
    // fully land in an 8-byte buffer and must eventually give up rather than
    // hang forever.
    let outcome = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("a session whose peer never reads must not hang forever")
        .unwrap();
    assert!(
        outcome.is_err(),
        "a stalled write must surface as a transport error, not silently succeed"
    );
    drop(client);
}

#[tokio::test]
async fn an_accept_recipient_hook_that_never_resolves_is_bounded_by_the_command_timeout() {
    let (mut client, task) = spawn_session(HangingHandler::accept_hangs());
    read_reply(&mut client).await;
    send_line(&mut client, "LHLO client.example.invalid").await;
    read_reply(&mut client).await;
    send_line(&mut client, "MAIL FROM:<alice@example.invalid>").await;
    read_reply(&mut client).await;
    send_line(&mut client, "RCPT TO:<list@example.invalid>").await;
    let reply = tokio::time::timeout(Duration::from_secs(2), read_reply(&mut client))
        .await
        .expect("a recipient-validation hook that never resolves must not hang the session");
    assert!(
        reply.starts_with("451 "),
        "an unresolved validation must fail closed (never accept), got {reply:?}"
    );
    // The recipient must not have been silently accepted: DATA has nothing to send to.
    send_line(&mut client, "DATA").await;
    assert!(read_reply(&mut client).await.starts_with("503 "));
    send_line(&mut client, "QUIT").await;
    read_reply(&mut client).await;
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_deliver_hook_that_never_resolves_is_bounded_by_the_command_timeout() {
    let (mut client, task) = spawn_session(HangingHandler::deliver_hangs());
    read_reply(&mut client).await;
    send_line(&mut client, "LHLO client.example.invalid").await;
    read_reply(&mut client).await;
    send_line(&mut client, "MAIL FROM:<alice@example.invalid>").await;
    read_reply(&mut client).await;
    send_line(&mut client, "RCPT TO:<list@example.invalid>").await;
    read_reply(&mut client).await;
    send_line(&mut client, "DATA").await;
    read_reply(&mut client).await;
    client
        .write_all(b"Subject: x\r\n\r\nbody\r\n.\r\n")
        .await
        .unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(2), read_reply(&mut client))
        .await
        .expect("a durable-intake hook that never resolves must not hang the session forever");
    assert!(
        reply.starts_with("451 "),
        "an unresolved delivery outcome must never be reported as 250 (a false success), got {reply:?}"
    );
    send_line(&mut client, "QUIT").await;
    read_reply(&mut client).await;
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn ehlo_is_rejected_only_lhlo_is_accepted() {
    let (mut client, task) = session_pair();
    read_reply(&mut client).await;
    send_line(&mut client, "EHLO client.example.invalid").await;
    let reply = read_reply(&mut client).await;
    assert!(
        !reply.starts_with("250"),
        "LMTP must not accept EHLO, got {reply:?}"
    );
    // The session must still be pre-greeting: MAIL is refused.
    send_line(&mut client, "MAIL FROM:<alice@example.invalid>").await;
    assert!(read_reply(&mut client).await.starts_with("503 "));
    send_line(&mut client, "LHLO client.example.invalid").await;
    assert!(read_reply(&mut client).await.starts_with("250"));
    send_line(&mut client, "QUIT").await;
    read_reply(&mut client).await;
    task.await.unwrap();
}

#[tokio::test]
async fn enhanced_status_codes_are_advertised_and_present_on_replies() {
    let (mut client, task) = session_pair();
    read_reply(&mut client).await;
    send_line(&mut client, "LHLO client.example.invalid").await;
    let greeting = read_reply(&mut client).await;
    assert!(greeting.contains("ENHANCEDSTATUSCODES"), "got {greeting:?}");
    send_line(&mut client, "MAIL FROM:<alice@example.invalid>").await;
    let mail_reply = read_reply(&mut client).await;
    assert!(
        mail_reply.starts_with("250 2."),
        "MAIL reply must carry a class-2 enhanced status code, got {mail_reply:?}"
    );
    send_line(&mut client, "RCPT TO:<nobody@example.invalid>").await;
    let rcpt_reply = read_reply(&mut client).await;
    assert!(
        rcpt_reply.starts_with("550 5."),
        "rejected RCPT must carry a class-5 enhanced status code, got {rcpt_reply:?}"
    );
    send_line(&mut client, "QUIT").await;
    let quit_reply = read_reply(&mut client).await;
    assert!(quit_reply.starts_with("221 2."), "got {quit_reply:?}");
    task.await.unwrap();
}

#[tokio::test]
async fn excess_hook_results_never_produce_more_replies_than_recipients() {
    let mut handler = FakeHandler::new();
    handler.known = vec!["list@example.invalid".into()];
    handler.result_count_override = Some(3);
    let (mut client, task) = session_pair_with(handler);
    read_reply(&mut client).await;
    send_line(&mut client, "LHLO client.example.invalid").await;
    read_reply(&mut client).await;
    send_line(&mut client, "MAIL FROM:<alice@example.invalid>").await;
    read_reply(&mut client).await;
    send_line(&mut client, "RCPT TO:<list@example.invalid>").await;
    read_reply(&mut client).await;
    send_line(&mut client, "DATA").await;
    read_reply(&mut client).await;
    client
        .write_all(b"Subject: x\r\n\r\nbody\r\n.\r\n")
        .await
        .unwrap();
    // Exactly one final reply must arrive for the one accepted RCPT, even
    // though the (misbehaving) handler returned three outcomes.
    let reply = read_reply(&mut client).await;
    assert_eq!(reply.matches("2.1.5 delivered").count(), 1, "got {reply:?}");
    // The next command's reply must not be desynced by a phantom extra reply.
    send_line(&mut client, "NOOP").await;
    let noop_reply = read_reply(&mut client).await;
    assert!(
        noop_reply.starts_with("250"),
        "pipelining desync: got {noop_reply:?}"
    );
    send_line(&mut client, "QUIT").await;
    read_reply(&mut client).await;
    task.await.unwrap();
}

#[tokio::test]
async fn duplicate_rcpt_still_gets_one_reply_per_command() {
    let (mut client, task) = session_pair();
    read_reply(&mut client).await;
    send_line(&mut client, "LHLO client.example.invalid").await;
    read_reply(&mut client).await;
    send_line(&mut client, "MAIL FROM:<alice@example.invalid>").await;
    read_reply(&mut client).await;
    send_line(&mut client, "RCPT TO:<list@example.invalid>").await;
    read_reply(&mut client).await;
    send_line(&mut client, "RCPT TO:<list@example.invalid>").await;
    read_reply(&mut client).await;
    send_line(&mut client, "DATA").await;
    read_reply(&mut client).await;
    client
        .write_all(b"Subject: x\r\n\r\nbody\r\n.\r\n")
        .await
        .unwrap();
    let reply = read_reply(&mut client).await;
    assert_eq!(
        reply.matches("2.1.5 delivered").count(),
        2,
        "duplicate RCPT must still get its own final reply, got {reply:?}"
    );
    send_line(&mut client, "QUIT").await;
    read_reply(&mut client).await;
    task.await.unwrap();
}

#[tokio::test]
async fn rset_clears_transaction_state() {
    let (mut client, task) = session_pair();
    read_reply(&mut client).await;
    send_line(&mut client, "LHLO client.example.invalid").await;
    read_reply(&mut client).await;
    send_line(&mut client, "MAIL FROM:<alice@example.invalid>").await;
    read_reply(&mut client).await;
    send_line(&mut client, "RCPT TO:<list@example.invalid>").await;
    read_reply(&mut client).await;
    send_line(&mut client, "RSET").await;
    assert!(read_reply(&mut client).await.starts_with("250 "));
    send_line(&mut client, "DATA").await;
    assert!(read_reply(&mut client).await.starts_with("503 "));
    send_line(&mut client, "QUIT").await;
    read_reply(&mut client).await;
    task.await.unwrap();
}

#[tokio::test]
async fn commands_before_lhlo_are_rejected() {
    let (mut client, task) = session_pair();
    read_reply(&mut client).await;
    send_line(&mut client, "MAIL FROM:<alice@example.invalid>").await;
    assert!(read_reply(&mut client).await.starts_with("503 "));
    send_line(&mut client, "NOOP").await;
    assert!(read_reply(&mut client).await.starts_with("250 "));
    send_line(&mut client, "QUIT").await;
    read_reply(&mut client).await;
    task.await.unwrap();
}

#[tokio::test]
async fn overlong_command_line_is_rejected_and_closes_the_session() {
    let (mut client, task) = session_pair();
    read_reply(&mut client).await;
    let overlong = format!("LHLO {}\r\n", "x".repeat(20_000));
    // The server may hang up mid-drain rather than absorb an unbounded line,
    // so the write itself is allowed to fail; only the safe-rejection matters.
    let writer = tokio::spawn(async move {
        let _ = client.write_all(overlong.as_bytes()).await;
        client
    });
    task.await.unwrap();
    let _ = writer.await;
}

/// Greet and open a session, returning the client end.
async fn greeted(handler: FakeHandler) -> (tokio::io::DuplexStream, tokio::task::JoinHandle<()>) {
    let (mut client, task) = session_pair_with(handler);
    read_reply(&mut client).await;
    send_line(&mut client, "LHLO client.example.invalid").await;
    read_reply(&mut client).await;
    (client, task)
}

#[tokio::test]
async fn a_declared_size_over_the_limit_is_refused_at_mail_from() {
    let mut handler = FakeHandler::new();
    handler.max_message_bytes = 1000;
    let (mut client, inspect, task) = session_pair_inspectable(handler);
    read_reply(&mut client).await;
    send_line(&mut client, "LHLO client.example.invalid").await;
    read_reply(&mut client).await;
    send_line(&mut client, "MAIL FROM:<alice@example.invalid> SIZE=1001").await;
    let reply = read_reply(&mut client).await;
    assert!(reply.starts_with("552 5.3.4"), "{reply}");
    // No transaction was opened, so RCPT is out of sequence.
    send_line(&mut client, "RCPT TO:<list@example.invalid>").await;
    assert!(read_reply(&mut client).await.starts_with("503 "));

    // The exact limit is acceptable, and the session continues normally.
    send_line(&mut client, "MAIL FROM:<alice@example.invalid> SIZE=1000").await;
    assert!(read_reply(&mut client).await.starts_with("250 "));
    send_line(&mut client, "RCPT TO:<list@example.invalid>").await;
    assert!(read_reply(&mut client).await.starts_with("250 "));
    send_line(&mut client, "DATA").await;
    read_reply(&mut client).await;
    client
        .write_all(b"Subject: sized\r\n\r\nbody\r\n.\r\n")
        .await
        .unwrap();
    assert!(read_reply(&mut client).await.starts_with("250 "));
    send_line(&mut client, "QUIT").await;
    read_reply(&mut client).await;
    task.await.unwrap();
    assert_eq!(
        inspect.deliveries().len(),
        1,
        "only the sized post is stored"
    );
}

#[tokio::test]
async fn body_and_size_parameters_are_validated_before_the_transaction_opens() {
    let (mut client, task) = greeted(FakeHandler::new()).await;
    for (command, code) in [
        // RFC 6152: both body types this server announces are accepted,
        // case-insensitively, in any order with SIZE.
        ("MAIL FROM:<a@example.invalid> BODY=8BITMIME", "250 "),
        ("MAIL FROM:<a@example.invalid> body=7bit", "250 "),
        (
            "MAIL FROM:<a@example.invalid> SIZE=10 BODY=8BITMIME",
            "250 ",
        ),
        // BINARYMIME needs CHUNKING, which this server does not offer.
        ("MAIL FROM:<a@example.invalid> BODY=BINARYMIME", "555 5.5.4"),
        ("MAIL FROM:<a@example.invalid> BODY=NONSENSE", "555 5.5.4"),
        // Nothing else is announced, so nothing else may be used.
        ("MAIL FROM:<a@example.invalid> RET=FULL", "555 5.5.4"),
        ("MAIL FROM:<a@example.invalid> AUTH=<>", "555 5.5.4"),
        ("MAIL FROM:<a@example.invalid> SIZE=", "501 "),
        ("MAIL FROM:<a@example.invalid> SIZE=many", "501 "),
        ("MAIL FROM:<a@example.invalid> SIZE=-1", "501 "),
        (
            "MAIL FROM:<a@example.invalid> SIZE=99999999999999999999",
            "501 ",
        ),
        ("MAIL FROM:<a@example.invalid> BODY", "555 5.5.4"),
        // A recipient parameter is equally unannounced.
        ("MAIL FROM:<a@example.invalid>", "250 "),
    ] {
        send_line(&mut client, command).await;
        let reply = read_reply(&mut client).await;
        assert!(reply.starts_with(code), "{command} -> {reply}");
    }
    send_line(&mut client, "RCPT TO:<list@example.invalid> NOTIFY=NEVER").await;
    let reply = read_reply(&mut client).await;
    assert!(reply.starts_with("555 5.5.4"), "{reply}");
    send_line(&mut client, "RCPT TO:<list@example.invalid>").await;
    assert!(read_reply(&mut client).await.starts_with("250 "));
    send_line(&mut client, "QUIT").await;
    read_reply(&mut client).await;
    task.await.unwrap();
}

#[tokio::test]
async fn an_eight_bit_body_is_stored_byte_for_byte_after_a_bodied_mail_from() {
    let (mut client, inspect, task) = session_pair_inspectable(FakeHandler::new());
    read_reply(&mut client).await;
    send_line(&mut client, "LHLO client.example.invalid").await;
    read_reply(&mut client).await;
    send_line(
        &mut client,
        "MAIL FROM:<a@example.invalid> BODY=8BITMIME SIZE=64",
    )
    .await;
    assert!(read_reply(&mut client).await.starts_with("250 "));
    send_line(&mut client, "RCPT TO:<list@example.invalid>").await;
    read_reply(&mut client).await;
    send_line(&mut client, "DATA").await;
    read_reply(&mut client).await;
    let body = b"Subject: t\xc3\xaan\r\n\r\nch\xc3\xa0o b\xe1\xba\xa1n\r\n.\r\n";
    client.write_all(body).await.unwrap();
    assert!(read_reply(&mut client).await.starts_with("250 "));
    send_line(&mut client, "QUIT").await;
    read_reply(&mut client).await;
    task.await.unwrap();
    let deliveries = inspect.deliveries();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(
        deliveries[0].2,
        b"Subject: t\xc3\xaan\r\n\r\nch\xc3\xa0o b\xe1\xba\xa1n\r\n"
    );
}

// ---- The experimental SMTP mode: the same handler, EHLO and one DATA reply ----

fn smtp_session_pair(
    mut handler: FakeHandler,
) -> (tokio::io::DuplexStream, tokio::task::JoinHandle<()>) {
    let (client, server) = tokio::io::duplex(8192);
    let task = tokio::spawn(async move {
        serve_session_as(Protocol::Smtp, server, &mut handler)
            .await
            .unwrap();
    });
    (client, task)
}

#[tokio::test]
async fn smtp_mode_greets_ehlo_and_helo_and_refuses_lhlo() {
    let (mut client, _task) = smtp_session_pair(FakeHandler::new());
    let greeting = read_reply(&mut client).await;
    assert!(
        greeting.starts_with("220 ") && greeting.contains("ESMTP"),
        "{greeting}"
    );
    send_line(&mut client, "LHLO mx.client.invalid").await;
    assert!(read_reply(&mut client).await.starts_with("500 "));
    send_line(&mut client, "MAIL FROM:<alice@example.invalid>").await;
    let reply = read_reply(&mut client).await;
    assert!(
        reply.starts_with("503 ") && reply.contains("EHLO"),
        "{reply}"
    );
    send_line(&mut client, "EHLO mx.client.invalid").await;
    let reply = read_reply(&mut client).await;
    assert!(reply.starts_with("250-mx.example.invalid"), "{reply}");
    assert!(
        reply.contains("PIPELINING") && reply.contains("SIZE 1024"),
        "{reply}"
    );
    send_line(&mut client, "HELO mx.client.invalid").await;
    assert_eq!(
        read_reply(&mut client).await,
        "250 2.0.0 mx.example.invalid\r\n"
    );
    send_line(&mut client, "QUIT").await;
    assert!(read_reply(&mut client).await.starts_with("221 "));
}

async fn smtp_transaction(client: &mut tokio::io::DuplexStream, recipients: usize) {
    read_reply(client).await;
    send_line(client, "EHLO mx.client.invalid").await;
    read_reply(client).await;
    send_line(client, "MAIL FROM:<alice@example.invalid>").await;
    assert!(read_reply(client).await.starts_with("250 "));
    for _ in 0..recipients {
        send_line(client, "RCPT TO:<list@example.invalid>").await;
        assert!(read_reply(client).await.starts_with("250 "));
    }
    send_line(client, "DATA").await;
    assert!(read_reply(client).await.starts_with("354 "));
    send_line(client, "Subject: over smtp").await;
    send_line(client, "").await;
    send_line(client, "body").await;
    send_line(client, ".").await;
}

/// Exactly one reply after `DATA`, however many recipients: a `NOOP`
/// straight after is answered by itself, with no stray reply ahead of it.
#[tokio::test]
async fn smtp_mode_answers_data_once_for_every_recipient() {
    let handler = FakeHandler::new();
    let inspect = handler.clone();
    let (mut client, _task) = smtp_session_pair(handler);
    smtp_transaction(&mut client, 2).await;
    let reply = read_reply(&mut client).await;
    assert_eq!(reply, "250 2.1.5 delivered\r\n");
    send_line(&mut client, "NOOP").await;
    assert_eq!(read_reply(&mut client).await, "250 2.0.0 ok\r\n");
    let deliveries = inspect.deliveries();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].1.len(), 2, "both recipients in one delivery");
    assert!(deliveries[0].2.starts_with(b"Subject: over smtp"));
}

#[tokio::test]
async fn smtp_mode_reports_a_refusal_and_an_oversized_message_once() {
    let mut refusing = FakeHandler::new();
    refusing.outcome = (451, "4.3.0 try later");
    let (mut client, _task) = smtp_session_pair(refusing);
    smtp_transaction(&mut client, 2).await;
    assert_eq!(read_reply(&mut client).await, "451 4.3.0 try later\r\n");
    send_line(&mut client, "NOOP").await;
    assert_eq!(read_reply(&mut client).await, "250 2.0.0 ok\r\n");
    // Fewer outcomes than recipients is the handler's fault, said once.
    let mut short = FakeHandler::new();
    short.result_count_override = Some(1);
    let (mut client, _task) = smtp_session_pair(short);
    smtp_transaction(&mut client, 2).await;
    assert!(read_reply(&mut client).await.starts_with("451 "));
    send_line(&mut client, "NOOP").await;
    assert_eq!(read_reply(&mut client).await, "250 2.0.0 ok\r\n");
    // Too large: one 552, and nothing delivered.
    let mut small = FakeHandler::new();
    small.max_message_bytes = 16;
    let inspect = small.clone();
    let (mut client, _task) = smtp_session_pair(small);
    smtp_transaction(&mut client, 2).await;
    assert!(read_reply(&mut client).await.starts_with("552 "));
    send_line(&mut client, "NOOP").await;
    assert_eq!(read_reply(&mut client).await, "250 2.0.0 ok\r\n");
    assert!(inspect.deliveries().is_empty());
}
