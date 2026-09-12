//! One-shot SMTP transactions over explicit trusted plaintext or verified REQUIRED STARTTLS.
//!
//! The runner uses `send_secure`; `send` is the explicit plaintext library entry point.
//! Optional AUTH PLAIN requires verified REQUIRED TLS; no opportunistic or implicit TLS.
//! SMTP cannot guarantee exactly-once delivery: a `250` after `DATA` means
//! the relay accepted responsibility, not that a mailbox received the mail.
mod auth;
mod tls;
use listmngr_core::{SmtpFailure, SmtpFailureStage};
use std::io::{Error as IoError, ErrorKind, Result as IoResult};
use std::time::Duration;
pub use tls::{AuthenticationPolicy, TransportSecurity, send_secure, send_secure_with_envid};
use tokio::io::{AsyncBufRead, AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::time::Instant;

/// Maximum bytes in one response line, including its terminator.
const MAX_LINE_BYTES: usize = 8192;
/// Maximum continuation lines accepted for one multiline response.
const MAX_RESPONSE_LINES: usize = 200;
/// Maximum total bytes accepted for one (possibly multiline) response.
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

/// Final disposition of one recipient in a single SMTP transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecipientStatus {
    /// The relay accepted the message for this recipient (not proof of final delivery).
    Sent,
    /// A 4xx response, or a connection failure before anything message-specific
    /// was sent; safe to retry later with backoff.
    TransientFailure(String),
    /// Local validation or legacy failure without remote metadata; must not be retried.
    PermanentFailure(String),
    /// A typed remote 5xx, unlike local validation or legacy string failures.
    RemotePermanentFailure {
        failure: listmngr_core::SmtpFailure,
        detail: String,
    },
    /// The connection was lost after the full message was already written to
    /// the relay but before its final reply was read: the relay may or may
    /// not have accepted the message. Retrying risks a duplicate; collapsing
    /// this into an ordinary transient failure would understate that risk.
    Ambiguous(String),
}

#[derive(Debug, Clone)]
pub struct SmtpClientConfig {
    pub local_hostname: String,
    pub command_timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct SendOutcome {
    /// One entry per input recipient, in the same order.
    pub results: Vec<RecipientStatus>,
}

struct Response {
    code: u16,
    text: String,
    extensions: Vec<String>,
}

/// A value used inside one SMTP command line (hostname, envelope address)
/// must contain no bare CR or LF: either would splice extra commands onto
/// the wire that the caller never intended (command injection).
fn is_safe_smtp_text(value: &str) -> bool {
    !value.bytes().any(|b| b == b'\r' || b == b'\n')
}

async fn read_capped_line_by<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    cap: usize,
    deadline: Instant,
) -> IoResult<Vec<u8>> {
    use tokio::io::AsyncBufReadExt;
    let mut out = Vec::new();
    loop {
        let buf = tokio::time::timeout_at(deadline, reader.fill_buf())
            .await
            .map_err(|_| IoError::new(ErrorKind::TimedOut, "SMTP response deadline exceeded"))??;
        if buf.is_empty() {
            return Err(IoError::new(
                ErrorKind::UnexpectedEof,
                "relay closed connection",
            ));
        }
        if let Some(position) = buf.iter().position(|byte| *byte == b'\n') {
            out.extend_from_slice(&buf[..=position]);
            reader.consume(position + 1);
            if out.len() > cap {
                return Err(IoError::new(
                    ErrorKind::InvalidData,
                    "SMTP response line too long",
                ));
            }
            return Ok(out);
        }
        let consumed = buf.len();
        out.extend_from_slice(buf);
        reader.consume(consumed);
        if out.len() > cap {
            return Err(IoError::new(
                ErrorKind::InvalidData,
                "SMTP response line too long",
            ));
        }
    }
}

fn trim_crlf(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// Read one (possibly multiline) SMTP response, byte-safe and bounded: a
/// single deadline covers the whole response (never reset per line), with
/// caps on line length, continuation-line count, and total bytes. Every
/// continuation line's code must match the first line's, and the separator
/// at byte 3 must be exactly `' '` (final) or `'-'` (continuation); anything
/// else is a hard parse error rather than a best-effort guess.
async fn read_response<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    timeout: Duration,
) -> IoResult<Response> {
    let deadline = Instant::now() + timeout;
    let mut first_code: Option<u16> = None;
    let mut lines: Vec<String> = Vec::new();
    let mut total_bytes = 0usize;
    loop {
        if lines.len() >= MAX_RESPONSE_LINES {
            return Err(IoError::new(
                ErrorKind::InvalidData,
                "SMTP response exceeded maximum continuation lines",
            ));
        }
        let line = read_capped_line_by(reader, MAX_LINE_BYTES, deadline).await?;
        total_bytes += line.len();
        if total_bytes > MAX_RESPONSE_BYTES {
            return Err(IoError::new(
                ErrorKind::InvalidData,
                "SMTP response exceeded maximum total bytes",
            ));
        }
        let trimmed = trim_crlf(&line);
        if trimmed.len() < 4 || !trimmed[..3].iter().all(u8::is_ascii_digit) {
            return Err(IoError::new(
                ErrorKind::InvalidData,
                "malformed SMTP response",
            ));
        }
        // Safe: exactly 3 verified ASCII digit bytes.
        let this_code: u16 = std::str::from_utf8(&trimmed[..3])
            .expect("ASCII digits are valid UTF-8")
            .parse()
            .expect("3 ASCII digits parse as u16");
        match first_code {
            None => first_code = Some(this_code),
            Some(code) if code == this_code => {}
            Some(_) => {
                return Err(IoError::new(
                    ErrorKind::InvalidData,
                    "SMTP multiline response code mismatch",
                ));
            }
        }
        let separator = trimmed[3];
        if separator != b' ' && separator != b'-' {
            return Err(IoError::new(
                ErrorKind::InvalidData,
                "malformed SMTP response separator",
            ));
        }
        lines.push(String::from_utf8_lossy(&trimmed[4..]).into_owned());
        if separator == b' ' {
            break;
        }
    }
    Ok(Response {
        code: first_code.expect("at least one line is always parsed"),
        text: lines.join("; "),
        extensions: lines.into_iter().skip(1).collect(),
    })
}

async fn write_with_timeout<W: AsyncWrite + Unpin>(
    writer: &mut W,
    timeout: Duration,
    bytes: &[u8],
) -> IoResult<()> {
    tokio::time::timeout(timeout, writer.write_all(bytes))
        .await
        .map_err(|_| IoError::new(ErrorKind::TimedOut, "SMTP write deadline exceeded"))??;
    Ok(())
}

async fn flush_with_timeout<W: AsyncWrite + Unpin>(
    writer: &mut W,
    timeout: Duration,
) -> IoResult<()> {
    tokio::time::timeout(timeout, writer.flush())
        .await
        .map_err(|_| IoError::new(ErrorKind::TimedOut, "SMTP flush deadline exceeded"))??;
    Ok(())
}

async fn command<W: AsyncWrite + Unpin, R: AsyncBufRead + Unpin>(
    writer: &mut W,
    reader: &mut R,
    timeout: Duration,
    line: &str,
) -> IoResult<Response> {
    write_with_timeout(writer, timeout, line.as_bytes()).await?;
    write_with_timeout(writer, timeout, b"\r\n").await?;
    flush_with_timeout(writer, timeout).await?;
    read_response(reader, timeout).await
}

fn dot_stuff(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 16);
    for line in data.split_inclusive(|b| *b == b'\n') {
        if line.starts_with(b".") {
            out.push(b'.');
        }
        out.extend_from_slice(line);
    }
    if !out.ends_with(b"\n") {
        out.extend_from_slice(b"\r\n");
    }
    out
}

fn all_transient(text: &str, count: usize) -> SendOutcome {
    SendOutcome {
        results: (0..count)
            .map(|_| RecipientStatus::TransientFailure(text.to_owned()))
            .collect(),
    }
}

const fn status_for(stage: SmtpFailureStage, code: u16, text: String) -> RecipientStatus {
    if code / 100 == 5 {
        RecipientStatus::RemotePermanentFailure {
            failure: SmtpFailure { stage, code },
            detail: text,
        }
    } else {
        RecipientStatus::TransientFailure(text)
    }
}

/// A negotiation stage's exact set of accepted success codes. Checking only
/// the response class (`code / 100 == 2`) would accept any 2xx code at any
/// stage — e.g. `221` (`QUIT`'s code) replying to `RCPT` — as if it were that
/// stage's real success code, on a desynced or malformed transcript.
#[derive(Debug, Clone, Copy)]
enum Stage {
    Ehlo,
    MailFrom,
    Rcpt,
}

const fn is_stage_success(stage: Stage, code: u16) -> bool {
    match stage {
        Stage::Ehlo | Stage::MailFrom => code == 250,
        Stage::Rcpt => code == 250 || code == 251,
    }
}

/// Whether the connection was lost before or after the message body was
/// fully written determines whether the outcome is safely retryable
/// ([`RecipientStatus::TransientFailure`]) or genuinely ambiguous
/// ([`RecipientStatus::Ambiguous`]).
#[derive(Debug, Clone, Copy)]
enum LossPhase {
    BeforeData,
    AfterData,
}

fn connection_lost(phase: LossPhase) -> RecipientStatus {
    match phase {
        LossPhase::BeforeData => RecipientStatus::TransientFailure("connection failure".into()),
        LossPhase::AfterData => RecipientStatus::Ambiguous(
            "connection lost after the message was written; remote outcome unknown".into(),
        ),
    }
}

/// Fill every still-unresolved (`None`) slot with `status`. Already-resolved
/// (`Some`) slots — a recipient's own known terminal outcome — are never
/// overwritten, so one recipient's later connection loss can never silently
/// reclassify another recipient's already-known permanent/transient result.
fn resolve_pending(results: &mut [Option<RecipientStatus>], status: &RecipientStatus) {
    for entry in results.iter_mut() {
        if entry.is_none() {
            *entry = Some(status.clone());
        }
    }
}

/// What the caller knows about 8-bit content and the relay's support for it
/// (RFC 6152). `relay_announced` is only consulted when this negotiation does
/// not run `EHLO` itself, which happens on the authenticated path.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct EightBit {
    /// The message contains octets above 0x7F.
    pub(crate) body: bool,
    pub(crate) relay_announced: bool,
}

/// Whether `data` needs the 8BITMIME extension.
pub(crate) fn is_eight_bit(data: &[u8]) -> bool {
    data.iter().any(|byte| !byte.is_ascii())
}

fn finish(results: Vec<Option<RecipientStatus>>) -> SendOutcome {
    SendOutcome {
        results: results
            .into_iter()
            .map(|entry| entry.expect("every recipient is resolved before finish"))
            .collect(),
    }
}

/// Negotiate `MAIL FROM`/`RCPT TO` for every recipient not already rejected
/// for unsafe content. Returns `Err(())` once `results` is fully resolved and
/// the caller must stop (already written into `results`); `Ok(())` means at
/// least one recipient is still pending and `DATA` should proceed.
#[allow(clippy::too_many_arguments)]
async fn negotiate<W: AsyncWrite + Unpin, R: AsyncBufRead + Unpin>(
    writer: &mut W,
    reader: &mut R,
    timeout: Duration,
    config: Option<&SmtpClientConfig>,
    mail_from: Option<&str>,
    recipients: &[String],
    results: &mut [Option<RecipientStatus>],
    dsn: Option<(&str, bool)>,
    eight_bit: EightBit,
) -> Result<(), ()> {
    let mut supports_dsn = dsn.is_some_and(|(_, supported)| supported);
    let mut supports_8bitmime = eight_bit.relay_announced;
    if let Some(config) = config {
        let Ok(ehlo) = command(
            writer,
            reader,
            timeout,
            &format!("EHLO {}", config.local_hostname),
        )
        .await
        else {
            resolve_pending(results, &connection_lost(LossPhase::BeforeData));
            return Err(());
        };
        if !is_stage_success(Stage::Ehlo, ehlo.code) {
            resolve_pending(
                results,
                &status_for(SmtpFailureStage::Ehlo, ehlo.code, ehlo.text),
            );
            return Err(());
        }
        supports_dsn = ehlo
            .extensions
            .iter()
            .any(|line| line.eq_ignore_ascii_case("DSN"));
        supports_8bitmime = ehlo
            .extensions
            .iter()
            .any(|line| line.eq_ignore_ascii_case("8BITMIME"));
    }
    // RFC 6152: an 8-bit body may only be handed to a relay that announced
    // 8BITMIME, and then only with the declaration. Sending it undeclared
    // would be a protocol violation whose outcome (mangling, rejection) is
    // the relay's choice, so this fails closed and retries instead.
    if eight_bit.body && !supports_8bitmime {
        resolve_pending(
            results,
            &RecipientStatus::TransientFailure(
                "relay does not announce 8BITMIME for an 8-bit message".into(),
            ),
        );
        return Err(());
    }
    if dsn.is_some() && !supports_dsn {
        resolve_pending(
            results,
            &RecipientStatus::TransientFailure("SMTP DSN capability required".into()),
        );
        return Err(());
    }
    let from = mail_from.unwrap_or("");
    let mut parameter = dsn.map_or_else(String::new, |(envid, _)| format!(" ENVID={envid}"));
    if eight_bit.body {
        parameter.push_str(" BODY=8BITMIME");
    }
    let Ok(mail) = command(
        writer,
        reader,
        timeout,
        &format!("MAIL FROM:<{from}>{parameter}"),
    )
    .await
    else {
        resolve_pending(results, &connection_lost(LossPhase::BeforeData));
        return Err(());
    };
    if !is_stage_success(Stage::MailFrom, mail.code) {
        resolve_pending(
            results,
            &status_for(SmtpFailureStage::MailFrom, mail.code, mail.text),
        );
        return Err(());
    }

    for (index, recipient) in recipients.iter().enumerate() {
        if !is_safe_smtp_text(recipient) {
            results[index] = Some(RecipientStatus::PermanentFailure(
                "invalid recipient address".into(),
            ));
            continue;
        }
        let Ok(rcpt) = command(writer, reader, timeout, &format!("RCPT TO:<{recipient}>")).await
        else {
            resolve_pending(results, &connection_lost(LossPhase::BeforeData));
            return Err(());
        };
        results[index] = if is_stage_success(Stage::Rcpt, rcpt.code) {
            None
        } else {
            Some(status_for(SmtpFailureStage::Rcpt, rcpt.code, rcpt.text))
        };
    }

    if results.iter().any(Option::is_none) {
        Ok(())
    } else {
        let _ = command(writer, reader, timeout, "RSET").await;
        Err(())
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_transaction<W: AsyncWrite + Unpin, R: AsyncBufRead + Unpin>(
    writer: &mut W,
    reader: &mut R,
    timeout: Duration,
    config: Option<&SmtpClientConfig>,
    mail_from: Option<&str>,
    recipients: &[String],
    data: &[u8],
    dsn: Option<(&str, bool)>,
    eight_bit_relay: bool,
) -> SendOutcome {
    let mut results: Vec<Option<RecipientStatus>> = vec![None; recipients.len()];
    if negotiate(
        writer,
        reader,
        timeout,
        config,
        mail_from,
        recipients,
        &mut results,
        dsn,
        EightBit {
            body: is_eight_bit(data),
            relay_announced: eight_bit_relay,
        },
    )
    .await
    .is_err()
    {
        return finish(results);
    }

    let Ok(data_start) = command(writer, reader, timeout, "DATA").await else {
        resolve_pending(&mut results, &connection_lost(LossPhase::BeforeData));
        return finish(results);
    };
    if data_start.code != 354 {
        resolve_pending(
            &mut results,
            &status_for(
                SmtpFailureStage::DataStart,
                data_start.code,
                data_start.text,
            ),
        );
        return finish(results);
    }

    let payload = dot_stuff(data);
    if write_with_timeout(writer, timeout, &payload).await.is_err() {
        // Nothing beyond (at most) a partial body reached the relay: no
        // terminator was ever attempted, so this cannot look like a
        // complete message and is safely retryable.
        resolve_pending(&mut results, &connection_lost(LossPhase::BeforeData));
        return finish(results);
    }
    // From here on the relay may already have the complete message body: a
    // failure writing the terminator, or flushing it, cannot rule out the
    // relay treating the transaction as complete. This is genuinely
    // ambiguous, not an ordinary (safely retryable) transient failure.
    if write_with_timeout(writer, timeout, b".\r\n").await.is_err()
        || flush_with_timeout(writer, timeout).await.is_err()
    {
        resolve_pending(&mut results, &connection_lost(LossPhase::AfterData));
        return finish(results);
    }

    let final_status = match read_response(reader, timeout).await {
        Ok(response) if response.code == 250 => RecipientStatus::Sent,
        Ok(response) => status_for(SmtpFailureStage::DataFinal, response.code, response.text),
        Err(_) => connection_lost(LossPhase::AfterData),
    };
    resolve_pending(&mut results, &final_status);
    // The final DATA result is authoritative. Closing this one-shot stream
    // is preferable to hiding that result behind potentially stalled QUIT I/O.
    finish(results)
}

/// Run one SMTP transaction over an already-connected plaintext stream.
/// # Errors
/// Returns an I/O error if `config.local_hostname`/`mail_from` contain a bare
/// CR or LF (validated before any I/O), or if the initial greeting cannot be
/// read. Every later failure degrades to a per-recipient [`RecipientStatus`]
/// instead, preserving each recipient's own already-known outcome.
pub async fn send<S>(
    stream: S,
    config: &SmtpClientConfig,
    mail_from: Option<&str>,
    recipients: &[String],
    data: &[u8],
) -> IoResult<SendOutcome>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    send_with_envid(stream, config, mail_from, recipients, data, None).await
}

fn validate_envid(
    envid: Option<&str>,
    sender: Option<&str>,
    recipients: &[String],
) -> IoResult<()> {
    if envid.is_some_and(|v| {
        v.is_empty()
            || v.len() > 100
            || !v
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
            || sender.is_none()
            || recipients.len() != 1
    }) {
        return Err(IoError::new(
            ErrorKind::InvalidInput,
            "invalid singleton DSN envelope",
        ));
    }
    Ok(())
}
async fn send_with_envid<S: AsyncRead + AsyncWrite + Unpin + Send>(
    stream: S,
    config: &SmtpClientConfig,
    mail_from: Option<&str>,
    recipients: &[String],
    data: &[u8],
    envid: Option<&str>,
) -> IoResult<SendOutcome> {
    validate_envid(envid, mail_from, recipients)?;
    if !is_safe_smtp_text(&config.local_hostname) {
        return Err(IoError::new(
            ErrorKind::InvalidInput,
            "unsafe SMTP local hostname",
        ));
    }
    if mail_from.is_some_and(|from| !is_safe_smtp_text(from)) {
        return Err(IoError::new(
            ErrorKind::InvalidInput,
            "unsafe SMTP envelope sender",
        ));
    }
    let (read_half, mut writer) = tokio::io::split(stream);
    let mut reader = tokio::io::BufReader::new(read_half);
    let timeout = config.command_timeout;
    let greeting = read_response(&mut reader, timeout).await?;
    if greeting.code != 220 {
        return Ok(all_transient(&greeting.text, recipients.len()));
    }
    Ok(run_transaction(
        &mut writer,
        &mut reader,
        timeout,
        Some(config),
        mail_from,
        recipients,
        data,
        envid.map(|v| (v, false)),
        false,
    )
    .await)
}
