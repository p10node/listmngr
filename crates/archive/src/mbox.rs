//! mbox files in and out of the archive.
//!
//! The format written and read is `mboxrd`: messages separated by a
//! `From ` line, every body line that would look like one quoted with a
//! leading `>` on the way out and unquoted once on the way in. Reading is
//! streaming, one message at a time, so an archive of a hundred thousand
//! posts never sits in memory at once.
use crate::identity;
use listmngr_core::{Error, ListId, Result};
use listmngr_db::Database;
use listmngr_db::archive::import::{ImportItem, Outcome};
use std::io::{self, BufRead, Write};

/// The separator line every message is written under. The archive keeps
/// no envelope date, so the epoch stands in, as Mailman's exports do.
pub const FROM_LINE: &[u8] = b"From archive@localhost Thu Jan  1 00:00:00 1970\n";

/// Write one message under a `From ` line, quoting lines that would end
/// it, and end it with a blank line.
/// # Errors
/// The writer's.
pub fn write_message(out: &mut impl Write, raw: &[u8]) -> io::Result<()> {
    out.write_all(FROM_LINE)?;
    let mut ended_with_newline = true;
    for line in raw.split_inclusive(|b| *b == b'\n') {
        if line
            .iter()
            .copied()
            .skip_while(|b| *b == b'>')
            .collect::<Vec<_>>()
            .starts_with(b"From ")
        {
            out.write_all(b">")?;
        }
        out.write_all(line)?;
        ended_with_newline = line.ends_with(b"\n");
    }
    if !ended_with_newline {
        out.write_all(b"\n")?;
    }
    out.write_all(b"\n")
}

/// Messages of an mbox, streamed from a reader.
pub struct Reader<R: BufRead> {
    input: R,
    /// The separator line that ended the last message and begins the
    /// next one; the next message consumes it before reading its body.
    pending: Option<Vec<u8>>,
}

impl<R: BufRead> std::fmt::Debug for Reader<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reader")
            .field("pending", &self.pending.is_some())
            .finish_non_exhaustive()
    }
}

impl<R: BufRead> Reader<R> {
    pub const fn new(input: R) -> Self {
        Self {
            input,
            pending: None,
        }
    }

    fn next_line(&mut self) -> io::Result<Option<Vec<u8>>> {
        if let Some(line) = self.pending.take() {
            return Ok(Some(line));
        }
        let mut line = Vec::new();
        let read = self.input.read_until(b'\n', &mut line)?;
        Ok((read > 0).then_some(line))
    }
}

fn is_separator(line: &[u8]) -> bool {
    line.starts_with(b"From ")
}

/// A `>From ` line loses one `>`; anything else passes through.
fn unquote(line: &[u8]) -> &[u8] {
    let quoted = line.iter().take_while(|b| **b == b'>').count();
    if quoted > 0 && line[quoted..].starts_with(b"From ") {
        &line[1..]
    } else {
        line
    }
}

impl<R: BufRead> Iterator for Reader<R> {
    type Item = io::Result<Vec<u8>>;

    fn next(&mut self) -> Option<Self::Item> {
        // Consume the separator this message begins under, skipping any
        // preamble before the first one. It is consumed here and not where
        // it was found, so that the line ending one message opens the next
        // exactly once.
        loop {
            match self.next_line() {
                Ok(Some(line)) if is_separator(&line) => break,
                Ok(Some(_)) => {}
                Ok(None) => return None,
                Err(error) => return Some(Err(error)),
            }
        }
        let mut message = Vec::new();
        loop {
            match self.next_line() {
                Ok(Some(line)) if is_separator(&line) => {
                    self.pending = Some(line);
                    break;
                }
                Ok(Some(line)) => message.extend_from_slice(unquote(&line)),
                Ok(None) => break,
                Err(error) => return Some(Err(error)),
            }
        }
        // The blank line that ended the message is the separator's, not the
        // message's.
        if message.ends_with(b"\n\n") {
            message.pop();
        } else if message.ends_with(b"\r\n\r\n") {
            message.truncate(message.len() - 2);
        }
        Some(Ok(message))
    }
}

/// One message as the importer stores it, parsed once here.
///
/// The database's batch then does only inserts. A message without a
/// `Message-ID` is given one derived from its bytes, written into the
/// stored copy so the hash and the headers agree.
/// # Errors
/// Returns validation for bytes that are not a message.
pub fn prepare(list: &ListId, mut raw: Vec<u8>, now_ms: i64) -> Result<ImportItem> {
    let parser = mail_parser::MessageParser::default();
    let existing = parser
        .parse(&raw)
        .ok_or_else(|| Error::Validation("invalid MIME".into()))?
        .message_id()
        .map(|id| format!("<{}>", id.trim_matches(|c| c == '<' || c == '>')));
    let message_id = match existing {
        Some(id) if listmngr_mail::message_id_hash(&id).is_ok() => id,
        _ => {
            use sha2::Digest as _;
            let digest = sha2::Sha256::digest(&raw);
            let id = format!("<import.{:x}@{}>", digest, list.mail_host());
            let mut with_id = format!("Message-ID: {id}\r\n").into_bytes();
            with_id.append(&mut raw);
            raw = with_id;
            id
        }
    };
    let parsed = parser
        .parse(&raw)
        .ok_or_else(|| Error::Validation("invalid MIME".into()))?;
    let identity = identity(&parsed, &message_id)?;
    let (sender_name, sender_email) = listmngr_db::archive::sender_of(&parsed);
    let attachments = listmngr_mail::attachments::stored(&raw).unwrap_or_default();
    Ok(ImportItem {
        hash: identity.hash,
        thread: identity.thread,
        parent: identity.parent,
        subject: parsed.subject().unwrap_or("").to_owned(),
        body: parsed.body_text(0).unwrap_or_default().into_owned(),
        sender_name,
        sender_email,
        date_ms: identity.date_ms,
        created_at: identity.date_ms.unwrap_or(now_ms),
        attachments,
        raw,
    })
}

/// Import every message of an mbox into a list's archive.
///
/// Messages go in `batch` at a time, one transaction each, and
/// `progress` hears the running count after every batch. Messages
/// already archived (same hash) are skipped; bytes that are not a
/// message are skipped too and counted.
/// # Errors
/// Read failures, a list whose archive is `never`, database failures.
pub async fn import<R: BufRead>(
    db: &Database,
    list: &ListId,
    reader: Reader<R>,
    batch: usize,
    now_ms: i64,
    mut progress: impl FnMut(&Outcome),
) -> Result<Outcome> {
    db.archive().ensure_importable(list).await?;
    let batch = batch.clamp(1, 5000);
    let mut outcome = Outcome::default();
    let mut items = Vec::with_capacity(batch);
    for message in reader {
        let raw = message.map_err(|error| Error::Validation(format!("reading mbox: {error}")))?;
        match prepare(list, raw, now_ms) {
            Ok(item) => items.push(item),
            Err(_) => outcome.skipped += 1,
        }
        if items.len() >= batch {
            outcome += db.archive().import_batch(list, &items, now_ms).await?;
            items.clear();
            progress(&outcome);
        }
    }
    if !items.is_empty() {
        outcome += db.archive().import_batch(list, &items, now_ms).await?;
        progress(&outcome);
    }
    Ok(outcome)
}
