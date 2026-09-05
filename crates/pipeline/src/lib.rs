#![forbid(unsafe_code)]

//! Inbound posting policy (accept/hold/reject/discard) and outgoing header
//! decoration. Pure decision logic only; queue/database access, LMTP/SMTP
//! transport, and full chain/handler parity remain in `listmngr-runners`/`-mail`
//! or open Phase 2/3 work — see `docs/PLAN.md`.

pub mod policy;
pub use policy::{
    CandidateRecipient, Disposition, ListChecks, ListHeaderInfo, PostingContext, SenderChecks,
    decide_posting, list_headers, select_recipients,
};
