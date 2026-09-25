//! Mailman-compatible chain/rule engine.
//!
//! A *chain* is an ordered list of *links*; each link names a *rule* and the
//! *action* to take when that rule hits. Terminal chains (`accept`, `hold`,
//! `reject`, `discard`) end evaluation and produce a [`Disposition`].
//!
//! The engine is pure: rules read only the [`PostingContext`] the caller
//! gathered plus the evaluation state so far ([`EvalState`], Mailman's
//! `msgdata`), so no database or network access happens here. Concept names
//! are kept identical to Mailman 3 (`docs/PLAN.md` §4.3) so operators can read
//! Mailman documentation and still recognize this runtime.
use crate::policy::{Disposition, HeaderMatch, PostingContext};
use listmngr_core::ModerationAction;
use std::fmt;
use std::sync::OnceLock;

/// Upper bound on link evaluations for one message, so a mis-wired chain
/// cycle fails loudly instead of hanging a runner.
const HOP_LIMIT: usize = 256;

/// Reason recorded when a non-accept terminal is reached without any rule
/// having supplied one. Mailman prints the same placeholder.
const NO_REASON: &str = "N/A";

/// Regular-expression compile budget for header rules: patterns come from
/// list owners and site config, never from message content, but a runaway
/// pattern must still not exhaust a runner.
const HEADER_PATTERN_SIZE_LIMIT: usize = 1 << 20;

/// A chain that ends evaluation with a fixed disposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terminal {
    Accept,
    Hold,
    Reject,
    Discard,
}

/// What a link does when its rule hits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkAction {
    /// Transfer to another chain and never come back.
    Jump(&'static str),
    /// Run another chain, then resume after this link if it did not terminate.
    Detour(&'static str),
    /// Record the hit and continue with the next link.
    Defer,
    /// End evaluation with no disposition.
    Stop,
    /// Record a named side effect for the caller, then continue.
    Run(&'static str),
}

/// One `(rule, action)` pair inside a chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Link {
    rule: &'static str,
    action: LinkAction,
}

impl Link {
    #[must_use]
    pub const fn new(rule: &'static str, action: LinkAction) -> Self {
        Self { rule, action }
    }

    #[must_use]
    pub const fn rule(&self) -> &'static str {
        self.rule
    }

    #[must_use]
    pub const fn action(&self) -> LinkAction {
        self.action
    }
}

/// The shapes a chain can take.
#[derive(Debug, Clone)]
pub enum ChainKind {
    /// Ends evaluation with this disposition.
    Terminal(Terminal),
    /// Maps the moderation action a rule recorded in [`EvalState`] to a
    /// disposition, the way Mailman's `moderation` chain reads
    /// `msgdata['moderation_action']`. With no recorded action it holds.
    Moderation,
    /// Evaluates the list's `header_matches` rows in position order and jumps
    /// to the matching row's chain (site default `hold`). Falls through when
    /// nothing matches, so it is meant to be reached by `Detour`.
    HeaderMatch,
    /// An ordered list of links.
    Links(Vec<Link>),
    /// Mailman's `dmarc-mitigation` chain: reject or discard by the list's
    /// `dmarc_mitigate_action` with the reason the rule recorded.
    DmarcMitigation,
}

/// A named chain.
#[derive(Debug, Clone)]
pub struct Chain {
    name: &'static str,
    kind: ChainKind,
}

impl Chain {
    #[must_use]
    pub const fn terminal(name: &'static str, terminal: Terminal) -> Self {
        Self {
            name,
            kind: ChainKind::Terminal(terminal),
        }
    }

    #[must_use]
    pub const fn moderation(name: &'static str) -> Self {
        Self {
            name,
            kind: ChainKind::Moderation,
        }
    }

    #[must_use]
    pub const fn header_match(name: &'static str) -> Self {
        Self {
            name,
            kind: ChainKind::HeaderMatch,
        }
    }

    #[must_use]
    pub const fn dmarc_mitigation(name: &'static str) -> Self {
        Self {
            name,
            kind: ChainKind::DmarcMitigation,
        }
    }

    #[must_use]
    pub const fn links(name: &'static str, links: Vec<Link>) -> Self {
        Self {
            name,
            kind: ChainKind::Links(links),
        }
    }

    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    #[must_use]
    pub const fn kind(&self) -> &ChainKind {
        &self.kind
    }

    /// Whether the engine can run this chain to a disposition today. A declared
    /// but empty chain is reported as not executable rather than pretending.
    #[must_use]
    pub const fn is_executable(&self) -> bool {
        match &self.kind {
            ChainKind::Terminal(_)
            | ChainKind::Moderation
            | ChainKind::HeaderMatch
            | ChainKind::DmarcMitigation => true,
            ChainKind::Links(links) => !links.is_empty(),
        }
    }
}

/// Mutable evaluation state for one message: Mailman's `msgdata`.
///
/// Rules may read it (`any`) or record decisions in it (`member-moderation`
/// sets the action the `moderation` chain applies).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvalState {
    /// Rules that hit, in evaluation order.
    pub hits: Vec<String>,
    /// Rules that were evaluated and did not hit, in evaluation order.
    pub misses: Vec<String>,
    /// Non-empty reasons supplied by hitting rules, in evaluation order.
    pub reasons: Vec<String>,
    /// Names recorded by [`LinkAction::Run`] links, in evaluation order.
    pub effects: Vec<String>,
    /// Tags recorded by matching header rules, in evaluation order.
    pub tags: Vec<String>,
    /// The action the `moderation` chain will apply; set by the member and
    /// nonmember moderation rules.
    pub moderation_action: Option<ModerationAction>,
}

/// A single posting rule.
///
/// Returning `Some(reason)` means the rule hit; a non-empty reason travels to
/// whatever terminal ends evaluation and is surfaced to moderators, so it must
/// be short and free of secrets. Glue rules return `Some(String::new())` to
/// hit without a reason.
pub trait Rule: fmt::Debug + Send + Sync {
    fn name(&self) -> &'static str;
    fn check(&self, ctx: &PostingContext, state: &mut EvalState) -> Option<String>;
}

/// Why the engine could not produce a disposition. Callers must fail closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainError {
    UnknownChain {
        name: String,
    },
    UnknownRule {
        name: String,
    },
    /// The chain stopped or ran out of links without reaching a terminal.
    NoDisposition {
        chain: String,
    },
    HopLimit {
        limit: usize,
    },
}

impl fmt::Display for ChainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownChain { name } => write!(f, "unknown chain: {name}"),
            Self::UnknownRule { name } => write!(f, "unknown rule: {name}"),
            Self::NoDisposition { chain } => {
                write!(f, "chain {chain} produced no disposition")
            }
            Self::HopLimit { limit } => write!(f, "chain hop limit {limit} exceeded"),
        }
    }
}

impl std::error::Error for ChainError {}

/// The result of running a chain, including the trace needed for the
/// `X-Mailman-Rule-Hits` / `X-Mailman-Rule-Misses` headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub disposition: Disposition,
    /// Rules that hit, in evaluation order.
    pub hits: Vec<String>,
    /// Rules that were evaluated and did not hit, in evaluation order.
    pub misses: Vec<String>,
    /// Names recorded by [`LinkAction::Run`] links, in evaluation order.
    pub effects: Vec<String>,
    /// Tags recorded by matching header rules, in evaluation order.
    pub tags: Vec<String>,
}

impl Outcome {
    fn new(disposition: Disposition, state: EvalState) -> Self {
        Self {
            disposition,
            hits: state.hits,
            misses: state.misses,
            effects: state.effects,
            tags: state.tags,
        }
    }
}

/// The moderation action that applies to a *member* sender: the per-member
/// override, else the list default for members. `None` for nonmembers.
#[must_use]
pub const fn member_moderation_action(ctx: &PostingContext) -> Option<ModerationAction> {
    match ctx.member_moderation_action {
        Some(Some(action)) => Some(action),
        Some(None) => Some(ctx.default_member_action),
        None => None,
    }
}

/// Compile a header-rule pattern exactly as evaluation will.
///
/// Unanchored search, case-insensitive, bounded size. Owners' rules are
/// validated with this same function so a rule that saves is a rule that runs.
///
/// # Errors
/// Returns the regex error for a pattern that does not compile or exceeds the
/// size budget.
pub fn compile_header_pattern(pattern: &str) -> Result<regex::Regex, regex::Error> {
    regex::RegexBuilder::new(pattern)
        .case_insensitive(true)
        .size_limit(HEADER_PATTERN_SIZE_LIMIT)
        .build()
}

/// Search `pattern` in every occurrence of `header`, case-insensitively.
///
/// # Errors
/// Returns the regex error for a pattern that does not compile or exceeds the
/// size budget. Callers fail closed on it.
pub fn header_matches(
    ctx: &PostingContext,
    header: &str,
    pattern: &str,
) -> Result<bool, regex::Error> {
    let regex = compile_header_pattern(pattern)?;
    Ok(ctx.headers(header).any(|value| regex.is_match(value)))
}

/// Outcome of evaluating one list of [`HeaderMatch`] rows in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeaderRuleHit {
    /// No row matched.
    None,
    /// The first matching row. Position is 1-based, in row order.
    Matched {
        position: usize,
        header: String,
        chain: Option<String>,
        tag: Option<String>,
    },
    /// A row's pattern does not compile. Position is 1-based for operators.
    InvalidPattern { position: usize, header: String },
}

/// Evaluate header rows in order, stopping at the first match or bad pattern.
#[must_use]
pub fn evaluate_header_rules(ctx: &PostingContext, rows: &[HeaderMatch]) -> HeaderRuleHit {
    for (index, row) in rows.iter().enumerate() {
        match header_matches(ctx, &row.header, &row.pattern) {
            Ok(true) => {
                return HeaderRuleHit::Matched {
                    position: index + 1,
                    header: row.header.clone(),
                    chain: row.chain.clone(),
                    tag: row.tag.clone(),
                };
            }
            Ok(false) => {}
            Err(_) => {
                return HeaderRuleHit::InvalidPattern {
                    position: index + 1,
                    header: row.header.clone(),
                };
            }
        }
    }
    HeaderRuleHit::None
}

/// A set of rules and chains that can evaluate a posting.
#[derive(Debug, Default)]
pub struct Registry {
    rules: Vec<Box<dyn Rule>>,
    chains: Vec<Chain>,
}

/// Where evaluation continues after a step.
enum Next<'a> {
    /// Transfer to this chain; never return here.
    Jump(&'a str),
    /// Run this chain, then resume at the next link if it does not terminate.
    Detour(&'a str),
    /// Next link in the current chain.
    Continue,
    /// This chain is finished without a disposition.
    Halt,
}

impl Registry {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            rules: Vec::new(),
            chains: Vec::new(),
        }
    }

    /// Registration order is preserved and is the order reported to operators.
    /// A repeated name replaces the earlier entry.
    pub fn register_rule(&mut self, rule: Box<dyn Rule>) {
        let name = rule.name();
        if let Some(slot) = self
            .rules
            .iter_mut()
            .find(|existing| existing.name() == name)
        {
            *slot = rule;
        } else {
            self.rules.push(rule);
        }
    }

    pub fn register_chain(&mut self, chain: Chain) {
        if let Some(slot) = self
            .chains
            .iter_mut()
            .find(|existing| existing.name == chain.name)
        {
            *slot = chain;
        } else {
            self.chains.push(chain);
        }
    }

    #[must_use]
    pub fn rule(&self, name: &str) -> Option<&dyn Rule> {
        self.rules
            .iter()
            .find(|rule| rule.name() == name)
            .map(AsRef::as_ref)
    }

    #[must_use]
    pub fn chain(&self, name: &str) -> Option<&Chain> {
        self.chains.iter().find(|chain| chain.name == name)
    }

    /// Chain names in registration order.
    pub fn chain_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.chains.iter().map(Chain::name)
    }

    /// Chains in registration order.
    pub fn chains(&self) -> impl Iterator<Item = &Chain> + '_ {
        self.chains.iter()
    }

    /// Evaluate `entry` against `ctx`.
    ///
    /// # Errors
    /// Returns [`ChainError`] when a chain or rule is missing, when evaluation
    /// ends without a disposition, or when the hop limit is exceeded. Every
    /// variant means the configuration is wrong, so callers must fail closed
    /// rather than treat it as an accept.
    pub fn run<'a>(
        &'a self,
        entry: &'a str,
        ctx: &'a PostingContext,
    ) -> Result<Outcome, ChainError> {
        let mut state = EvalState::default();
        // Return addresses pushed by `Detour`, innermost last.
        let mut returns: Vec<(&'a str, usize)> = Vec::new();
        let mut current: &'a str = self
            .chain(entry)
            .ok_or_else(|| ChainError::UnknownChain {
                name: entry.to_owned(),
            })?
            .name;
        let mut index = 0_usize;

        for _ in 0..HOP_LIMIT {
            let chain = self
                .chain(current)
                .ok_or_else(|| ChainError::UnknownChain {
                    name: current.to_owned(),
                })?;
            let next = match chain.kind() {
                ChainKind::Terminal(terminal) => {
                    let disposition = terminal_disposition(*terminal, &state.reasons);
                    return Ok(Outcome::new(disposition, state));
                }
                ChainKind::Moderation => {
                    let disposition =
                        moderation_disposition(state.moderation_action, &state.reasons);
                    return Ok(Outcome::new(disposition, state));
                }
                ChainKind::DmarcMitigation => {
                    let disposition = match ctx.list.dmarc_action {
                        listmngr_core::DmarcMitigateAction::Discard => {
                            Disposition::Discard(joined_reason(&state.reasons))
                        }
                        _ => Disposition::Reject(joined_reason(&state.reasons)),
                    };
                    return Ok(Outcome::new(disposition, state));
                }
                ChainKind::HeaderMatch => header_match_step(ctx, &mut state),
                ChainKind::Links(links) => match links.get(index) {
                    Some(link) => self.link_step(link, ctx, &mut state)?,
                    None => Next::Halt,
                },
            };
            match next {
                Next::Jump(target) => {
                    current = target;
                    index = 0;
                }
                Next::Detour(target) => {
                    returns.push((current, index + 1));
                    current = target;
                    index = 0;
                }
                Next::Continue => index += 1,
                Next::Halt => {
                    // Exhausted: resume the chain that detoured here, if any.
                    let Some((chain_name, resume)) = returns.pop() else {
                        return Err(ChainError::NoDisposition {
                            chain: current.to_owned(),
                        });
                    };
                    current = chain_name;
                    index = resume;
                }
            }
        }
        Err(ChainError::HopLimit { limit: HOP_LIMIT })
    }
}

impl Registry {
    /// Evaluate one link: run its rule, record the trace, and say where to go.
    fn link_step<'a>(
        &'a self,
        link: &'a Link,
        ctx: &PostingContext,
        state: &mut EvalState,
    ) -> Result<Next<'a>, ChainError> {
        let rule = self
            .rule(link.rule())
            .ok_or_else(|| ChainError::UnknownRule {
                name: link.rule().to_owned(),
            })?;
        let Some(reason) = rule.check(ctx, state) else {
            state.misses.push(link.rule().to_owned());
            return Ok(Next::Continue);
        };
        state.hits.push(link.rule().to_owned());
        if !reason.is_empty() {
            state.reasons.push(reason);
        }
        Ok(match link.action() {
            LinkAction::Jump(target) => Next::Jump(target),
            LinkAction::Detour(target) => Next::Detour(target),
            LinkAction::Defer => Next::Continue,
            LinkAction::Run(effect) => {
                state.effects.push(effect.to_owned());
                Next::Continue
            }
            LinkAction::Stop => Next::Halt,
        })
    }
}

/// One step of the dynamic `header-match` chain over the list's rows.
///
/// A matching row jumps to its chain (site default `hold`); an invalid pattern
/// fails closed to `hold` with a reason that names the row for the operator.
fn header_match_step<'a>(ctx: &'a PostingContext, state: &mut EvalState) -> Next<'a> {
    let hit = evaluate_header_rules(ctx, &ctx.list.header_matches);
    match hit {
        HeaderRuleHit::None => Next::Halt,
        HeaderRuleHit::Matched {
            position,
            header,
            tag,
            ..
        } => {
            state.hits.push("header-match".to_owned());
            state
                .reasons
                .push(format!("Header \"{header}\" matched a header rule"));
            if let Some(tag) = tag {
                state.tags.push(tag);
            }
            let target = ctx.list.header_matches[position - 1]
                .chain
                .as_deref()
                .unwrap_or(ctx.site_jump_chain.as_str());
            Next::Jump(target)
        }
        HeaderRuleHit::InvalidPattern { position, header } => {
            state.hits.push("header-match".to_owned());
            state.reasons.push(format!(
                "Header rule {position} for \"{header}\" has an invalid pattern"
            ));
            Next::Jump("hold")
        }
    }
}

fn joined_reason(reasons: &[String]) -> String {
    if reasons.is_empty() {
        NO_REASON.to_owned()
    } else {
        reasons.join("; ")
    }
}

fn terminal_disposition(terminal: Terminal, reasons: &[String]) -> Disposition {
    match terminal {
        Terminal::Accept => Disposition::Accept,
        Terminal::Hold => Disposition::Hold(joined_reason(reasons)),
        Terminal::Reject => Disposition::Reject(joined_reason(reasons)),
        Terminal::Discard => Disposition::Discard(joined_reason(reasons)),
    }
}

fn moderation_disposition(action: Option<ModerationAction>, reasons: &[String]) -> Disposition {
    match action {
        Some(ModerationAction::Defer | ModerationAction::Accept) => Disposition::Accept,
        Some(ModerationAction::Hold) | None => Disposition::Hold(joined_reason(reasons)),
        Some(ModerationAction::Reject) => Disposition::Reject(joined_reason(reasons)),
        Some(ModerationAction::Discard) => Disposition::Discard(joined_reason(reasons)),
    }
}

/// The shipped rules and chains.
///
/// The `default-posting-chain` reproduces Mailman 3's built-in chain.
/// `crates/pipeline/tests/policy_characterization.rs` holds the result to a
/// transcription of that contract over the whole input domain.
#[must_use]
pub fn builtin() -> &'static Registry {
    static BUILTIN: OnceLock<Registry> = OnceLock::new();
    BUILTIN.get_or_init(|| {
        let mut registry = Registry::new();
        for rule in crate::rules::builtin_rules() {
            registry.register_rule(rule);
        }
        // The build's plugins: their rules, and their links as a detour
        // the posting chain takes once a ban is ruled out and before
        // moderation — only when there is one, so a build without
        // plugins traces exactly the chain Mailman does.
        let mut plugin_links = Vec::new();
        for plugin in crate::plugins::installed() {
            for rule in plugin.rules() {
                registry.register_rule(rule);
            }
            plugin_links.extend(plugin.links());
        }
        let mut posting = vec![
            Link::new("dmarc-mitigation", LinkAction::Jump("dmarc-mitigation")),
            Link::new("no-senders", LinkAction::Jump("discard")),
            Link::new("approved", LinkAction::Jump("accept")),
            Link::new("emergency", LinkAction::Jump("hold")),
            Link::new("loop", LinkAction::Jump("discard")),
            Link::new("banned-address", LinkAction::Jump("reject")),
            // Determine whether the member or nonmember has an action to take.
            Link::new("member-moderation", LinkAction::Jump("moderation")),
            Link::new("nonmember-moderation", LinkAction::Jump("moderation")),
            // Do all of the following before deciding on a moderation action.
            Link::new("administrivia", LinkAction::Defer),
            Link::new("implicit-dest", LinkAction::Defer),
            Link::new("max-recipients", LinkAction::Defer),
            Link::new("max-size", LinkAction::Defer),
            Link::new("news-moderation", LinkAction::Defer),
            Link::new("no-subject", LinkAction::Defer),
            Link::new("suspicious-header", LinkAction::Defer),
            // Now if any of the above hit, jump to the moderation chain.
            Link::new("any", LinkAction::Jump("moderation")),
            // Take a detour through the list's own header-match rows.
            Link::new("truth", LinkAction::Detour("header-match")),
            // Finally, the message must be accepted.
            Link::new("truth", LinkAction::Jump("accept")),
        ];
        if !plugin_links.is_empty() {
            posting.insert(6, Link::new("truth", LinkAction::Detour("plugins")));
            registry.register_chain(Chain::links("plugins", plugin_links));
        }
        registry.register_chain(Chain::links("default-posting-chain", posting));
        registry.register_chain(Chain::links(
            "default-owner-chain",
            vec![Link::new("truth", LinkAction::Jump("accept"))],
        ));
        registry.register_chain(Chain::terminal("accept", Terminal::Accept));
        registry.register_chain(Chain::terminal("hold", Terminal::Hold));
        registry.register_chain(Chain::terminal("reject", Terminal::Reject));
        registry.register_chain(Chain::terminal("discard", Terminal::Discard));
        registry.register_chain(Chain::moderation("moderation"));
        registry.register_chain(Chain::header_match("header-match"));
        registry.register_chain(Chain::dmarc_mitigation("dmarc-mitigation"));
        registry
    })
}
