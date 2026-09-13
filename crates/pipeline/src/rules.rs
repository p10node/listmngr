//! The posting rules this runtime enforces.
//!
//! Names and semantics follow Mailman 3 (`docs/PLAN.md` §4.3). Rules are pure
//! predicates over [`PostingContext`] and the evaluation state; every fact
//! they read is gathered by the `in` runner before evaluation. The reason a
//! rule returns is shown to moderators and can reach a DSN, so it must stay
//! short and free of secrets. Glue rules hit with an empty reason.
use crate::chain::{EvalState, HeaderRuleHit, Rule, evaluate_header_rules};
use crate::policy::{ADMINISTRIVIA_MAX_LINES, PostingContext};
use listmngr_core::ModerationAction;

/// Hit with no reason to record.
const fn silent() -> String {
    String::new()
}

/// Exact address or `^`-anchored regular expression, as Mailman's legacy
/// address lists allow. Exact matches are case-insensitive; regexes are
/// compiled case-insensitively and must anchor themselves.
fn address_list_matches(entries: &[String], address: &str) -> bool {
    entries.iter().any(|entry| {
        entry.strip_prefix('^').map_or_else(
            || entry.eq_ignore_ascii_case(address),
            |pattern| {
                regex::RegexBuilder::new(&format!("^{pattern}"))
                    .case_insensitive(true)
                    .size_limit(1 << 20)
                    .build()
                    .is_ok_and(|regex| regex.is_match(address))
            },
        )
    })
}

/// No usable sender at all.
///
/// Stricter than Mailman on purpose: a null reverse path (`MAIL FROM:<>`)
/// is discarded even when a `From:` is present, because bounces and other
/// null-sender traffic must never be redistributed or answered with a notice
/// to a possibly forged author. A non-null envelope with no `From`, `Sender`
/// or `Reply-To` is also unusable for cooking and is discarded too.
#[derive(Debug)]
pub struct NoSenders;

impl Rule for NoSenders {
    fn name(&self) -> &'static str {
        "no-senders"
    }
    fn check(&self, ctx: &PostingContext, _state: &mut EvalState) -> Option<String> {
        if ctx.envelope_sender.is_none() {
            return Some("null reverse path".to_owned());
        }
        let has_header_sender = ["From", "Sender", "Reply-To"].iter().any(|name| {
            ctx.header(name)
                .is_some_and(|value| !value.trim().is_empty())
        });
        (!has_header_sender).then(|| "The message has no valid senders".to_owned())
    }
}

/// The runner verified an `Approved:` posting key, so the message bypasses
/// moderation. Stripping the key from the message is the cook stage's job.
#[derive(Debug)]
pub struct Approved;

impl Rule for Approved {
    fn name(&self) -> &'static str {
        "approved"
    }
    fn check(&self, ctx: &PostingContext, _state: &mut EvalState) -> Option<String> {
        ctx.sender.is_approved.then(silent)
    }
}

/// Mailman's default reason for a post refused by DMARC mitigation.
pub const DMARC_REJECT_REASON: &str = "You are not allowed to post to this mailing list From: a domain which publishes a DMARC policy of reject or quarantine, and your message has been automatically rejected.  If you think that your messages are being rejected in error, contact the mailing list owner at $listowner.";

/// The `dmarc` tag: the post needs the list's DMARC mitigation.
pub const DMARC_TAG: &str = "dmarc";

/// Mailman's `dmarc-mitigation`.
///
/// When the list mitigates and the `From` domain's policy (or
/// `dmarc_mitigate_unconditionally`, or `dmarc_addresses`) applies, tag the
/// post for the `dmarc` handler; hit — and so jump to the
/// `dmarc-mitigation` chain — only when the action is `reject` or
/// `discard`.
#[derive(Debug)]
pub struct DmarcMitigation;

impl Rule for DmarcMitigation {
    fn name(&self) -> &'static str {
        "dmarc-mitigation"
    }
    fn check(&self, ctx: &PostingContext, state: &mut EvalState) -> Option<String> {
        use listmngr_core::DmarcMitigateAction as Action;
        if ctx.list.dmarc_action == Action::NoMitigation {
            return None;
        }
        let from = ctx.header("from").map(from_address).unwrap_or_default();
        let applies = ctx.list.dmarc_unconditional
            || ctx.sender.dmarc_policy_restrictive
            || (!from.is_empty() && address_list_matches(&ctx.list.dmarc_addresses, &from));
        if !applies {
            return None;
        }
        if !state.tags.iter().any(|tag| tag == DMARC_TAG) {
            state.tags.push(DMARC_TAG.to_owned());
        }
        match ctx.list.dmarc_action {
            Action::Reject | Action::Discard => {
                Some(if ctx.list.dmarc_moderation_notice.trim().is_empty() {
                    DMARC_REJECT_REASON.to_owned()
                } else {
                    ctx.list.dmarc_moderation_notice.clone()
                })
            }
            Action::NoMitigation | Action::MungeFrom => None,
        }
    }
}

/// The address inside a `From` header value: the last `<…>` group, else
/// the value itself, trimmed.
fn from_address(value: &str) -> String {
    let inner = value
        .rsplit_once('<')
        .and_then(|(_, rest)| rest.split_once('>'))
        .map_or(value, |(address, _)| address);
    inner.trim().to_owned()
}

/// The sender is banned from this list, globally or per list.
#[derive(Debug)]
pub struct BannedAddress;

impl Rule for BannedAddress {
    fn name(&self) -> &'static str {
        "banned-address"
    }
    fn check(&self, ctx: &PostingContext, _state: &mut EvalState) -> Option<String> {
        if !ctx.sender.is_banned {
            return None;
        }
        // `no-senders` precedes this link, so a banned check always has a sender.
        let sender = ctx.envelope_sender.as_deref().unwrap_or_default();
        Some(format!("{sender} is banned from this list"))
    }
}

/// The inbound message already carries this list's posting address in a
/// `List-Post` (or equivalent) marker, so redistributing it would loop.
#[derive(Debug)]
pub struct Loop;

impl Rule for Loop {
    fn name(&self) -> &'static str {
        "loop"
    }
    fn check(&self, ctx: &PostingContext, _state: &mut EvalState) -> Option<String> {
        ctx.sender
            .is_loop
            .then(|| "List-Post header names this list (loop)".to_owned())
    }
}

/// The list is in emergency moderation: everything is held.
#[derive(Debug)]
pub struct Emergency;

impl Rule for Emergency {
    fn name(&self) -> &'static str {
        "emergency"
    }
    fn check(&self, ctx: &PostingContext, _state: &mut EvalState) -> Option<String> {
        ctx.list
            .emergency
            .then(|| "list is in emergency moderation mode".to_owned())
    }
}

/// The original headers plus body exceed the per-list byte limit.
#[derive(Debug)]
pub struct MaxSize;

impl Rule for MaxSize {
    fn name(&self) -> &'static str {
        "max-size"
    }
    fn check(&self, ctx: &PostingContext, _state: &mut EvalState) -> Option<String> {
        ctx.list
            .message_too_large
            .then(|| "message exceeds list max_message_size".to_owned())
    }
}

/// Too many visible To/Cc recipients, or a To/Cc this runtime cannot parse
/// safely (which fails closed rather than under-counting).
#[derive(Debug)]
pub struct MaxRecipients;

impl Rule for MaxRecipients {
    fn name(&self) -> &'static str {
        "max-recipients"
    }
    fn check(&self, ctx: &PostingContext, _state: &mut EvalState) -> Option<String> {
        ctx.list
            .too_many_recipients
            .then(|| "message exceeds list max_num_recipients or has malformed To/Cc".to_owned())
    }
}

/// Subject is absent or blank.
#[derive(Debug)]
pub struct NoSubject;

impl Rule for NoSubject {
    fn name(&self) -> &'static str {
        "no-subject"
    }
    fn check(&self, ctx: &PostingContext, _state: &mut EvalState) -> Option<String> {
        ctx.header("Subject")
            .is_none_or(|subject| subject.trim().is_empty())
            .then(|| "Message has no subject".to_owned())
    }
}

/// Email commands, with the minimum and maximum argument counts that make a
/// short message look like a mis-addressed command rather than a post.
const EMAIL_COMMANDS: [(&str, usize, usize); 11] = [
    ("confirm", 1, 1),
    ("help", 0, 0),
    ("info", 0, 0),
    ("lists", 0, 0),
    ("options", 0, 0),
    ("password", 2, 2),
    ("remove", 0, 0),
    ("set", 3, 3),
    ("subscribe", 0, 3),
    ("unsubscribe", 0, 1),
    ("who", 0, 1),
];

fn looks_like_command(text: &str) -> bool {
    let mut words = text.split_whitespace();
    let Some(first) = words.next() else {
        return false;
    };
    let first = first.to_ascii_lowercase();
    let arguments = words.count();
    EMAIL_COMMANDS
        .iter()
        .any(|(command, min, max)| *command == first && (*min..=*max).contains(&arguments))
}

/// A short post whose subject or body is an email command: the author almost
/// certainly meant to send it to `-request`.
#[derive(Debug)]
pub struct Administrivia;

impl Rule for Administrivia {
    fn name(&self) -> &'static str {
        "administrivia"
    }
    fn check(&self, ctx: &PostingContext, _state: &mut EvalState) -> Option<String> {
        if !ctx.list.administrivia {
            return None;
        }
        let lines = &ctx.message.body_lines;
        if lines.len() > ADMINISTRIVIA_MAX_LINES {
            return None;
        }
        let body = lines.join("\n");
        let subject = ctx.header("Subject").unwrap_or_default();
        (looks_like_command(&body) || looks_like_command(subject))
            .then(|| "Message contains administrivia".to_owned())
    }
}

/// Neither the list's posting address nor an acceptable alias appears in the
/// visible To/Cc, so the post reached the list by Bcc or alias expansion.
#[derive(Debug)]
pub struct ImplicitDest;

impl Rule for ImplicitDest {
    fn name(&self) -> &'static str {
        "implicit-dest"
    }
    fn check(&self, ctx: &PostingContext, _state: &mut EvalState) -> Option<String> {
        if !ctx.list.require_explicit_destination {
            return None;
        }
        let posting = ctx.list.posting_address.to_ascii_lowercase();
        let explicit = ctx.message.recipients.iter().any(|recipient| {
            recipient.eq_ignore_ascii_case(&posting)
                || address_list_matches(&ctx.list.acceptable_aliases, recipient)
        });
        (!explicit).then(|| "Message has implicit destination".to_owned())
    }
}

/// Site-wide `[antispam] header_checks`. Per-list rows are the separate
/// `header-match` chain.
#[derive(Debug)]
pub struct SuspiciousHeader;

impl Rule for SuspiciousHeader {
    fn name(&self) -> &'static str {
        "suspicious-header"
    }
    fn check(&self, ctx: &PostingContext, _state: &mut EvalState) -> Option<String> {
        match evaluate_header_rules(ctx, &ctx.site_header_checks) {
            HeaderRuleHit::None => None,
            HeaderRuleHit::Matched { header, .. } => {
                Some(format!("Header \"{header}\" matched a header rule"))
            }
            HeaderRuleHit::InvalidPattern { position, header } => Some(format!(
                "Site header check {position} for \"{header}\" has an invalid pattern"
            )),
        }
    }
}

/// The sender holds a membership on this list and their action is not
/// `defer`. Records the action for the `moderation` chain; an explicit
/// `accept` therefore bypasses the deferred checks, as in Mailman.
#[derive(Debug)]
pub struct MemberModeration;

impl Rule for MemberModeration {
    fn name(&self) -> &'static str {
        "member-moderation"
    }
    fn check(&self, ctx: &PostingContext, state: &mut EvalState) -> Option<String> {
        let action = crate::chain::member_moderation_action(ctx)?;
        if action == ModerationAction::Defer {
            return None;
        }
        state.moderation_action = Some(action);
        Some("moderation policy".to_owned())
    }
}

/// The sender holds no membership.
///
/// The legacy `*_these_nonmembers` lists win, then a `nonmember` role row's
/// override, then the list default. A resolved `defer` means "keep
/// evaluating"; anything else is recorded for the `moderation` chain.
#[derive(Debug)]
pub struct NonmemberModeration;

impl NonmemberModeration {
    fn resolve(ctx: &PostingContext) -> ModerationAction {
        let sender = ctx.envelope_sender.as_deref().unwrap_or_default();
        let lists = [
            (&ctx.list.accept_these_nonmembers, ModerationAction::Accept),
            (&ctx.list.hold_these_nonmembers, ModerationAction::Hold),
            (&ctx.list.reject_these_nonmembers, ModerationAction::Reject),
            (
                &ctx.list.discard_these_nonmembers,
                ModerationAction::Discard,
            ),
        ];
        for (entries, action) in lists {
            if address_list_matches(entries, sender) {
                return action;
            }
        }
        ctx.sender
            .nonmember_action
            .unwrap_or(ctx.default_nonmember_action)
    }
}

impl Rule for NonmemberModeration {
    fn name(&self) -> &'static str {
        "nonmember-moderation"
    }
    fn check(&self, ctx: &PostingContext, state: &mut EvalState) -> Option<String> {
        if ctx.member_moderation_action.is_some() {
            return None;
        }
        let action = Self::resolve(ctx);
        if action == ModerationAction::Defer {
            return None;
        }
        state.moderation_action = Some(action);
        Some("moderation policy".to_owned())
    }
}

/// Always hits. Chain glue for an unconditional final link or detour.
#[derive(Debug)]
pub struct Truth;

impl Rule for Truth {
    fn name(&self) -> &'static str {
        "truth"
    }
    fn check(&self, _ctx: &PostingContext, _state: &mut EvalState) -> Option<String> {
        Some(silent())
    }
}

/// Hits when any earlier rule in this evaluation hit. Chain glue that turns
/// a run of `defer` links into one decision.
#[derive(Debug)]
pub struct Any;

impl Rule for Any {
    fn name(&self) -> &'static str {
        "any"
    }
    fn check(&self, _ctx: &PostingContext, state: &mut EvalState) -> Option<String> {
        (!state.hits.is_empty()).then(silent)
    }
}

/// Every rule the shipped registry knows, in `default-posting-chain` order
/// followed by the glue rules.
#[must_use]
pub fn builtin_rules() -> Vec<Box<dyn Rule>> {
    vec![
        Box::new(DmarcMitigation),
        Box::new(NoSenders),
        Box::new(Approved),
        Box::new(Emergency),
        Box::new(Loop),
        Box::new(BannedAddress),
        Box::new(MemberModeration),
        Box::new(NonmemberModeration),
        Box::new(Administrivia),
        Box::new(ImplicitDest),
        Box::new(MaxRecipients),
        Box::new(MaxSize),
        Box::new(NoSubject),
        Box::new(SuspiciousHeader),
        Box::new(Any),
        Box::new(Truth),
    ]
}
