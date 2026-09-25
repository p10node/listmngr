//! A real RFC 2033 LMTP session state machine over any async byte stream.
//!
//! This module only implements the protocol; binding a socket, applying
//! posting policy, and durable intake are the caller's responsibility via
//! [`LmtpHandler`]. No relaying, no unauthenticated capability claims: only
//! the commands and extensions actually honored here are advertised.
use std::io::{Error as IoError, ErrorKind, Result as IoResult};
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::time::Instant;

/// Maximum bytes in one command or data line, including its terminator.
pub const MAX_LINE_BYTES: usize = 8192;

/// Outcome of one accepted recipient's durable intake, reported as its own
/// LMTP reply after `DATA` completes.
///
/// `code` must be a real 2xx/4xx/5xx SMTP reply code (e.g. `250` delivered,
/// `451` temporary storage failure, `550` permanently invalid) so a temporary
/// failure is retried by the peer instead of treated as a permanent bounce.
#[derive(Debug, Clone)]
pub struct RecipientOutcome {
    pub code: u16,
    /// Enhanced-status-free reply text; must never include secrets or raw error chains.
    pub detail: String,
}

impl RecipientOutcome {
    #[must_use]
    pub const fn is_success(&self) -> bool {
        self.code / 100 == 2
    }
}

/// Typed RCPT rejection; dependency failures must never become permanent bounces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecipientRejection {
    Permanent(String),
    Temporary(String),
}

impl RecipientRejection {
    #[must_use]
    pub const fn code(&self) -> u16 {
        match self {
            Self::Permanent(_) => 550,
            Self::Temporary(_) => 451,
        }
    }
    #[must_use]
    pub fn detail(&self) -> &str {
        match self {
            Self::Permanent(detail) | Self::Temporary(detail) => detail,
        }
    }
}

/// Caller-supplied policy and durable-intake hooks for one LMTP session.
///
/// Implementations must keep `deliver` fast: policy evaluation and delivery
/// happen asynchronously afterward via the durable `in` queue, not here.
pub trait LmtpHandler: Send {
    fn local_hostname(&self) -> &str;
    fn max_message_bytes(&self) -> usize;
    fn max_recipients(&self) -> usize;
    fn command_timeout(&self) -> Duration;
    /// Validate and resolve one `RCPT TO` address. `Err` becomes a 550 reply
    /// with the given (non-secret) reason; never route to relay or an unknown/
    /// unsupported command address.
    fn accept_recipient(
        &mut self,
        address: &str,
    ) -> impl Future<Output = Result<(), String>> + Send;
    /// Typed validation hook. Legacy handlers retain permanent string rejections.
    fn validate_recipient(
        &mut self,
        address: &str,
    ) -> impl Future<Output = Result<(), RecipientRejection>> + Send {
        async move {
            self.accept_recipient(address)
                .await
                .map_err(RecipientRejection::Permanent)
        }
    }
    /// Durably store the exact message bytes for every previously accepted
    /// recipient of this transaction, returning one outcome per recipient in
    /// the same order. Must complete (or fail) before any 250 is sent.
    /// The future may be dropped on timeout: stage all successful recipients
    /// and their audits in a single transaction, never commit per recipient.
    /// Cancellation before COMMIT must roll back the batch. Cancellation while
    /// COMMIT is in flight can still leave an all-or-none ambiguous outcome.
    fn deliver(
        &mut self,
        mail_from: Option<&str>,
        recipients: &[String],
        data: &[u8],
    ) -> impl Future<Output = Vec<RecipientOutcome>> + Send;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Init,
    Greeted,
    MailFrom,
    RcptTo,
}

/// One `MAIL FROM`/`RCPT TO` path: absent (`<>`, a null reverse path) or an address.
enum Path {
    Null,
    Address(String),
}

struct Session {
    state: State,
    mail_from: Path,
    have_mail_from: bool,
    recipients: Vec<String>,
}

impl Session {
    const fn new() -> Self {
        Self {
            state: State::Init,
            mail_from: Path::Null,
            have_mail_from: false,
            recipients: Vec::new(),
        }
    }

    fn reset_transaction(&mut self) {
        self.have_mail_from = false;
        self.mail_from = Path::Null;
        self.recipients.clear();
    }

    const fn mail_from_str(&self) -> Option<&str> {
        match &self.mail_from {
            Path::Null => None,
            Path::Address(address) => Some(address.as_str()),
        }
    }
}

/// Read one line, bounded by both a byte cap and a fixed deadline (not reset
/// per chunk, so a slow-but-steady drip cannot hold the connection open past
/// `deadline` regardless of how it paces its bytes). Returns `Ok(None)` only
/// on a clean EOF with no partial line pending.
async fn read_capped_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    cap: usize,
    deadline: Instant,
) -> IoResult<Option<Vec<u8>>> {
    let mut out = Vec::new();
    loop {
        let buf = tokio::time::timeout_at(deadline, reader.fill_buf())
            .await
            .map_err(|_| IoError::new(ErrorKind::TimedOut, "session deadline exceeded"))??;
        if buf.is_empty() {
            return Ok(if out.is_empty() { None } else { Some(out) });
        }
        if let Some(position) = buf.iter().position(|byte| *byte == b'\n') {
            out.extend_from_slice(&buf[..=position]);
            reader.consume(position + 1);
            if out.len() > cap {
                return Err(IoError::new(ErrorKind::InvalidData, "line too long"));
            }
            return Ok(Some(out));
        }
        let consumed = buf.len();
        out.extend_from_slice(buf);
        reader.consume(consumed);
        if out.len() > cap {
            return Err(IoError::new(ErrorKind::InvalidData, "line too long"));
        }
    }
}

fn trim_line(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// Whether `text` already starts with a well-formed `class.N.M ` enhanced
/// status code (RFC 3463) matching `class`.
fn has_enhanced_prefix(class: u16, text: &str) -> bool {
    let Some(rest) = text.strip_prefix(&format!("{class}.")) else {
        return false;
    };
    let Some((subject, rest)) = rest.split_once('.') else {
        return false;
    };
    if subject.is_empty() || !subject.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let Some((detail, after)) = rest.split_once(' ') else {
        return false;
    };
    !detail.is_empty() && detail.bytes().all(|b| b.is_ascii_digit()) && !after.is_empty()
}

/// Prepend a default `class.0.0` enhanced status code (RFC 2034/3463) to
/// `text` unless it already carries one matching `code`'s class. Classes
/// other than 2/4/5 (e.g. `354`'s class 3) have no enhanced-status
/// convention and are left untouched.
fn ensure_enhanced(code: u16, text: &str) -> String {
    let class = code / 100;
    if !matches!(class, 2 | 4 | 5) {
        return text.to_owned();
    }
    if has_enhanced_prefix(class, text) {
        text.to_owned()
    } else {
        format!("{class}.0.0 {text}")
    }
}

async fn write_with_timeout<W: AsyncWrite + Unpin>(
    writer: &mut W,
    timeout: Duration,
    bytes: &[u8],
) -> IoResult<()> {
    tokio::time::timeout(timeout, writer.write_all(bytes))
        .await
        .map_err(|_| IoError::new(ErrorKind::TimedOut, "LMTP write deadline exceeded"))??;
    Ok(())
}

async fn flush_with_timeout<W: AsyncWrite + Unpin>(
    writer: &mut W,
    timeout: Duration,
) -> IoResult<()> {
    tokio::time::timeout(timeout, writer.flush())
        .await
        .map_err(|_| IoError::new(ErrorKind::TimedOut, "LMTP flush deadline exceeded"))??;
    Ok(())
}

/// Write a reply exactly as given, with no enhanced-status injection. Only
/// for the `LHLO` capability announcement, whose lines are keywords, not
/// reply prose.
///
/// Bounded by `timeout`, exactly like every read in this module: a peer that
/// stops reading (backpressure) must not hold the session open forever, not
/// even while the server is only trying to report a timeout to it.
async fn reply_raw<W: AsyncWrite + Unpin>(
    writer: &mut W,
    timeout: Duration,
    code: u16,
    lines: &[&str],
) -> IoResult<()> {
    let (last, head) = lines
        .split_last()
        .expect("reply is always called with a line");
    for line in head {
        write_with_timeout(writer, timeout, format!("{code}-{line}\r\n").as_bytes()).await?;
    }
    write_with_timeout(writer, timeout, format!("{code} {last}\r\n").as_bytes()).await?;
    flush_with_timeout(writer, timeout).await
}

/// Write a reply, ensuring every line carries a valid enhanced status code
/// (RFC 2034) for its class. Use [`reply_raw`] instead for the `LHLO`
/// capability lines.
async fn reply<W: AsyncWrite + Unpin>(
    writer: &mut W,
    timeout: Duration,
    code: u16,
    lines: &[&str],
) -> IoResult<()> {
    let owned: Vec<String> = lines
        .iter()
        .map(|line| ensure_enhanced(code, line))
        .collect();
    let borrowed: Vec<&str> = owned.iter().map(String::as_str).collect();
    reply_raw(writer, timeout, code, &borrowed).await
}

/// Parse a `<...>` path and return it with whatever follows it (the ESMTP
/// parameters). `None` means malformed input (no `Err` variant needed since
/// callers only report a fixed 501 reply on failure).
fn parse_path(rest: &str) -> Option<(Path, &str)> {
    let rest = rest.trim_start();
    let open = rest.find('<')?;
    let close = rest[open..].find('>').map(|i| open + i)?;
    let inner = &rest[open + 1..close];
    let parameters = rest[close + 1..].trim();
    let path = if inner.is_empty() {
        Path::Null
    } else if inner.contains(char::is_control) || inner.contains(' ') {
        return None;
    } else {
        Path::Address(inner.to_owned())
    };
    Some((path, parameters))
}

/// Why an ESMTP parameter list was refused. Only the extensions this server
/// announces in `LHLO` may be used (RFC 1869 §4): anything else is a 555,
/// never silently ignored, so a client never believes in a service (DSN,
/// CHUNKING, AUTH) that is not implemented here.
enum ParameterError {
    /// 555 5.5.4: syntactically fine, but not an announced extension.
    Unsupported,
    /// 501: an announced parameter with an unusable value.
    Malformed,
    /// 552 5.3.4: `SIZE=` exceeds the announced maximum, refused now rather
    /// than after the peer has sent the whole message (RFC 1870 §6.1).
    TooLarge,
}

/// Validate `MAIL FROM` parameters: `SIZE=` (RFC 1870) and `BODY=` (RFC
/// 6152) are the only ones announced.
fn check_mail_parameters(parameters: &str, max_message_bytes: usize) -> Result<(), ParameterError> {
    for parameter in parameters.split_ascii_whitespace() {
        let (keyword, value) = parameter
            .split_once('=')
            .ok_or(ParameterError::Unsupported)?;
        if keyword.eq_ignore_ascii_case("SIZE") {
            let declared: usize = value.parse().map_err(|_| ParameterError::Malformed)?;
            if declared > max_message_bytes {
                return Err(ParameterError::TooLarge);
            }
        } else if keyword.eq_ignore_ascii_case("BODY") {
            // BINARYMIME (RFC 3030) needs CHUNKING, which is not offered.
            if !value.eq_ignore_ascii_case("7BIT") && !value.eq_ignore_ascii_case("8BITMIME") {
                return Err(ParameterError::Unsupported);
            }
        } else {
            return Err(ParameterError::Unsupported);
        }
    }
    Ok(())
}

async fn reply_parameter_error<W: AsyncWrite + Unpin>(
    writer: &mut W,
    timeout: Duration,
    error: &ParameterError,
) -> IoResult<()> {
    match error {
        ParameterError::Unsupported => {
            reply(
                writer,
                timeout,
                555,
                &["5.5.4 unsupported or unannounced parameter"],
            )
            .await
        }
        ParameterError::Malformed => {
            reply(writer, timeout, 501, &["malformed parameter value"]).await
        }
        ParameterError::TooLarge => {
            reply(
                writer,
                timeout,
                552,
                &["5.3.4 declared message size exceeds the announced maximum"],
            )
            .await
        }
    }
}

async fn handle_rset<W: AsyncWrite + Unpin>(
    session: &mut Session,
    writer: &mut W,
    timeout: Duration,
) -> IoResult<()> {
    session.reset_transaction();
    if session.state != State::Init {
        session.state = State::Greeted;
    }
    reply(writer, timeout, 250, &["ok"]).await
}

async fn handle_mail<W: AsyncWrite + Unpin>(
    protocol: Protocol,
    session: &mut Session,
    max_message_bytes: usize,
    writer: &mut W,
    timeout: Duration,
    rest: &str,
) -> IoResult<()> {
    if session.state == State::Init {
        return reply(
            writer,
            timeout,
            503,
            &[&format!("send {} first", protocol.greeting_verb())],
        )
        .await;
    }
    let Some(rest) = rest
        .strip_prefix("FROM:")
        .or_else(|| rest.strip_prefix("from:"))
    else {
        return reply(writer, timeout, 501, &["malformed MAIL FROM"]).await;
    };
    let Some((path, parameters)) = parse_path(rest) else {
        return reply(writer, timeout, 501, &["malformed reverse-path"]).await;
    };
    // Parameters are checked before the transaction opens, so a refusal
    // leaves no half-built transaction behind.
    if let Err(error) = check_mail_parameters(parameters, max_message_bytes) {
        return reply_parameter_error(writer, timeout, &error).await;
    }
    session.mail_from = path;
    session.have_mail_from = true;
    session.recipients.clear();
    session.state = State::MailFrom;
    reply(writer, timeout, 250, &["ok"]).await
}

async fn handle_rcpt<W: AsyncWrite + Unpin, H: LmtpHandler>(
    session: &mut Session,
    handler: &mut H,
    writer: &mut W,
    timeout: Duration,
    rest: &str,
) -> IoResult<()> {
    if !session.have_mail_from {
        return reply(writer, timeout, 503, &["send MAIL FROM first"]).await;
    }
    if session.recipients.len() >= handler.max_recipients() {
        return reply(writer, timeout, 452, &["too many recipients"]).await;
    }
    let Some(rest) = rest
        .strip_prefix("TO:")
        .or_else(|| rest.strip_prefix("to:"))
    else {
        return reply(writer, timeout, 501, &["malformed RCPT TO"]).await;
    };
    match parse_path(rest) {
        // No RCPT extension (DSN's NOTIFY/ORCPT included) is announced.
        Some((_, parameters)) if !parameters.is_empty() => {
            reply_parameter_error(writer, timeout, &ParameterError::Unsupported).await
        }
        Some((Path::Address(address), _)) => {
            match tokio::time::timeout(timeout, handler.validate_recipient(&address)).await {
                Ok(Ok(())) => {
                    session.recipients.push(address);
                    session.state = State::RcptTo;
                    reply(writer, timeout, 250, &["ok"]).await
                }
                Ok(Err(reason)) => reply(writer, timeout, reason.code(), &[reason.detail()]).await,
                // The hook did not finish within the deadline: we genuinely
                // do not know whether the recipient is valid, so this fails
                // closed (never added to the transaction) with a transient
                // reply rather than fabricating an acceptance or a false
                // permanent rejection.
                Err(_elapsed) => {
                    reply(writer, timeout, 451, &["recipient validation timed out"]).await
                }
            }
        }
        _ => {
            reply(
                writer,
                timeout,
                501,
                &["malformed or unsupported forward-path"],
            )
            .await
        }
    }
}

enum DataOutcome {
    Body(Vec<u8>),
    TooLarge,
}

async fn read_data<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    cap: usize,
    deadline: Instant,
) -> IoResult<DataOutcome> {
    let mut body = Vec::new();
    let mut oversized = false;
    loop {
        // The same deadline covers every line, including the post-oversize
        // discard/drain phase: a stalled or endlessly-dripping peer cannot
        // hold the session (or an unbounded-time drain) open past it.
        let Some(line) = read_capped_line(reader, MAX_LINE_BYTES, deadline).await? else {
            return Err(IoError::new(
                ErrorKind::UnexpectedEof,
                "connection closed mid-DATA",
            ));
        };
        if line == b".\r\n" || line == b".\n" {
            break;
        }
        let line: &[u8] = if line.starts_with(b"..") {
            &line[1..]
        } else {
            &line
        };
        if !oversized {
            if body.len() + line.len() > cap {
                oversized = true;
            } else {
                body.extend_from_slice(line);
            }
        }
    }
    Ok(if oversized {
        DataOutcome::TooLarge
    } else {
        DataOutcome::Body(body)
    })
}

/// Whether the caller loop should keep reading commands or end the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Continue {
    Yes,
    No,
}

async fn handle_data<R: AsyncBufRead + Unpin, W: AsyncWrite + Unpin, H: LmtpHandler>(
    protocol: Protocol,
    session: &mut Session,
    handler: &mut H,
    reader: &mut R,
    writer: &mut W,
    timeout: Duration,
) -> IoResult<Continue> {
    if session.state != State::RcptTo {
        reply(writer, timeout, 503, &["need at least one accepted RCPT"]).await?;
        return Ok(Continue::Yes);
    }
    reply(
        writer,
        timeout,
        354,
        &["start mail input; end with <CRLF>.<CRLF>"],
    )
    .await?;
    let deadline = Instant::now() + timeout;
    let outcome = match read_data(reader, handler.max_message_bytes(), deadline).await {
        Ok(outcome) => outcome,
        Err(error) if error.kind() == ErrorKind::TimedOut => {
            reply(
                writer,
                timeout,
                421,
                &["timed out awaiting message data; closing"],
            )
            .await?;
            return Ok(Continue::No);
        }
        Err(error) => return Err(error),
    };
    match outcome {
        DataOutcome::TooLarge => {
            for _ in 0..protocol.data_replies(session.recipients.len()) {
                reply(writer, timeout, 552, &["message exceeds size limit"]).await?;
            }
        }
        DataOutcome::Body(body) => {
            match tokio::time::timeout(
                timeout,
                handler.deliver(session.mail_from_str(), &session.recipients, &body),
            )
            .await
            {
                Ok(results) if protocol == Protocol::Smtp => {
                    // SMTP answers the message once: the transaction's
                    // outcome, summarised over its recipients.
                    let (code, detail) = summarize(&results, session.recipients.len());
                    reply(writer, timeout, code, &[&detail]).await?;
                }
                Ok(results) => {
                    // Exactly one reply per accepted RCPT, in order: never
                    // fewer (pad with an accounting-error reply) and never
                    // more (a misbehaving handler returning extra outcomes
                    // must not desync pipelining by sending replies the peer
                    // never expected).
                    for index in 0..session.recipients.len() {
                        match results.get(index) {
                            Some(result) => {
                                reply(writer, timeout, result.code, &[&result.detail]).await?;
                            }
                            None => {
                                reply(
                                    writer,
                                    timeout,
                                    451,
                                    &["internal delivery accounting error"],
                                )
                                .await?;
                            }
                        }
                    }
                }
                // The durable-intake hook did not finish within the deadline:
                // whether it already committed for some or all recipients is
                // genuinely unknown. Never report 250 here (a false
                // success); a uniform transient reply is the only honest
                // answer, matching plain SMTP/LMTP's documented inability to
                // guarantee exactly-once delivery under this kind of
                // ambiguity.
                Err(_elapsed) => {
                    for _ in 0..protocol.data_replies(session.recipients.len()) {
                        reply(
                            writer,
                            timeout,
                            451,
                            &["delivery outcome unknown; do not resend without checking"],
                        )
                        .await?;
                    }
                }
            }
        }
    }
    session.state = State::Greeted;
    session.reset_transaction();
    Ok(Continue::Yes)
}

/// The greeting verbs. RFC 2033 §4.1: an LMTP server MUST NOT implement
/// EHLO/HELO, only LHLO; an SMTP server the reverse.
async fn handle_greeting<W: AsyncWrite + Unpin>(
    protocol: Protocol,
    verb: &str,
    session: &mut Session,
    hostname: &str,
    max_message_bytes: usize,
    writer: &mut W,
    timeout: Duration,
) -> IoResult<()> {
    let verb = verb.to_ascii_uppercase();
    match (protocol, verb.as_str()) {
        (Protocol::Lmtp, "LHLO") | (Protocol::Smtp, "EHLO") => {
            session.state = State::Greeted;
            session.reset_transaction();
            reply_raw(
                writer,
                timeout,
                250,
                &[
                    hostname,
                    "PIPELINING",
                    &format!("SIZE {max_message_bytes}"),
                    "8BITMIME",
                    "ENHANCEDSTATUSCODES",
                ],
            )
            .await
        }
        (Protocol::Smtp, "HELO") => {
            session.state = State::Greeted;
            session.reset_transaction();
            reply(writer, timeout, 250, &[hostname]).await
        }
        (Protocol::Lmtp, _) => {
            reply(
                writer,
                timeout,
                500,
                &["this is LMTP; use LHLO, not EHLO/HELO (RFC 2033 4.1)"],
            )
            .await
        }
        (Protocol::Smtp, _) => {
            reply(
                writer,
                timeout,
                500,
                &["this is SMTP; use EHLO or HELO, not LHLO"],
            )
            .await
        }
    }
}

/// The one SMTP reply for a transaction: every recipient taken → the
/// first outcome's `250`; none taken → the first refusal as it stands;
/// some taken → `250` naming how many, since the taken ones are already
/// durable and the peer must not resend them (the refused ones are the
/// handler's to report, as with any SMTP server that accepts a message
/// for part of its recipients).
fn summarize(results: &[RecipientOutcome], expected: usize) -> (u16, String) {
    if results.len() < expected {
        return (451, "internal delivery accounting error".into());
    }
    let taken = results
        .iter()
        .take(expected)
        .filter(|outcome| (200..300).contains(&outcome.code))
        .count();
    match results.first() {
        Some(first) if taken == expected => (250, first.detail.clone()),
        Some(_) if taken > 0 => (
            250,
            format!("2.1.5 accepted for {taken} of {expected} recipients"),
        ),
        Some(first) => (first.code, first.detail.clone()),
        None => (451, "internal delivery accounting error".into()),
    }
}

/// Drive one LMTP session to completion (`QUIT` or peer disconnect).
/// # Errors
/// Returns an I/O error for transport failures; protocol violations (bad
/// commands, oversize lines) are reported to the peer, not returned here.
pub async fn serve_session<S, H>(stream: S, handler: &mut H) -> IoResult<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
    H: LmtpHandler,
{
    serve_session_as(Protocol::Lmtp, stream, handler).await
}

/// The wire protocol a session speaks.
///
/// LMTP behind an MTA (RFC 2033), or SMTP straight from the network (RFC
/// 5321) for the experimental inbound listener. They differ in the
/// greeting verb — `LHLO` against `EHLO`/`HELO` — and in how `DATA` is
/// answered: LMTP once per accepted recipient, SMTP once for the message.
/// Everything else, the recipient check included, is the same handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Lmtp,
    Smtp,
}

impl Protocol {
    /// What the greeting banner and the logs call it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Lmtp => "LMTP",
            Self::Smtp => "ESMTP",
        }
    }

    const fn greeting_verb(self) -> &'static str {
        match self {
            Self::Lmtp => "LHLO",
            Self::Smtp => "EHLO",
        }
    }

    /// How many replies `DATA` gets for `recipients` accepted recipients.
    const fn data_replies(self, recipients: usize) -> usize {
        match self {
            Self::Lmtp => recipients,
            Self::Smtp => 1,
        }
    }
}

/// Drive one session in `protocol` to completion (`QUIT` or peer
/// disconnect).
/// # Errors
/// Returns an I/O error for transport failures; protocol violations are
/// answered on the wire, not returned.
pub async fn serve_session_as<S, H>(protocol: Protocol, stream: S, handler: &mut H) -> IoResult<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
    H: LmtpHandler,
{
    let (read_half, mut writer) = tokio::io::split(stream);
    let mut reader = tokio::io::BufReader::new(read_half);
    let timeout = handler.command_timeout();
    reply(
        &mut writer,
        timeout,
        220,
        &[&format!(
            "{} listmngr {} ready",
            handler.local_hostname(),
            protocol.name()
        )],
    )
    .await?;

    let mut session = Session::new();
    loop {
        let timeout = handler.command_timeout();
        let deadline = Instant::now() + timeout;
        let line = match read_capped_line(&mut reader, MAX_LINE_BYTES, deadline).await {
            Ok(Some(line)) => line,
            Ok(None) => return Ok(()),
            Err(error) if error.kind() == ErrorKind::InvalidData => {
                reply(&mut writer, timeout, 500, &["line too long"]).await?;
                return Ok(());
            }
            Err(error) if error.kind() == ErrorKind::TimedOut => {
                reply(&mut writer, timeout, 421, &["command timeout"]).await?;
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let text = String::from_utf8_lossy(trim_line(&line)).into_owned();
        let (verb, rest) = text.split_once(' ').unwrap_or((text.as_str(), ""));
        match verb.to_ascii_uppercase().as_str() {
            "LHLO" | "EHLO" | "HELO" => {
                handle_greeting(
                    protocol,
                    verb,
                    &mut session,
                    handler.local_hostname(),
                    handler.max_message_bytes(),
                    &mut writer,
                    timeout,
                )
                .await?;
            }
            "NOOP" => reply(&mut writer, timeout, 250, &["ok"]).await?,
            "RSET" => handle_rset(&mut session, &mut writer, timeout).await?,
            "QUIT" => {
                reply(&mut writer, timeout, 221, &["bye"]).await?;
                return Ok(());
            }
            "MAIL" => {
                handle_mail(
                    protocol,
                    &mut session,
                    handler.max_message_bytes(),
                    &mut writer,
                    timeout,
                    rest,
                )
                .await?;
            }
            "RCPT" => handle_rcpt(&mut session, handler, &mut writer, timeout, rest).await?,
            "DATA" => {
                if handle_data(
                    protocol,
                    &mut session,
                    handler,
                    &mut reader,
                    &mut writer,
                    timeout,
                )
                .await?
                    == Continue::No
                {
                    return Ok(());
                }
            }
            _ => {
                reply(
                    &mut writer,
                    timeout,
                    500,
                    &["unrecognized or unsupported command"],
                )
                .await?;
            }
        }
    }
}
