//! Mailman-compatible pipeline of handlers.
//!
//! After the chain accepts a post, a named *pipeline* — an ordered list of
//! *handlers* — transforms the message and decides where copies go. Handler
//! names follow Mailman 3 (`docs/PLAN.md` §4.4).
//!
//! This runtime keeps one immutable stored message and cooks lazily per
//! consumer, so the pipeline is run in two ways from the same definition:
//!
//! - [`Target::Plan`] runs every handler and collects the fan-out
//!   [`Effect`]s (`member-recipients`, `to-archive`, `to-digest`,
//!   `to-outgoing`) the `in` runner turns into child jobs;
//! - a consumer target ([`Target::Out`], [`Target::Archive`],
//!   [`Target::Digest`]) runs the handlers that precede its own fan-out
//!   handler and takes the working bytes there — the same snapshot Mailman's
//!   `to-*` handlers would have copied at that position. That is what puts
//!   `dmarc` after the archive and digest copies but before delivery.
//!
//! Byte-transforming handlers live in `listmngr-mail`, which owns MIME; this
//! module holds the engine and the pure fan-out handlers.
use listmngr_core::{ArchivePolicy, MailingList};
use std::fmt;

/// Which consumer is cooking, or that the run only plans fan-out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Collect effects; no snapshot.
    Plan,
    Out,
    Archive,
    Digest,
}

impl Target {
    /// The fan-out handler whose position is this target's snapshot.
    #[must_use]
    pub const fn snapshot_handler(self) -> Option<&'static str> {
        match self {
            Self::Plan => None,
            Self::Out => Some("to-outgoing"),
            Self::Archive => Some("to-archive"),
            Self::Digest => Some("to-digest"),
        }
    }
}

/// A queue a fan-out handler asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FanOut {
    Out,
    Archive,
    Digest,
}

/// What the `in` runner must do for an accepted post, in handler order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Resolve the delivery roster for the outgoing copy.
    PlanRecipients,
    /// Create a child job on this queue.
    Enqueue(FanOut),
    /// Mailman's `after-delivery`: bump the list's `post_id` and stamp
    /// `last_post_at`.
    RecordPost,
    /// Mailman's `acknowledge`: tell a member who asked for it that their
    /// post was received.
    Acknowledge,
}

/// Mailman's `msgdata`: what handlers recorded while running.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MsgData {
    pub effects: Vec<Effect>,
    /// Handlers that ran, in order.
    pub ran: Vec<String>,
}

/// The message as handlers see it: the working bytes plus the original for
/// handlers that must read what arrived (loop history) rather than what an
/// earlier handler left.
#[derive(Debug)]
pub struct Working<'a> {
    pub raw: Vec<u8>,
    pub original: &'a [u8],
}

/// Everything a handler may read besides the message.
#[derive(Debug, Clone, Copy)]
pub struct PipelineContext<'a> {
    pub list: &'a MailingList,
    /// Application-owned, stable identity for this stored message; used for
    /// generated `Message-ID`s on anonymous lists.
    pub identity: &'a str,
    pub target: Target,
    /// The site's public base URL (`site.base_url`), when the caller knows
    /// it; drives `List-Archive`/`Archived-At`. `None` omits those headers.
    pub base_url: Option<&'a str>,
    /// The `in` runner's `dmarc-mitigation` verdict for this post: the
    /// `dmarc` handler mitigates only when this is set (or the list is
    /// unconditional).
    pub dmarc_mitigate: bool,
    /// The `Authentication-Results` value the `in` runner computed, written
    /// by `validate-authenticity`.
    pub authentication_results: Option<&'a str>,
    /// The site seals deliveries (ARC), so the chain the post arrived with
    /// must stay to be extended; `cleanse-dkim` keeps the `ARC-*` fields.
    pub keep_arc: bool,
}

/// What a handler that ends the pipeline asks the caller to do with the post.
///
/// Mailman's `DiscardMessage`/`RejectMessage` plus the content filter's
/// forward and preserve variants, and the fail-closed default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The message could not be processed safely: shunt it for an operator.
    Shunt,
    /// Drop it silently (Mailman `DiscardMessage`).
    Discard,
    /// Drop it and tell the author why (Mailman `RejectMessage`).
    Reject,
    /// Send the only copy to the moderators, then drop it.
    Forward,
    /// Keep a copy for the site administrator, then drop it.
    Preserve,
}

/// Why a handler refused the message and what the caller should do about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandlerError {
    pub handler: &'static str,
    pub reason: String,
    pub refusal: Refusal,
}

impl fmt::Display for HandlerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "handler {}: {}", self.handler, self.reason)
    }
}

impl std::error::Error for HandlerError {}

/// One pipeline step.
pub trait Handler: fmt::Debug + Send + Sync {
    fn name(&self) -> &'static str;
    /// # Errors
    /// Returns [`HandlerError`] when the message cannot be processed safely.
    fn process(
        &self,
        message: &mut Working<'_>,
        ctx: &PipelineContext<'_>,
        data: &mut MsgData,
    ) -> Result<(), HandlerError>;
}

/// A named, ordered list of handler names.
#[derive(Debug, Clone)]
pub struct Pipeline {
    name: &'static str,
    handlers: Vec<&'static str>,
}

impl Pipeline {
    #[must_use]
    pub const fn new(name: &'static str, handlers: Vec<&'static str>) -> Self {
        Self { name, handlers }
    }

    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    #[must_use]
    pub fn handlers(&self) -> &[&'static str] {
        &self.handlers
    }

    /// Whether a list may run its posts through this pipeline: it must
    /// resolve the roster and hand the copy to delivery, or every accepted
    /// post would be shunted.
    #[must_use]
    pub fn delivers_posts(&self) -> bool {
        self.handlers.contains(&"member-recipients") && self.handlers.contains(&"to-outgoing")
    }
}

/// Why a pipeline run could not complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipelineError {
    UnknownPipeline {
        name: String,
    },
    UnknownHandler {
        name: String,
    },
    /// The target's fan-out handler is not in this pipeline, so there is no
    /// defined snapshot for it.
    NoSnapshot {
        pipeline: String,
        target: Target,
    },
    Handler(HandlerError),
}

impl fmt::Display for PipelineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownPipeline { name } => write!(f, "unknown pipeline: {name}"),
            Self::UnknownHandler { name } => write!(f, "unknown handler: {name}"),
            Self::NoSnapshot { pipeline, target } => {
                write!(f, "pipeline {pipeline} has no snapshot for {target:?}")
            }
            Self::Handler(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for PipelineError {}

impl From<HandlerError> for PipelineError {
    fn from(error: HandlerError) -> Self {
        Self::Handler(error)
    }
}

/// Handlers and pipelines a runtime can execute.
#[derive(Debug, Default)]
pub struct HandlerRegistry {
    handlers: Vec<Box<dyn Handler>>,
    pipelines: Vec<Pipeline>,
}

impl HandlerRegistry {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            handlers: Vec::new(),
            pipelines: Vec::new(),
        }
    }

    /// A repeated name replaces the earlier entry; registration order is the
    /// order reported to operators.
    pub fn register_handler(&mut self, handler: Box<dyn Handler>) {
        let name = handler.name();
        if let Some(slot) = self
            .handlers
            .iter_mut()
            .find(|existing| existing.name() == name)
        {
            *slot = handler;
        } else {
            self.handlers.push(handler);
        }
    }

    pub fn register_pipeline(&mut self, pipeline: Pipeline) {
        if let Some(slot) = self
            .pipelines
            .iter_mut()
            .find(|existing| existing.name == pipeline.name)
        {
            *slot = pipeline;
        } else {
            self.pipelines.push(pipeline);
        }
    }

    #[must_use]
    pub fn handler(&self, name: &str) -> Option<&dyn Handler> {
        self.handlers
            .iter()
            .find(|handler| handler.name() == name)
            .map(AsRef::as_ref)
    }

    #[must_use]
    pub fn pipeline(&self, name: &str) -> Option<&Pipeline> {
        self.pipelines.iter().find(|pipeline| pipeline.name == name)
    }

    /// Pipelines in registration order.
    pub fn pipelines(&self) -> impl Iterator<Item = &Pipeline> + '_ {
        self.pipelines.iter()
    }

    /// Whether every handler a pipeline names is registered, so a run can
    /// actually complete.
    #[must_use]
    pub fn is_executable(&self, pipeline: &Pipeline) -> bool {
        pipeline
            .handlers
            .iter()
            .all(|name| self.handler(name).is_some())
    }

    /// Run `pipeline` for `ctx.target`, returning the working message and the
    /// recorded data.
    ///
    /// For a consumer target the run stops at that target's fan-out handler
    /// and the bytes at that point are the result; for [`Target::Plan`] every
    /// handler runs.
    ///
    /// # Errors
    /// Returns [`PipelineError`] for an unknown pipeline or handler, a
    /// consumer target whose fan-out handler the pipeline lacks, or a handler
    /// refusal. Every variant means the message must not be delivered as is.
    pub fn run(
        &self,
        pipeline: &str,
        raw: &[u8],
        ctx: &PipelineContext<'_>,
    ) -> Result<(Vec<u8>, MsgData), PipelineError> {
        let definition = self
            .pipeline(pipeline)
            .ok_or_else(|| PipelineError::UnknownPipeline {
                name: pipeline.to_owned(),
            })?;
        let snapshot = ctx.target.snapshot_handler();
        if let Some(name) = snapshot
            && !definition.handlers.contains(&name)
        {
            return Err(PipelineError::NoSnapshot {
                pipeline: pipeline.to_owned(),
                target: ctx.target,
            });
        }
        let mut message = Working {
            raw: raw.to_vec(),
            original: raw,
        };
        let mut data = MsgData::default();
        for name in &definition.handlers {
            if snapshot == Some(*name) {
                break;
            }
            let handler = self
                .handler(name)
                .ok_or_else(|| PipelineError::UnknownHandler {
                    name: (*name).to_owned(),
                })?;
            handler.process(&mut message, ctx, &mut data)?;
            data.ran.push((*name).to_owned());
        }
        Ok((message.raw, data))
    }
}

/// Ask the `in` runner to resolve the delivery roster.
#[derive(Debug)]
pub struct MemberRecipients;

impl Handler for MemberRecipients {
    fn name(&self) -> &'static str {
        "member-recipients"
    }
    fn process(
        &self,
        _message: &mut Working<'_>,
        _ctx: &PipelineContext<'_>,
        data: &mut MsgData,
    ) -> Result<(), HandlerError> {
        data.effects.push(Effect::PlanRecipients);
        Ok(())
    }
}

/// Copy to the archive queue unless the list never archives.
#[derive(Debug)]
pub struct ToArchive;

impl Handler for ToArchive {
    fn name(&self) -> &'static str {
        "to-archive"
    }
    fn process(
        &self,
        _message: &mut Working<'_>,
        ctx: &PipelineContext<'_>,
        data: &mut MsgData,
    ) -> Result<(), HandlerError> {
        if ctx.list.archive_policy != ArchivePolicy::Never {
            data.effects.push(Effect::Enqueue(FanOut::Archive));
        }
        Ok(())
    }
}

/// Copy to the digest queue.
#[derive(Debug)]
pub struct ToDigest;

impl Handler for ToDigest {
    fn name(&self) -> &'static str {
        "to-digest"
    }
    fn process(
        &self,
        _message: &mut Working<'_>,
        ctx: &PipelineContext<'_>,
        data: &mut MsgData,
    ) -> Result<(), HandlerError> {
        // Mailman's `digests_enabled`: a list that produces no digests
        // collects nothing, whatever its members asked for.
        if ctx.list.digests_enabled {
            data.effects.push(Effect::Enqueue(FanOut::Digest));
        }
        Ok(())
    }
}

/// Copy to the outgoing queue.
#[derive(Debug)]
pub struct ToOutgoing;

impl Handler for ToOutgoing {
    fn name(&self) -> &'static str {
        "to-outgoing"
    }
    fn process(
        &self,
        _message: &mut Working<'_>,
        _ctx: &PipelineContext<'_>,
        data: &mut MsgData,
    ) -> Result<(), HandlerError> {
        data.effects.push(Effect::Enqueue(FanOut::Out));
        Ok(())
    }
}

/// The pure fan-out handlers this crate ships.
#[must_use]
pub fn fan_out_handlers() -> Vec<Box<dyn Handler>> {
    vec![
        Box::new(MemberRecipients),
        Box::new(ToArchive),
        Box::new(ToDigest),
        Box::new(AfterDelivery),
        Box::new(Acknowledge),
        Box::new(ToOutgoing),
    ]
}

/// Mailman's `after-delivery`: record that the list posted.
#[derive(Debug)]
pub struct AfterDelivery;

impl Handler for AfterDelivery {
    fn name(&self) -> &'static str {
        "after-delivery"
    }
    fn process(
        &self,
        _message: &mut Working<'_>,
        _ctx: &PipelineContext<'_>,
        data: &mut MsgData,
    ) -> Result<(), HandlerError> {
        data.effects.push(Effect::RecordPost);
        Ok(())
    }
}

/// Mailman's `acknowledge`: the poster's receipt, when they asked for one.
#[derive(Debug)]
pub struct Acknowledge;

impl Handler for Acknowledge {
    fn name(&self) -> &'static str {
        "acknowledge"
    }
    fn process(
        &self,
        _message: &mut Working<'_>,
        _ctx: &PipelineContext<'_>,
        data: &mut MsgData,
    ) -> Result<(), HandlerError> {
        data.effects.push(Effect::Acknowledge);
        Ok(())
    }
}

/// The pipeline every list runs unless `posting_pipeline` names another.
pub const DEFAULT_POSTING_PIPELINE: &str = "default-posting-pipeline";

/// The shipped pipeline definitions, in Mailman's handler order.
///
/// Handlers that do not exist yet are left out: `avoid-duplicates`,
/// `to-usenet`, `arc-sign`. Decoration happens at delivery, as in Mailman.
#[must_use]
pub fn builtin_pipelines() -> Vec<Pipeline> {
    vec![
        Pipeline::new(
            DEFAULT_POSTING_PIPELINE,
            vec![
                "validate-authenticity",
                "mime-delete",
                "tagger",
                "member-recipients",
                "cleanse",
                "cleanse-dkim",
                "cook-headers",
                "subject-prefix",
                "rfc-2369",
                "to-archive",
                "to-digest",
                "after-delivery",
                "acknowledge",
                "dmarc",
                "to-outgoing",
            ],
        ),
        // Internally generated mail: already well-formed, deliver as is.
        Pipeline::new("virgin", vec!["to-outgoing"]),
        // Owner mail is cooked by its own admission path today; declared so
        // operators see it, executable once its handlers are ported.
        Pipeline::new(
            "default-owner-pipeline",
            vec!["owner-recipients", "cleanse", "cook-headers", "to-outgoing"],
        ),
    ]
}
