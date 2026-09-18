//! Gathers real list/member/ban facts from the database for the pure
//! `listmngr_pipeline::policy` decision, and resolves accepted-post recipients.
use listmngr_core::{
    Config, DeliveryMode, DeliveryStatus, ListId, MemberRole, ModerationAction, Result,
};
use listmngr_db::Database;
use listmngr_pipeline::{
    ADMINISTRIVIA_MAX_LINES, CandidateRecipient, HeaderMatch, ListChecks, MessageChecks,
    PostingContext, SenderChecks, select_recipients,
};

pub use listmngr_mail::facts::loop_markers;

async fn is_banned(db: &Database, list_id: &ListId, sender: &str) -> Result<bool> {
    db.bans().is_banned(list_id, sender).await
}

/// The list's own header rules, in position order, in the pipeline's shape.
async fn header_matches(db: &Database, list_id: &ListId) -> Result<Vec<HeaderMatch>> {
    Ok(db
        .header_matches()
        .list(list_id)
        .await?
        .into_iter()
        .map(|row| HeaderMatch {
            header: row.header,
            pattern: row.pattern,
            chain: row.chain,
            tag: row.tag,
        })
        .collect())
}

/// Site-wide `[antispam] header_checks`, which never name a chain of their own.
fn site_header_checks(config: &Config) -> Vec<HeaderMatch> {
    config
        .antispam
        .header_checks
        .iter()
        .map(|check| HeaderMatch {
            header: check.header.trim().to_owned(),
            pattern: check.pattern.clone(),
            chain: None,
            tag: None,
        })
        .collect()
}

/// A post written on the web by a signed-in member from a verified
/// address, as the web layer recorded it in the job's context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebPost {
    pub user_id: String,
    pub address: String,
}

impl WebPost {
    /// The `web_post` object of a queued submission's context, if any.
    #[must_use]
    pub fn from_context(context: &serde_json::Value) -> Option<Self> {
        let web = context.get("web_post")?;
        Some(Self {
            user_id: web.get("user_id")?.as_str()?.to_owned(),
            address: web.get("address")?.as_str()?.to_owned(),
        })
    }
}

/// A web post is admitted the way an `Approved:` post is — past the
/// deferred checks (destination, size, recipients, subject, header rules)
/// and emergency moderation — when its envelope sender is the recorded
/// verified address, the address is not banned, and the member would post
/// unmoderated anyway (their own action, or the list's default, is `defer`
/// or `accept`). A moderated member's web post is held like their mail.
/// Returns whether the post was approved.
pub fn approve_web_post(ctx: &mut PostingContext, web: Option<&WebPost>) -> bool {
    let Some(web) = web else {
        return false;
    };
    let same_sender = ctx
        .envelope_sender
        .as_deref()
        .is_some_and(|sender| sender.eq_ignore_ascii_case(&web.address));
    if !same_sender || ctx.sender.is_banned {
        return false;
    }
    let effective = ctx
        .member_moderation_action
        .map(|own| own.unwrap_or(ctx.default_member_action));
    if matches!(
        effective,
        Some(ModerationAction::Defer | ModerationAction::Accept)
    ) {
        ctx.sender.is_approved = true;
        return true;
    }
    false
}

/// Verify an `Approved:` posting key against the list's moderator password.
/// A message carrying no key, or a list with no password, is simply not
/// approved; the key itself never leaves this function.
async fn is_approved(db: &Database, list_id: &ListId, raw: &[u8]) -> Result<bool> {
    match listmngr_mail::facts::approved_key(raw) {
        Some(key) => db.lists().verify_moderator_password(list_id, &key).await,
        None => Ok(false),
    }
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
    let is_loop = loop_markers(raw)
        .iter()
        .any(|address| address.eq_ignore_ascii_case(&list_id.posting_address()));
    let mut member_moderation_action = None;
    let mut nonmember_action = None;
    let mut is_banned_sender = false;
    if let Some(sender) = envelope_sender {
        is_banned_sender = is_banned(db, list_id, sender).await?;
        let members = db.members().find(sender).await?;
        if let Some(member) = members
            .iter()
            .find(|member| member.list_id == *list_id && member.role == MemberRole::Member)
        {
            member_moderation_action = Some(member.moderation_action);
        } else if members.iter().any(|member| {
            member.list_id == *list_id
                && matches!(member.role, MemberRole::Owner | MemberRole::Moderator)
        }) {
            // Owners and moderators post as explicitly accepted members. As in
            // Mailman, an explicit accept bypasses the deferred checks (size,
            // recipients, subject, destination, header rules); emergency
            // moderation and bans still apply.
            member_moderation_action = Some(Some(ModerationAction::Accept));
        } else if let Some(nonmember) = members
            .iter()
            .find(|member| member.list_id == *list_id && member.role == MemberRole::Nonmember)
        {
            nonmember_action = nonmember.moderation_action;
        }
    }
    let message = MessageChecks {
        headers: listmngr_mail::facts::header_fields(raw),
        body_lines: listmngr_mail::facts::body_preview_lines(raw, ADMINISTRIVIA_MAX_LINES + 1),
        recipients: listmngr_mail::visible_recipients::mailboxes(raw)
            .unwrap_or_default()
            .into_iter()
            .map(|address| address.to_ascii_lowercase())
            .collect(),
    };
    Ok(PostingContext {
        envelope_sender: envelope_sender.map(str::to_owned),
        sender: SenderChecks {
            is_banned: is_banned_sender,
            is_loop,
            is_approved: is_approved(db, list_id, raw).await?,
            nonmember_action,
            // Filled by the runner from the `validate-authenticity` verdict.
            dmarc_policy_restrictive: false,
        },
        list: ListChecks {
            emergency: list.emergency,
            message_too_large: list.max_message_size != 0
                && u64::try_from(raw.len()).unwrap_or(u64::MAX)
                    > u64::from(list.max_message_size) * 1024,
            too_many_recipients: list.max_num_recipients != 0
                && crate::visible_recipients::count(raw)
                    .is_none_or(|count| count >= u64::from(list.max_num_recipients)),
            posting_address: list_id.posting_address().to_ascii_lowercase(),
            administrivia: list.administrivia,
            require_explicit_destination: list.require_explicit_destination,
            acceptable_aliases: list.acceptable_aliases.clone(),
            accept_these_nonmembers: list.accept_these_nonmembers.clone(),
            hold_these_nonmembers: list.hold_these_nonmembers.clone(),
            reject_these_nonmembers: list.reject_these_nonmembers.clone(),
            discard_these_nonmembers: list.discard_these_nonmembers.clone(),
            header_matches: header_matches(db, list_id).await?,
            dmarc_action: list.dmarc.action,
            dmarc_unconditional: list.dmarc.unconditional,
            dmarc_addresses: list.dmarc.dmarc_addresses.clone(),
            dmarc_moderation_notice: list.dmarc.dmarc_moderation_notice.clone(),
        },
        message,
        site_header_checks: site_header_checks(config),
        site_jump_chain: config.antispam.jump_chain.clone(),
        member_moderation_action,
        default_member_action: list
            .default_member_action
            .unwrap_or(config.mailman.default_member_action),
        default_nonmember_action: list
            .default_nonmember_action
            .unwrap_or(config.mailman.default_nonmember_action),
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
    resolve_recipients_for_message(db, list_id, sender_email, b"\r\n").await
}

/// Resolve regular delivery using the original, uncooked To/Cc headers.
/// # Errors
/// Returns database errors.
pub async fn resolve_recipients_for_message(
    db: &Database,
    list_id: &ListId,
    sender_email: &str,
    raw: &[u8],
) -> Result<Vec<String>> {
    let direct = listmngr_mail::visible_recipients::mailboxes(raw).unwrap_or_default();
    let members = db.members().roster(list_id, MemberRole::Member).await?;
    let mut candidates = Vec::with_capacity(members.len());
    for member in &members {
        let address = db.addresses().get_by_id(member.address_id).await?;
        let preferences = db
            .preferences()
            .resolve_member(member.id, db.default_language())
            .await?;
        if !preferences.receive_list_copy.unwrap_or(true) && direct.contains(&address.email) {
            continue;
        }
        candidates.push(CandidateRecipient {
            email: address.original_email,
            delivery_status: preferences
                .delivery_status
                .unwrap_or(DeliveryStatus::Enabled),
            delivery_mode: preferences.delivery_mode.unwrap_or(DeliveryMode::Regular),
            receive_own_postings: preferences.receive_own_postings.unwrap_or(true),
        });
    }
    Ok(select_recipients(&candidates, sender_email))
}

#[cfg(test)]
mod tests {
    use super::loop_markers;

    #[tokio::test]
    async fn exact_bans_use_mailbox_identity_but_regex_keeps_original_sender() {
        let db = listmngr_db::Database::connect("sqlite::memory:", 1)
            .await
            .unwrap();
        db.migrate().await.unwrap();
        db.domains().create("example.net", "", None).await.unwrap();
        let list = db
            .lists()
            .create(listmngr_db::NewList {
                list_id: "team.example.net".parse().unwrap(),
                display_name: "Team".into(),
                style: "legacy-default".into(),
            })
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO bans(id,list_id,email_or_regex) VALUES('exact',$1,'author@example.net')",
        )
        .bind(list.id.as_str())
        .execute(db.pool())
        .await
        .unwrap();
        assert!(
            super::is_banned(&db, &list.id, "AUTHOR@EXAMPLE.NET.")
                .await
                .unwrap()
        );
        assert!(
            !super::is_banned(&db, &list.id, "other@example.net")
                .await
                .unwrap()
        );
        sqlx::query("DELETE FROM bans")
            .execute(db.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO bans(id,list_id,email_or_regex) VALUES('regex',NULL,'^Author@outside[.]net$')")
            .execute(db.pool()).await.unwrap();
        assert!(
            super::is_banned(&db, &list.id, "Author@outside.net")
                .await
                .unwrap()
        );
        assert!(
            !super::is_banned(&db, &list.id, "author@outside.net")
                .await
                .unwrap()
        );
    }

    #[test]
    fn mailto_markers_decode_uri_escapes_without_matching_body_or_query_addresses() {
        let raw = b"List-Post: <mailto:t%65st%40example.invalid?subject=post>, <mailto:notest@example.invalid>\nList-Post: <https://example.invalid/test@example.invalid>\nList-Post: <mailto:other@example.invalid?cc=test@example.invalid>\nX-BeenThere:\n\told@example.invalid\nX-BeenThere: older@example.invalid\n\nList-Post: <mailto:body@example.invalid>\n";
        assert_eq!(
            loop_markers(raw),
            [
                "test@example.invalid",
                "notest@example.invalid",
                "other@example.invalid",
                "old@example.invalid",
                "older@example.invalid"
            ]
        );
        assert!(loop_markers(b"List-Post: <mailto:test%0a@example.invalid>, <mailto:test%ZZ@example.invalid>\n\n").is_empty());
    }
}
