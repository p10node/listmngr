#![forbid(unsafe_code)]

//! Inbound posting policy (accept/hold/reject/discard) and outgoing header
//! decoration. Pure decision logic only; queue/database access, LMTP/SMTP
//! transport, and full handler parity remain in `listmngr-runners`/`-mail`
//! or open Phase 2/3 work — see `docs/PLAN.md`.
//!
//! Moderation runs on the Mailman-compatible chain/rule engine in [`chain`];
//! the shipped rules live in [`rules`].

pub mod chain;
pub mod handlers;
pub mod plugins;
pub mod policy;
pub mod rules;
pub mod topics;

pub use chain::{
    Chain, ChainError, ChainKind, EvalState, HeaderRuleHit, Link, LinkAction, Outcome, Registry,
    Rule, Terminal, builtin, compile_header_pattern, evaluate_header_rules, header_matches,
    member_moderation_action,
};
pub use policy::{
    ADMINISTRIVIA_MAX_LINES, CandidateRecipient, Disposition, HeaderMatch, ListChecks,
    ListHeaderInfo, MessageChecks, POSTING_CHAIN, PostingContext, SenderChecks, decide_posting,
    decide_posting_traced, list_headers, select_recipients,
};
