//! Pure inbound posting policy: no I/O, no queue/DB access.
//!
//! Deliberately conservative: any enabled control this runtime does not yet
//! enforce (header-match rules, list emergency mode) holds the message for a
//! human rather than silently delivering it. See `docs/PLAN.md` §4.3 for the
//! full Mailman rule set this is a bounded subset of.
//!
//! The decision itself lives in [`crate::chain`]; this module owns the facts
//! the caller must gather, recipient selection, and header decoration.
use crate::chain::{ChainError, Outcome, builtin};
use listmngr_core::{DeliveryMode, DeliveryStatus, ModerationAction};

/// Final disposition of one inbound post, with a short, non-secret reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disposition {
    Accept,
    Hold(String),
    Reject(String),
    Discard(String),
}

/// Sender-specific facts, gathered by the `in` runner.
#[derive(Debug, Clone, Default)]
// Independent facts, not exclusive states.
#[allow(clippy::struct_excessive_bools)]
pub struct SenderChecks {
    pub is_banned: bool,
    /// The inbound `List-Post` header already names this list (a loop).
    pub is_loop: bool,
    /// The runner verified an `Approved:` posting key against the list's
    /// moderator password. Verification never happens in the pipeline.
    pub is_approved: bool,
    /// A `nonmember` role row exists for this sender, carrying its override.
    pub nonmember_action: Option<ModerationAction>,
    /// The `From` domain publishes a DMARC policy of `reject` or
    /// `quarantine` (the runner's `validate-authenticity` verdict).
    pub dmarc_policy_restrictive: bool,
}

/// One per-list `header_matches` row, or one site-wide antispam check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderMatch {
    /// Header field name, matched case-insensitively.
    pub header: String,
    /// Regular expression, searched (unanchored, case-insensitive) in each
    /// occurrence of the header.
    pub pattern: String,
    /// Chain to jump to on match; `None` means the site default (`hold`).
    pub chain: Option<String>,
    /// Tag recorded on the outcome when this row matches.
    pub tag: Option<String>,
}

/// List-configuration facts, sampled before evaluation.
#[derive(Debug, Clone, Default)]
// These controls can independently hold a message; they are not exclusive states.
#[allow(clippy::struct_excessive_bools)]
pub struct ListChecks {
    pub emergency: bool,
    /// Original headers plus body exceed the configured per-list byte limit.
    pub message_too_large: bool,
    /// Visible To/Cc count exceeds the limit, or cannot be safely parsed.
    pub too_many_recipients: bool,
    /// The list's posting address, lowercased, for `implicit-dest`.
    pub posting_address: String,
    /// `administrivia` setting: hold posts that look like email commands.
    pub administrivia: bool,
    /// `require_explicit_destination` setting.
    pub require_explicit_destination: bool,
    /// `acceptable_aliases`: exact addresses or `^`-prefixed regexes.
    pub acceptable_aliases: Vec<String>,
    /// Legacy nonmember lists: exact addresses or `^`-prefixed regexes.
    pub accept_these_nonmembers: Vec<String>,
    pub hold_these_nonmembers: Vec<String>,
    pub reject_these_nonmembers: Vec<String>,
    pub discard_these_nonmembers: Vec<String>,
    /// Per-list `header_matches` rows in position order.
    pub header_matches: Vec<HeaderMatch>,
    /// `dmarc_mitigate_action`.
    pub dmarc_action: listmngr_core::DmarcMitigateAction,
    /// `dmarc_mitigate_unconditionally`: mitigate every post.
    pub dmarc_unconditional: bool,
    /// `dmarc_addresses`: exact addresses or `^` regexes always mitigated.
    pub dmarc_addresses: Vec<String>,
    /// `dmarc_moderation_notice`: the reason a rejected post carries.
    pub dmarc_moderation_notice: String,
}

/// Message-derived facts. Extracted by the runner so the pipeline never parses
/// MIME itself.
#[derive(Debug, Clone, Default)]
pub struct MessageChecks {
    /// Unfolded header fields in order; names as written, values trimmed.
    pub headers: Vec<(String, String)>,
    /// Non-blank lines of the first text part, stopping at a `-- ` signature
    /// separator, capped at one more than [`ADMINISTRIVIA_MAX_LINES`] so the
    /// rule can tell "short" from "too long" without seeing the whole body.
    pub body_lines: Vec<String>,
    /// Visible To/Cc mailboxes, lowercased.
    pub recipients: Vec<String>,
}

/// Bodies longer than this many non-blank lines are never administrivia.
pub const ADMINISTRIVIA_MAX_LINES: usize = 10;

/// Every fact the policy needs, gathered by the caller (the `in` runner) from
/// the durable list/member/ban state and the raw message.
///
/// No network or database access happens here.
#[derive(Debug, Clone)]
pub struct PostingContext {
    /// `None` means a null reverse path (`MAIL FROM:<>`).
    pub envelope_sender: Option<String>,
    pub sender: SenderChecks,
    pub list: ListChecks,
    pub message: MessageChecks,
    /// Site-wide `[antispam] header_checks`, evaluated by `suspicious-header`.
    pub site_header_checks: Vec<HeaderMatch>,
    /// Site-wide `[antispam] jump_chain`: where a per-list header rule that
    /// names no chain sends the message.
    pub site_jump_chain: String,
    /// `Some` if the sender address matches an existing list membership
    /// (any role), carrying that member's per-member override, if any.
    pub member_moderation_action: Option<Option<ModerationAction>>,
    pub default_member_action: ModerationAction,
    pub default_nonmember_action: ModerationAction,
}

impl Default for PostingContext {
    /// A nonmember posting with no sender and site-default actions; tests and
    /// fact gatherers fill in what they know.
    fn default() -> Self {
        Self {
            envelope_sender: None,
            sender: SenderChecks::default(),
            list: ListChecks::default(),
            message: MessageChecks::default(),
            site_header_checks: Vec::new(),
            site_jump_chain: "hold".into(),
            member_moderation_action: None,
            default_member_action: ModerationAction::Defer,
            default_nonmember_action: ModerationAction::Hold,
        }
    }
}

impl PostingContext {
    /// First occurrence of a header, matched case-insensitively.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.message
            .headers
            .iter()
            .find(|(field, _)| field.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Every occurrence of a header, matched case-insensitively, in order.
    pub fn headers(&self, name: &str) -> impl Iterator<Item = &str> + '_ {
        let name = name.to_owned();
        self.message
            .headers
            .iter()
            .filter(move |(field, _)| field.eq_ignore_ascii_case(&name))
            .map(|(_, value)| value.as_str())
    }
}

/// The chain entry point for ordinary list postings.
pub const POSTING_CHAIN: &str = "default-posting-chain";

/// Decide one inbound post by running the built-in `default-posting-chain`.
///
/// A mis-wired chain cannot produce an accept: any [`ChainError`] fails closed
/// to a hold, matching this module's conservative posture.
///
/// # Panics
/// Never panics; total over `PostingContext`.
#[must_use]
pub fn decide_posting(ctx: &PostingContext) -> Disposition {
    decide_posting_traced(ctx).disposition
}

/// Decide one inbound post and keep the rule trace, for the
/// `X-Mailman-Rule-Hits` / `X-Mailman-Rule-Misses` headers and for audit.
///
/// # Panics
/// Never panics; total over `PostingContext`.
#[must_use]
pub fn decide_posting_traced(ctx: &PostingContext) -> Outcome {
    match builtin().run(POSTING_CHAIN, ctx) {
        Ok(outcome) => outcome,
        Err(error) => fail_closed(&error),
    }
}

/// Turn an engine misconfiguration into a hold. The reason names the fault
/// without echoing message content.
fn fail_closed(error: &ChainError) -> Outcome {
    Outcome {
        disposition: Disposition::Hold(format!("posting chain misconfigured: {error}")),
        hits: Vec::new(),
        misses: Vec::new(),
        effects: Vec::new(),
        tags: Vec::new(),
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
        .filter(|candidate| {
            candidate.receive_own_postings || !candidate.email.eq_ignore_ascii_case(sender_email)
        })
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
        ("List-Id".to_owned(), format!("<{}>", info.list_id)),
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
