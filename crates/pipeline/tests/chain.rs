//! Chain/rule engine: link semantics, built-in chain shape, and tracing.
use listmngr_core::ModerationAction;
use listmngr_pipeline::chain::{
    Chain, ChainError, ChainKind, EvalState, Link, LinkAction, Registry, Rule, Terminal, builtin,
};
use listmngr_pipeline::{Disposition, ListChecks, MessageChecks, PostingContext, SenderChecks};

/// A well-formed member post on a list with `defer` defaults.
fn ctx() -> PostingContext {
    PostingContext {
        envelope_sender: Some("alice@example.invalid".into()),
        sender: SenderChecks::default(),
        list: ListChecks {
            posting_address: "dev@example.invalid".into(),
            administrivia: true,
            require_explicit_destination: true,
            ..ListChecks::default()
        },
        message: MessageChecks {
            headers: vec![
                ("From".into(), "alice@example.invalid".into()),
                ("To".into(), "dev@example.invalid".into()),
                ("Subject".into(), "hello".into()),
            ],
            body_lines: vec!["An ordinary post.".into()],
            recipients: vec!["dev@example.invalid".into()],
        },
        site_header_checks: Vec::new(),
        site_jump_chain: "hold".into(),
        member_moderation_action: Some(None),
        default_member_action: ModerationAction::Defer,
        default_nonmember_action: ModerationAction::Hold,
    }
}

/// A rule whose verdict is fixed at construction, for exercising link actions.
#[derive(Debug)]
struct Fixed {
    name: &'static str,
    hit: bool,
}

impl Rule for Fixed {
    fn name(&self) -> &'static str {
        self.name
    }
    fn check(&self, _ctx: &PostingContext, _state: &mut EvalState) -> Option<String> {
        self.hit.then(|| format!("{} fired", self.name))
    }
}

fn registry_with(rules: Vec<(&'static str, bool)>, chains: Vec<Chain>) -> Registry {
    let mut registry = Registry::new();
    for (name, hit) in rules {
        registry.register_rule(Box::new(Fixed { name, hit }));
    }
    for chain in chains {
        registry.register_chain(chain);
    }
    registry.register_chain(Chain::terminal("accept", Terminal::Accept));
    registry.register_chain(Chain::terminal("hold", Terminal::Hold));
    registry.register_chain(Chain::terminal("discard", Terminal::Discard));
    registry
}

#[test]
fn builtin_posting_chain_preserves_the_documented_rule_order() {
    let chain = builtin()
        .chain("default-posting-chain")
        .expect("built-in posting chain");
    let ChainKind::Links(links) = chain.kind() else {
        panic!("posting chain must be a link chain");
    };
    let names: Vec<&str> = links.iter().map(Link::rule).collect();
    // Mailman 3's built-in chain, minus the `news-moderation` link whose
    // rule does not exist yet.
    assert_eq!(
        names,
        vec![
            "dmarc-mitigation",
            "no-senders",
            "approved",
            "emergency",
            "loop",
            "banned-address",
            "member-moderation",
            "nonmember-moderation",
            "administrivia",
            "implicit-dest",
            "max-recipients",
            "max-size",
            "news-moderation",
            "no-subject",
            "suspicious-header",
            "any",
            "truth",
            "truth",
        ]
    );
    let actions: Vec<LinkAction> = links.iter().map(Link::action).collect();
    assert_eq!(
        actions[0],
        LinkAction::Jump("dmarc-mitigation"),
        "a restrictive policy jumps to the DMARC chain"
    );
    assert_eq!(
        actions[15],
        LinkAction::Jump("moderation"),
        "any -> moderation"
    );
    assert_eq!(
        actions[16],
        LinkAction::Detour("header-match"),
        "detour through the list's header rules"
    );
    assert_eq!(actions[17], LinkAction::Jump("accept"));
}

#[test]
fn every_builtin_link_names_a_registered_rule_and_an_existing_chain() {
    let registry = builtin();
    for name in registry.chain_names() {
        let ChainKind::Links(links) = registry.chain(name).expect("chain").kind() else {
            continue;
        };
        for link in links {
            assert!(
                registry.rule(link.rule()).is_some(),
                "chain {name} links unregistered rule {}",
                link.rule()
            );
            if let LinkAction::Jump(target) | LinkAction::Detour(target) = link.action() {
                assert!(
                    registry.chain(target).is_some(),
                    "chain {name} targets missing chain {target}"
                );
            }
        }
    }
}

#[test]
fn jump_transfers_control_without_returning() {
    let registry = registry_with(
        vec![("first", true), ("second", true)],
        vec![Chain::links(
            "entry",
            vec![
                Link::new("first", LinkAction::Jump("hold")),
                Link::new("second", LinkAction::Jump("discard")),
            ],
        )],
    );
    let outcome = registry.run("entry", &ctx()).expect("outcome");
    assert!(matches!(outcome.disposition, Disposition::Hold(_)));
    // The second link must never have been evaluated.
    assert_eq!(outcome.hits, vec!["first"]);
    assert!(outcome.misses.is_empty());
}

#[test]
fn a_missed_link_falls_through_to_the_next_one() {
    let registry = registry_with(
        vec![("first", false), ("second", true)],
        vec![Chain::links(
            "entry",
            vec![
                Link::new("first", LinkAction::Jump("hold")),
                Link::new("second", LinkAction::Jump("discard")),
            ],
        )],
    );
    let outcome = registry.run("entry", &ctx()).expect("outcome");
    assert!(matches!(outcome.disposition, Disposition::Discard(_)));
    assert_eq!(outcome.hits, vec!["second"]);
    assert_eq!(outcome.misses, vec!["first"]);
}

#[test]
fn detour_returns_to_the_outer_chain_when_the_sub_chain_does_not_terminate() {
    let registry = registry_with(
        vec![("gate", true), ("inner", false), ("after", true)],
        vec![
            Chain::links(
                "entry",
                vec![
                    Link::new("gate", LinkAction::Detour("side")),
                    Link::new("after", LinkAction::Jump("discard")),
                ],
            ),
            Chain::links("side", vec![Link::new("inner", LinkAction::Jump("hold"))]),
        ],
    );
    let outcome = registry.run("entry", &ctx()).expect("outcome");
    assert!(matches!(outcome.disposition, Disposition::Discard(_)));
    assert_eq!(outcome.hits, vec!["gate", "after"]);
    assert_eq!(outcome.misses, vec!["inner"]);
}

#[test]
fn detour_that_terminates_wins_over_the_outer_chain() {
    let registry = registry_with(
        vec![("gate", true), ("inner", true), ("after", true)],
        vec![
            Chain::links(
                "entry",
                vec![
                    Link::new("gate", LinkAction::Detour("side")),
                    Link::new("after", LinkAction::Jump("discard")),
                ],
            ),
            Chain::links("side", vec![Link::new("inner", LinkAction::Jump("hold"))]),
        ],
    );
    let outcome = registry.run("entry", &ctx()).expect("outcome");
    assert!(matches!(outcome.disposition, Disposition::Hold(_)));
    assert_eq!(outcome.hits, vec!["gate", "inner"]);
}

#[test]
fn defer_evaluates_the_rule_but_takes_no_action() {
    let registry = registry_with(
        vec![("noted", true), ("after", true)],
        vec![Chain::links(
            "entry",
            vec![
                Link::new("noted", LinkAction::Defer),
                Link::new("after", LinkAction::Jump("hold")),
            ],
        )],
    );
    let outcome = registry.run("entry", &ctx()).expect("outcome");
    assert!(matches!(outcome.disposition, Disposition::Hold(_)));
    assert_eq!(outcome.hits, vec!["noted", "after"]);
}

#[test]
fn stop_ends_the_chain_without_a_disposition() {
    let registry = registry_with(
        vec![("halt", true), ("after", true)],
        vec![Chain::links(
            "entry",
            vec![
                Link::new("halt", LinkAction::Stop),
                Link::new("after", LinkAction::Jump("hold")),
            ],
        )],
    );
    assert!(matches!(
        registry.run("entry", &ctx()),
        Err(ChainError::NoDisposition { .. })
    ));
}

#[test]
fn an_exhausted_chain_reports_no_disposition_rather_than_guessing() {
    let registry = registry_with(
        vec![("miss", false)],
        vec![Chain::links(
            "entry",
            vec![Link::new("miss", LinkAction::Jump("hold"))],
        )],
    );
    assert!(matches!(
        registry.run("entry", &ctx()),
        Err(ChainError::NoDisposition { .. })
    ));
}

#[test]
fn run_records_a_named_effect_and_continues() {
    let registry = registry_with(
        vec![("marker", true), ("after", true)],
        vec![Chain::links(
            "entry",
            vec![
                Link::new("marker", LinkAction::Run("record-authenticity")),
                Link::new("after", LinkAction::Jump("hold")),
            ],
        )],
    );
    let outcome = registry.run("entry", &ctx()).expect("outcome");
    assert_eq!(outcome.effects, vec!["record-authenticity"]);
    assert!(matches!(outcome.disposition, Disposition::Hold(_)));
}

#[test]
fn a_chain_cycle_is_bounded_rather_than_hanging() {
    let registry = registry_with(
        vec![("always", true)],
        vec![
            Chain::links("entry", vec![Link::new("always", LinkAction::Jump("back"))]),
            Chain::links("back", vec![Link::new("always", LinkAction::Jump("entry"))]),
        ],
    );
    assert!(matches!(
        registry.run("entry", &ctx()),
        Err(ChainError::HopLimit { .. })
    ));
}

#[test]
fn an_unknown_chain_or_rule_is_an_error_not_a_silent_accept() {
    let registry = registry_with(vec![], vec![]);
    assert!(matches!(
        registry.run("nope", &ctx()),
        Err(ChainError::UnknownChain { .. })
    ));

    let registry = registry_with(
        vec![],
        vec![Chain::links(
            "entry",
            vec![Link::new("ghost", LinkAction::Jump("hold"))],
        )],
    );
    assert!(matches!(
        registry.run("entry", &ctx()),
        Err(ChainError::UnknownRule { .. })
    ));
}

#[test]
fn the_terminal_carries_the_reason_of_the_rule_that_jumped_to_it() {
    let registry = registry_with(
        vec![("culprit", true)],
        vec![Chain::links(
            "entry",
            vec![Link::new("culprit", LinkAction::Jump("hold"))],
        )],
    );
    let outcome = registry.run("entry", &ctx()).expect("outcome");
    assert_eq!(
        outcome.disposition,
        Disposition::Hold("culprit fired".into())
    );
}

#[test]
fn the_moderation_chain_maps_the_resolved_action_to_a_terminal() {
    let registry = builtin();
    // The fixture context is a member's, so the member rule's reason.
    let reason = "The message comes from a moderated member";
    for (action, expected) in [
        (ModerationAction::Hold, Disposition::Hold(reason.into())),
        (ModerationAction::Reject, Disposition::Reject(reason.into())),
        (
            ModerationAction::Discard,
            Disposition::Discard(reason.into()),
        ),
        (ModerationAction::Accept, Disposition::Accept),
        (ModerationAction::Defer, Disposition::Accept),
    ] {
        let mut ctx = ctx();
        ctx.member_moderation_action = Some(Some(action));
        assert_eq!(
            registry
                .run("default-posting-chain", &ctx)
                .expect("outcome")
                .disposition,
            expected,
            "moderation action {action:?}"
        );
    }
}

#[test]
fn an_accepted_post_records_every_rule_it_passed() {
    let outcome = builtin()
        .run("default-posting-chain", &ctx())
        .expect("outcome");
    assert_eq!(outcome.disposition, Disposition::Accept);
    // Both `truth` links hit: the detour into `header-match` (which falls
    // through with no rows) and the final jump to `accept`.
    assert_eq!(outcome.hits, vec!["truth", "truth"]);
    assert_eq!(
        outcome.misses,
        vec![
            "dmarc-mitigation",
            "no-senders",
            "approved",
            "emergency",
            "loop",
            "banned-address",
            "member-moderation",
            "nonmember-moderation",
            "administrivia",
            "implicit-dest",
            "max-recipients",
            "max-size",
            "news-moderation",
            "no-subject",
            "suspicious-header",
            "any",
        ]
    );
}

#[test]
fn the_moderation_chain_holds_when_no_rule_recorded_an_action() {
    // Reached via `any` after deferred hits: Mailman holds with the reasons.
    let mut ctx = ctx();
    ctx.list.message_too_large = true;
    let outcome = builtin()
        .run("default-posting-chain", &ctx)
        .expect("outcome");
    assert_eq!(
        outcome.disposition,
        Disposition::Hold("message exceeds list max_message_size".into())
    );
    assert_eq!(outcome.hits, vec!["max-size", "any"]);
}

#[test]
fn entering_a_terminal_directly_yields_the_placeholder_reason() {
    // No rule ran, so there is nothing better to say than Mailman's "N/A".
    let outcome = builtin().run("hold", &ctx()).expect("outcome");
    assert_eq!(outcome.disposition, Disposition::Hold("N/A".into()));
}

#[test]
fn reasons_from_every_hitting_rule_are_joined_in_order() {
    let registry = registry_with(
        vec![("first", true), ("second", true), ("go", true)],
        vec![Chain::links(
            "entry",
            vec![
                Link::new("first", LinkAction::Defer),
                Link::new("second", LinkAction::Defer),
                Link::new("go", LinkAction::Jump("hold")),
            ],
        )],
    );
    let outcome = registry.run("entry", &ctx()).expect("outcome");
    assert_eq!(
        outcome.disposition,
        Disposition::Hold("first fired; second fired; go fired".into())
    );
}

#[test]
fn the_builtin_owner_chain_accepts_unconditionally() {
    // Owner mail bypasses posting moderation entirely; it must not inherit the
    // posting chain's holds.
    let mut ctx = ctx();
    ctx.list.emergency = true;
    ctx.member_moderation_action = None;
    ctx.default_nonmember_action = ModerationAction::Discard;
    let outcome = builtin().run("default-owner-chain", &ctx).expect("outcome");
    assert_eq!(outcome.disposition, Disposition::Accept);
}
