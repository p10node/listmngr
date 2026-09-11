#![forbid(unsafe_code)]
//! MIME parsing with reviewed mail-parser 0.11.8. Rendered bodies are always text.
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
    let hash = listmngr_mail::message_id_hash(&stored.external_id)
        .map_err(|e| Error::Validation(e.to_string()))?;
    // Optional conversation hints must not reject an otherwise valid message.
    let thread = reference_hash(message.references())
        .or_else(|| reference_hash(message.in_reply_to()))
        .unwrap_or_else(|| hash.clone());
    let item = ArchiveMessage {
        hash,
        thread,
        // Only opaque thread identity is derived here. The repository cooks
        // and indexes subject/body from the safe payload under its policy lock.
        subject: String::new(),
        body: String::new(),
        raw: vec![],
    };
    let repo = db.archive();
    let repo = clock.map_or(repo, |clock| db.archive().with_clock(clock));
    repo.complete(lease, &item, now_ms).await
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
