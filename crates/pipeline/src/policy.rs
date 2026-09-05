//! Pure inbound posting policy: no I/O, no queue/DB access.
//!
//! Deliberately conservative: any enabled control this runtime does not yet
//! enforce (header-match rules, list emergency mode) holds the message for a
//! human rather than silently delivering it. See `docs/PLAN.md` §4.3 for the
//! full Mailman rule set this is a bounded subset of.
use listmngr_core::{DeliveryMode, DeliveryStatus, ModerationAction};

/// Final disposition of one inbound post, with a short, non-secret reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disposition {
    Accept,
    Hold(String),
    Reject(String),
    Discard(String),
}

/// Sender-specific pre-checks, evaluated before any membership-based moderation.
#[derive(Debug, Clone, Copy, Default)]
pub struct SenderChecks {
    pub is_banned: bool,
    /// The inbound `List-Post` header already names this list (a loop).
    pub is_loop: bool,
}

/// List-configuration facts that force a hold when this runtime does not yet
/// enforce a control the operator has enabled.
#[derive(Debug, Clone, Copy, Default)]
pub struct ListChecks {
    pub emergency: bool,
    /// The list has `header_matches` rows configured, which this runtime does
    /// not yet enforce; fail closed rather than silently ignore them.
    pub has_unsupported_header_matches: bool,
}

/// Every fact the policy needs, gathered by the caller (the `in` runner) from
/// the durable list/member/ban state.
///
/// No network or database access happens here.
#[derive(Debug, Clone)]
pub struct PostingContext {
    /// `None` means a null reverse path (`MAIL FROM:<>`).
    pub envelope_sender: Option<String>,
    pub sender: SenderChecks,
    pub list: ListChecks,
    /// `Some` if the sender address matches an existing list membership
    /// (any role), carrying that member's per-member override, if any.
    pub member_moderation_action: Option<Option<ModerationAction>>,
    pub default_member_action: ModerationAction,
    pub default_nonmember_action: ModerationAction,
}

/// # Panics
/// Never panics; total over `PostingContext`.
#[must_use]
pub fn decide_posting(ctx: &PostingContext) -> Disposition {
    let Some(sender) = ctx.envelope_sender.as_deref() else {
        return Disposition::Discard("null reverse path".into());
    };
    if ctx.sender.is_banned {
        return Disposition::Reject(format!("{sender} is banned from this list"));
    }
    if ctx.sender.is_loop {
        return Disposition::Discard("List-Post header names this list (loop)".into());
    }
    if ctx.list.emergency {
        return Disposition::Hold("list is in emergency moderation mode".into());
    }
    if ctx.list.has_unsupported_header_matches {
        return Disposition::Hold(
            "list has header-match rules not yet enforced by this runtime".into(),
        );
    }
    let action = match ctx.member_moderation_action {
        Some(Some(action)) => action,
        Some(None) => ctx.default_member_action,
        None => ctx.default_nonmember_action,
    };
    match action {
        ModerationAction::Defer | ModerationAction::Accept => Disposition::Accept,
        ModerationAction::Hold => Disposition::Hold("moderation policy".into()),
        ModerationAction::Reject => Disposition::Reject("moderation policy".into()),
        ModerationAction::Discard => Disposition::Discard("moderation policy".into()),
    }
}

/// One roster member's delivery-relevant preferences, already layer-resolved.
#[derive(Debug, Clone)]
pub struct CandidateRecipient {
    pub email: String,
    pub delivery_status: DeliveryStatus,
    pub delivery_mode: DeliveryMode,
    pub receive_own_postings: bool,
}

/// Enabled, regular-delivery members, excluding the sender if they disabled
/// receiving a copy of their own postings.
///
/// Digest/summary members are intentionally excluded: this runtime does not
/// yet build digests, and delivering a digest member an unformatted regular
/// copy would misrepresent their chosen delivery mode.
#[must_use]
pub fn select_recipients(candidates: &[CandidateRecipient], sender_email: &str) -> Vec<String> {
    candidates
        .iter()
        .filter(|candidate| candidate.delivery_status == DeliveryStatus::Enabled)
        .filter(|candidate| candidate.delivery_mode == DeliveryMode::Regular)
        .filter(|candidate| candidate.receive_own_postings || candidate.email != sender_email)
        .map(|candidate| candidate.email.clone())
        .collect()
}

/// RFC 2369 `List-*` headers and `Precedence: list`, honest about what this
/// runtime actually advertises: no `List-Archive` unless the list is publicly archived.
#[derive(Debug, Clone)]
pub struct ListHeaderInfo {
    pub list_id: String,
    pub posting_address: String,
    pub subscribe_address: String,
    pub unsubscribe_address: String,
    pub archive_url: Option<String>,
}

#[must_use]
pub fn list_headers(info: &ListHeaderInfo) -> Vec<(String, String)> {
    let mut headers = vec![
        ("List-Id".to_owned(), info.list_id.clone()),
        (
            "List-Post".to_owned(),
            format!("<mailto:{}>", info.posting_address),
        ),
        (
            "List-Subscribe".to_owned(),
            format!("<mailto:{}>", info.subscribe_address),
        ),
        (
            "List-Unsubscribe".to_owned(),
            format!("<mailto:{}>", info.unsubscribe_address),
        ),
        ("Precedence".to_owned(), "list".to_owned()),
    ];
    if let Some(archive) = &info.archive_url {
        headers.push(("List-Archive".to_owned(), format!("<{archive}>")));
    }
    headers
}
