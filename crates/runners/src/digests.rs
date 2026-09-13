//! Digest collector and periodic/size-triggered publisher. SMTP is exclusively
//! handled by the existing fenced outgoing queue processor.
use listmngr_core::{DeliveryMode, DeliveryStatus, Error, ListId, MemberRole, Result};
use listmngr_db::{
    Database,
    digests::{DigestIssue, DigestOutput, DigestRecipient},
    mail_queue::{Lease, Queue},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};
use tokio::sync::watch;

async fn collect(db: &Database, lease: &Lease) -> Result<()> {
    let message = db.mail_queue().live().message(lease.job.message_id).await?;
    let context: serde_json::Value =
        serde_json::from_str(&message.context).map_err(|e| Error::Validation(e.to_string()))?;
    let list: ListId = context["list_id"]
        .as_str()
        .ok_or_else(|| Error::Validation("digest missing list".into()))?
        .parse()?;
    let sender = context["envelope_sender"].as_str().unwrap_or("");
    let direct = listmngr_mail::visible_recipients::mailboxes(&message.raw).unwrap_or_default();
    let mut recipients = Vec::new();
    for member in db.members().roster(&list, MemberRole::Member).await? {
        let p = db
            .preferences()
            .resolve_member(member.id, db.default_language())
            .await?;
        let mode = p.delivery_mode.unwrap_or(DeliveryMode::Regular);
        if mode == DeliveryMode::Regular
            || p.delivery_status.unwrap_or(DeliveryStatus::Enabled) != DeliveryStatus::Enabled
        {
            continue;
        }
        let address = db.addresses().get_by_id(member.address_id).await?;
        if (!p.receive_own_postings.unwrap_or(true) && address.email.eq_ignore_ascii_case(sender))
            || (!p.receive_list_copy.unwrap_or(true) && direct.contains(&address.email))
        {
            continue;
        }
        recipients.push(DigestRecipient {
            email: address.original_email,
            mode: mode.to_string(),
        });
    }
    let (raw, _) =
        crate::outbound::prepare(db, &message.raw, &message.context, lease.job.message_id.0)
            .await
            .map_err(|e| match e {
                crate::outbound::PrepareError::Invalid => {
                    Error::Validation("invalid digest headers".into())
                }
                crate::outbound::PrepareError::Dependency => {
                    Error::Database("digest list lookup failed".into())
                }
            })?;
    db.digests()
        .live()
        .collect(
            lease,
            &list,
            &raw,
            &recipients,
            chrono::Utc::now().timestamp_millis(),
        )
        .await
}
fn render(issue: &DigestIssue) -> Result<Vec<DigestOutput>> {
    // Per-post exclusions must not accidentally reappear in a shared digest.
    let mut membership: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
    for (index, post) in issue.posts.iter().enumerate() {
        for recipient in &post.recipients {
            membership
                .entry((recipient.mode.clone(), recipient.email.clone()))
                .or_default()
                .push(index);
        }
    }
    let mut groups: BTreeMap<(String, Vec<usize>), Vec<String>> = BTreeMap::new();
    for ((mode, email), indices) in membership {
        groups.entry((mode, indices)).or_default().push(email);
    }
    groups
        .into_iter()
        .map(|((mode, indices), recipients)| {
            let messages: Vec<&[u8]> = indices
                .iter()
                .map(|i| issue.posts[*i].raw.as_slice())
                .collect();
            let history: BTreeSet<_> = messages
                .iter()
                .flat_map(|raw| crate::policy_facts::loop_markers(raw))
                .collect();
            let raw = listmngr_mail::digest::build(&listmngr_mail::digest::Digest {
                list: issue.list.clone(),
                display_name: issue.display_name.clone(),
                volume: issue.volume,
                number: issue.number,
                mode: mode.parse()?,
                timestamp: issue.timestamp,
                masthead: issue.masthead.clone(),
                header: issue.header.clone(),
                footer: issue.footer.clone(),
                messages,
            })
            .map_err(|e| Error::Validation(e.to_string()))?;
            let mut headers = crate::digests::headers(&issue.list);
            headers.push((
                "Message-ID".into(),
                format!("<{}@{}>", uuid::Uuid::now_v7(), issue.list.mail_host()),
            ));
            headers.extend(history.into_iter().map(|v| ("X-BeenThere".into(), v)));
            let raw = listmngr_mail::cook_headers(&raw, None, &headers)
                .map_err(|e| Error::Validation(e.to_string()))?;
            Ok(DigestOutput {
                raw,
                recipients,
                mode,
            })
        })
        .collect()
}
fn headers(list: &ListId) -> Vec<(String, String)> {
    listmngr_pipeline::list_headers(&listmngr_pipeline::ListHeaderInfo {
        list_id: list.to_string(),
        posting_address: list.posting_address(),
        subscribe_address: list.join_address(),
        unsubscribe_address: list.leave_address(),
        archive_url: None,
    })
}
/// Force one issue, or publish only if periodic/size threshold is due.
/// # Errors
/// Returns collection, rendering, lease or database errors.
pub async fn send(db: &Database, list: &ListId, force: bool) -> Result<usize> {
    db.digests()
        .flush(list, chrono::Utc::now().timestamp_millis(), force, render)
        .await
}
/// Collect at most 100 ready jobs and check all lists for due issues.
/// # Errors
/// Returns collection, rendering, lease or database errors.
pub async fn tick(db: &Database, worker: &str, force: bool) -> Result<usize> {
    for _ in 0..100 {
        let Some(lease) = db
            .mail_queue()
            .live()
            .claim(
                Queue::Digest,
                worker,
                chrono::Utc::now().timestamp_millis(),
                30_000,
            )
            .await?
        else {
            break;
        };
        let result =
            crate::heartbeat::run_while_renewing(db, &lease, 30_000, collect(db, &lease)).await;
        if let crate::heartbeat::Outcome::Completed(Err(error)) = result {
            let now = chrono::Utc::now().timestamp_millis();
            if matches!(error, Error::Validation(_)) {
                db.mail_queue()
                    .live()
                    .shunt(&lease, now, &error.to_string())
                    .await?;
            } else {
                db.mail_queue()
                    .live()
                    .retry(&lease, now, 5000, &error.to_string())
                    .await?;
            }
            return Err(error);
        }
    }
    let mut count = 0;
    for list in db.lists().list(None).await? {
        count += send(db, &list.id, force).await?;
    }
    Ok(count)
}
/// Periodic runner, included in the enabled mail role's supervised lifecycle.
pub async fn run(db: Database, mut shutdown: watch::Receiver<bool>) {
    loop {
        if *shutdown.borrow() {
            return;
        }
        if let Err(error) = tick(&db, "digest-0", false).await {
            tracing::error!(%error,"digest tick failed; durable work retained");
        }
        tokio::select! { ()=tokio::time::sleep(Duration::from_secs(1))=>{}, _=shutdown.changed()=>{} }
    }
}
