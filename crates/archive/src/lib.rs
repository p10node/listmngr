#![forbid(unsafe_code)]
//! MIME parsing with reviewed mail-parser 0.11.8. Indexing derives the
//! thread and the parent from the reference headers; rendering (`render`)
//! and thread order (`threading`) are pure functions the browser uses.
pub mod render;
pub mod search;
pub mod threading;
use listmngr_core::{Error, Result};
use listmngr_db::{Database, archive::ArchiveMessage, mail_queue::Lease};
/// Process one durable archive lease. No HTML or attachment is rendered inline.
/// # Errors
/// Returns invalid MIME, metadata or storage errors.
pub async fn process(db: &Database, lease: &Lease, now_ms: i64) -> Result<()> {
    process_with_clock(db, lease, now_ms, None).await
}
/// Process a production lease using post-lock wall-clock fencing.
/// # Errors
/// Returns invalid MIME, metadata or storage errors.
pub async fn process_live(db: &Database, lease: &Lease) -> Result<()> {
    process_with_clock(
        db,
        lease,
        0,
        Some(&listmngr_db::mail_queue::SystemLeaseClock),
    )
    .await
}
/// Process with an injectable completion clock, retaining explicit-time fixtures.
/// # Errors
/// Returns invalid MIME, metadata or storage errors.
pub async fn process_with_clock(
    db: &Database,
    lease: &Lease,
    now_ms: i64,
    clock: Option<&dyn listmngr_db::mail_queue::LeaseClock>,
) -> Result<()> {
    let stored = db.mail_queue().message(lease.job.message_id).await?;
    let message = mail_parser::MessageParser::default()
        .parse(&stored.raw)
        .ok_or_else(|| Error::Validation("invalid MIME".into()))?;
    let Identity {
        hash,
        thread,
        parent,
        ..
    } = identity(&message, &stored.external_id)?;
    let item = ArchiveMessage {
        hash,
        thread,
        parent,
        // Only opaque thread identity is derived here. The repository cooks
        // and indexes subject/body/sender/attachments from the safe payload
        // under its policy lock.
        ..ArchiveMessage::default()
    };
    let repo = db.archive();
    let repo = clock.map_or(repo, |clock| db.archive().with_clock(clock));
    repo.complete(lease, &item, now_ms).await
}
/// What the reference headers say about a post's place in its thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// The post's own Message-ID-Hash.
    pub hash: String,
    /// The provisional thread root: the first reference that hashes, else
    /// the post itself. The repository resolves it to an indexed root.
    pub thread: String,
    /// The post replied to: `In-Reply-To`, else the last `References`
    /// entry; never the post itself.
    pub parent: Option<String>,
    /// The `Date` header in milliseconds, when it parsed.
    pub date_ms: Option<i64>,
}

/// The thread identity of a parsed message whose Message-ID is
/// `message_id` (the stored external id, or the header when importing).
/// # Errors
/// Returns validation when the Message-ID cannot be hashed.
pub fn identity(message: &mail_parser::Message<'_>, message_id: &str) -> Result<Identity> {
    let hash =
        listmngr_mail::message_id_hash(message_id).map_err(|e| Error::Validation(e.to_string()))?;
    // Optional conversation hints must not reject an otherwise valid message.
    let thread = reference_hash(message.references())
        .or_else(|| reference_hash(message.in_reply_to()))
        .unwrap_or_else(|| hash.clone());
    let parent = reference_hash(message.in_reply_to())
        .or_else(|| last_reference_hash(message.references()))
        .filter(|parent| *parent != hash);
    Ok(Identity {
        hash,
        thread,
        parent,
        date_ms: message
            .date()
            .map(|date| date.to_timestamp().saturating_mul(1000)),
    })
}

fn reference_hash(value: &mail_parser::HeaderValue<'_>) -> Option<String> {
    value
        .as_text_list()
        .into_iter()
        .flatten()
        .find_map(|id| listmngr_mail::message_id_hash(id.as_ref()).ok())
        .or_else(|| {
            value
                .as_text()
                .and_then(|id| listmngr_mail::message_id_hash(id).ok())
        })
}

fn last_reference_hash(value: &mail_parser::HeaderValue<'_>) -> Option<String> {
    value
        .as_text_list()
        .into_iter()
        .flatten()
        .rev()
        .find_map(|id| listmngr_mail::message_id_hash(id.as_ref()).ok())
        .or_else(|| {
            value
                .as_text()
                .and_then(|id| listmngr_mail::message_id_hash(id).ok())
        })
}

/// Generate mboxrd with exact input octets except line-boundary quoting and final separator.
#[must_use]
pub fn mbox(messages: &[ArchiveMessage]) -> Vec<u8> {
    let mut out = Vec::new();
    for message in messages {
        out.extend_from_slice(b"From archive@localhost Thu Jan  1 00:00:00 1970\n");
        for line in message.raw.split_inclusive(|b| *b == b'\n') {
            if line
                .iter()
                .copied()
                .skip_while(|b| *b == b'>')
                .collect::<Vec<_>>()
                .starts_with(b"From ")
            {
                out.push(b'>');
            }
            out.extend_from_slice(line);
        }
        if !out.ends_with(b"\n") {
            out.push(b'\n');
        }
        out.push(b'\n');
    }
    out
}

/// The archived post behind a finished archive job, as the search index
/// stores it; `None` when the job archived nothing (a list whose archive
/// policy is `never`).
/// # Errors
/// Returns database errors.
pub async fn index_document(
    db: &Database,
    message_id: listmngr_db::mail_queue::MessageId,
) -> Result<Option<search::Document>> {
    Ok(db
        .archive()
        .index_row_for_message(message_id)
        .await?
        .map(document_from_row))
}

fn document_from_row(row: listmngr_db::archive::IndexRow) -> search::Document {
    search::Document {
        list: row.list,
        hash: row.hash,
        thread: row.thread,
        subject: row.subject,
        body: row.body,
        sender_name: row.sender_name,
        sender_email: row.sender_email,
        date_ms: row.date_ms,
    }
}

/// Rebuild the index from every archived post, in batches of a thousand,
/// and commit once at the end. Returns how many posts were indexed.
/// # Errors
/// Returns database or index errors.
pub async fn reindex(db: &Database, index: &search::SearchIndex) -> Result<usize> {
    let mut writer = index.writer()?;
    writer.clear()?;
    let mut after: Option<(i64, String)> = None;
    let mut count = 0;
    loop {
        let rows = db.archive().index_rows_after(after.as_ref(), 1000).await?;
        let Some(last) = rows.last() else {
            break;
        };
        after = Some((last.created_at, last.hash.clone()));
        for row in rows {
            writer.add(&document_from_row(row))?;
            count += 1;
        }
    }
    writer.commit()?;
    index.reload()?;
    Ok(count)
}
