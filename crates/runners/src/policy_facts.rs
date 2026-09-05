//! Gathers real list/member/ban facts from the database for the pure
//! `listmngr_pipeline::policy` decision, and resolves accepted-post recipients.
use listmngr_core::{Config, DeliveryMode, DeliveryStatus, Error, ListId, MemberRole, Result};
use listmngr_db::Database;
use listmngr_pipeline::{
    CandidateRecipient, ListChecks, PostingContext, SenderChecks, select_recipients,
};

async fn is_banned(db: &Database, list_id: &ListId, sender: &str) -> Result<bool> {
    let patterns: Vec<String> =
        sqlx::query_scalar("SELECT email_or_regex FROM bans WHERE list_id=$1 OR list_id IS NULL")
            .bind(list_id.as_str())
            .fetch_all(db.pool())
            .await
            .map_err(|error| Error::Database(error.to_string()))?;
    let sender_lower = sender.to_ascii_lowercase();
    Ok(patterns.iter().any(|pattern| {
        pattern.strip_prefix('^').map_or_else(
            || pattern.to_ascii_lowercase() == sender_lower,
            |body| {
                regex::Regex::new(&format!("^{body}"))
                    .is_ok_and(|expression| expression.is_match(sender))
            },
        )
    }))
}

async fn header_matches_exist(db: &Database, list_id: &ListId) -> Result<bool> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM header_matches WHERE list_id=$1")
        .bind(list_id.as_str())
        .fetch_one(db.pool())
        .await
        .map_err(|error| Error::Database(error.to_string()))?;
    Ok(count > 0)
}

/// Gather every fact `listmngr_pipeline::policy::decide_posting` needs for
/// one inbound submission. No decision logic lives here.
/// # Errors
/// Returns a database error, or not-found if the list no longer exists.
pub async fn gather_context(
    db: &Database,
    config: &Config,
    list_id: &ListId,
    envelope_sender: Option<&str>,
    raw: &[u8],
) -> Result<PostingContext> {
    let list = db.lists().get(list_id).await?;
    let is_loop = listmngr_mail::header_value(raw, "list-post").is_some_and(|value| {
        value
            .to_ascii_lowercase()
            .contains(&list_id.posting_address().to_ascii_lowercase())
    });
    let mut member_moderation_action = None;
    let mut is_banned_sender = false;
    if let Some(sender) = envelope_sender {
        is_banned_sender = is_banned(db, list_id, sender).await?;
        let members = db.members().find(sender).await?;
        if let Some(member) = members
            .into_iter()
            .find(|member| member.list_id == *list_id)
        {
            member_moderation_action = Some(member.moderation_action);
        }
    }
    Ok(PostingContext {
        envelope_sender: envelope_sender.map(str::to_owned),
        sender: SenderChecks {
            is_banned: is_banned_sender,
            is_loop,
        },
        list: ListChecks {
            emergency: list.emergency,
            has_unsupported_header_matches: header_matches_exist(db, list_id).await?,
        },
        member_moderation_action,
        default_member_action: config.mailman.default_member_action,
        default_nonmember_action: config.mailman.default_nonmember_action,
    })
}

/// Enabled, regular-delivery members' addresses for an accepted post,
/// honoring `receive_own_postings`. Digest/summary members are excluded (see
/// `listmngr_pipeline::policy::select_recipients`).
/// # Errors
/// Returns a database error.
pub async fn resolve_recipients(
    db: &Database,
    list_id: &ListId,
    sender_email: &str,
) -> Result<Vec<String>> {
    let members = db.members().roster(list_id, MemberRole::Member).await?;
    let mut candidates = Vec::with_capacity(members.len());
    for member in &members {
        let address = db.addresses().get_by_id(member.address_id).await?;
        let preferences = db.preferences().resolve_member(member.id, "en").await?;
        candidates.push(CandidateRecipient {
            email: address.email,
            delivery_status: preferences
                .delivery_status
                .unwrap_or(DeliveryStatus::Enabled),
            delivery_mode: preferences.delivery_mode.unwrap_or(DeliveryMode::Regular),
            receive_own_postings: preferences.receive_own_postings.unwrap_or(true),
        });
    }
    Ok(select_recipients(&candidates, sender_email))
}
