//! Characterization of `decide_posting` over the core input domain.
//!
//! `oracle` is a transcription of the Mailman 3 built-in posting chain
//! contract (`docs/PLAN.md` §4.3), not of the engine: any divergence between
//! the two is a behavior change and must be justified, not absorbed.
//!
//! Notable contract points this locks in, all of which differ from the
//! pre-engine flat ladder:
//!
//! - an `Approved:` key bypasses everything but `no-senders`;
//! - `emergency` holds even an approved-by-role member, but not an approved key;
//! - `loop` discards before `banned-address` rejects;
//! - a member's explicit `accept` bypasses the size/recipient checks, and an
//!   explicit `discard`/`reject` wins over them;
//! - several deferred checks that hit together produce one hold whose reason
//!   lists each of them, in chain order.
//!
//! Message-level facts (subject, senders, body, destination, header rules)
//! are held at a well-formed baseline here and exercised in `tests/rules.rs`.
use listmngr_core::ModerationAction;
use listmngr_pipeline::{
    Disposition, ListChecks, MessageChecks, PostingContext, SenderChecks, decide_posting,
};

const ACTIONS: [ModerationAction; 5] = [
    ModerationAction::Defer,
    ModerationAction::Accept,
    ModerationAction::Hold,
    ModerationAction::Reject,
    ModerationAction::Discard,
];

const SENDER: &str = "alice@example.invalid";
const POSTING: &str = "dev@example.invalid";

/// A well-formed post: explicit destination, subject, author, ordinary body.
fn clean_message() -> MessageChecks {
    MessageChecks {
        headers: vec![
            ("From".into(), format!("Alice <{SENDER}>")),
            ("To".into(), POSTING.into()),
            ("Subject".into(), "Release notes".into()),
        ],
        body_lines: vec!["Here are the release notes for this week.".into()],
        recipients: vec![POSTING.into()],
    }
}

fn moderation(action: ModerationAction) -> Disposition {
    match action {
        ModerationAction::Defer | ModerationAction::Accept => Disposition::Accept,
        ModerationAction::Hold => Disposition::Hold("moderation policy".into()),
        ModerationAction::Reject => Disposition::Reject("moderation policy".into()),
        ModerationAction::Discard => Disposition::Discard("moderation policy".into()),
    }
}

/// The Mailman built-in chain, transcribed for the core domain.
fn oracle(ctx: &PostingContext) -> Disposition {
    let Some(sender) = ctx.envelope_sender.as_deref() else {
        return Disposition::Discard("null reverse path".into());
    };
    if ctx.sender.is_approved {
        return Disposition::Accept;
    }
    if ctx.list.emergency {
        return Disposition::Hold("list is in emergency moderation mode".into());
    }
    if ctx.sender.is_loop {
        return Disposition::Discard("List-Post header names this list (loop)".into());
    }
    if ctx.sender.is_banned {
        return Disposition::Reject(format!("{sender} is banned from this list"));
    }
    let action = match ctx.member_moderation_action {
        Some(Some(action)) => action,
        Some(None) => ctx.default_member_action,
        None => ctx
            .sender
            .nonmember_action
            .unwrap_or(ctx.default_nonmember_action),
    };
    if action != ModerationAction::Defer {
        return moderation(action);
    }
    let mut reasons = Vec::new();
    if ctx.list.too_many_recipients {
        reasons.push("message exceeds list max_num_recipients or has malformed To/Cc");
    }
    if ctx.list.message_too_large {
        reasons.push("message exceeds list max_message_size");
    }
    if !reasons.is_empty() {
        return Disposition::Hold(reasons.join("; "));
    }
    Disposition::Accept
}

/// Every distinct `member_moderation_action` shape: absent (nonmember), present
/// with no override (member), and present with each explicit override.
///
/// The nested `Option` mirrors `PostingContext::member_moderation_action`
/// exactly; collapsing it here would stop enumerating the production domain.
#[allow(clippy::option_option)]
fn membership_shapes() -> Vec<Option<Option<ModerationAction>>> {
    let mut shapes = vec![None, Some(None)];
    shapes.extend(ACTIONS.map(|action| Some(Some(action))));
    shapes
}

/// Nonmember role-row overrides: none, or each explicit action.
fn nonmember_shapes() -> Vec<Option<ModerationAction>> {
    let mut shapes = vec![None];
    shapes.extend(ACTIONS.map(Some));
    shapes
}

fn contexts() -> impl Iterator<Item = PostingContext> {
    let bits = |value: usize, index: u32| value & (1 << index) != 0;
    (0..128).flat_map(move |flags| {
        membership_shapes().into_iter().flat_map(move |membership| {
            nonmember_shapes().into_iter().flat_map(move |nonmember| {
                ACTIONS.into_iter().flat_map(move |member_default| {
                    ACTIONS
                        .into_iter()
                        .map(move |nonmember_default| PostingContext {
                            envelope_sender: bits(flags, 0).then(|| SENDER.to_owned()),
                            sender: SenderChecks {
                                is_banned: bits(flags, 1),
                                is_loop: bits(flags, 2),
                                is_approved: bits(flags, 6),
                                nonmember_action: nonmember,
                            },
                            list: ListChecks {
                                emergency: bits(flags, 3),
                                message_too_large: bits(flags, 4),
                                too_many_recipients: bits(flags, 5),
                                posting_address: POSTING.into(),
                                administrivia: true,
                                require_explicit_destination: true,
                                ..ListChecks::default()
                            },
                            message: clean_message(),
                            site_header_checks: Vec::new(),
                            site_jump_chain: "hold".into(),
                            member_moderation_action: membership,
                            default_member_action: member_default,
                            default_nonmember_action: nonmember_default,
                        })
                })
            })
        })
    })
}

#[test]
fn decide_posting_matches_the_mailman_chain_contract_on_every_core_input() {
    let mut checked = 0_usize;
    for ctx in contexts() {
        assert_eq!(
            decide_posting(&ctx),
            oracle(&ctx),
            "disposition diverged for {ctx:?}"
        );
        checked += 1;
    }
    assert_eq!(checked, 134_400, "input domain enumeration changed size");
}

#[test]
fn every_context_reaches_exactly_one_disposition() {
    // The engine must be total: no input may fall off the end of the chain.
    for ctx in contexts() {
        let disposition = decide_posting(&ctx);
        assert!(
            matches!(
                disposition,
                Disposition::Accept
                    | Disposition::Hold(_)
                    | Disposition::Reject(_)
                    | Disposition::Discard(_)
            ),
            "no disposition for {ctx:?}"
        );
    }
}

#[test]
fn hold_and_reject_reasons_never_leak_an_empty_string() {
    // Held/rejected reasons reach moderators and DSNs; an empty one is a bug.
    for ctx in contexts() {
        match decide_posting(&ctx) {
            Disposition::Hold(reason)
            | Disposition::Reject(reason)
            | Disposition::Discard(reason) => {
                assert!(!reason.trim().is_empty(), "empty reason for {ctx:?}");
                assert_ne!(reason, "N/A", "placeholder reason leaked for {ctx:?}");
            }
            Disposition::Accept => {}
        }
    }
}
