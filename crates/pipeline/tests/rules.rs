//! Per-rule behavior of the message-level rules, and their place in the
//! built-in chain. The core boolean/enum domain is covered exhaustively in
//! `policy_characterization.rs`; this file varies the facts that file holds
//! at a well-formed baseline.
use listmngr_core::ModerationAction;
use listmngr_pipeline::{
    Disposition, HeaderMatch, ListChecks, MessageChecks, PostingContext, SenderChecks,
    decide_posting, decide_posting_traced,
};

const SENDER: &str = "alice@example.invalid";
const POSTING: &str = "dev@example.invalid";

/// A well-formed nonmember post to a list whose nonmember default is `defer`,
/// so only the deferred checks decide.
fn base() -> PostingContext {
    PostingContext {
        envelope_sender: Some(SENDER.into()),
        sender: SenderChecks::default(),
        list: ListChecks {
            posting_address: POSTING.into(),
            administrivia: true,
            require_explicit_destination: true,
            ..ListChecks::default()
        },
        message: MessageChecks {
            headers: vec![
                ("From".into(), format!("Alice <{SENDER}>")),
                ("To".into(), POSTING.into()),
                ("Subject".into(), "Release notes".into()),
            ],
            body_lines: vec!["Here are the release notes for this week.".into()],
            recipients: vec![POSTING.into()],
        },
        site_header_checks: Vec::new(),
        site_jump_chain: "hold".into(),
        site_max_received_hops: 0,
        site_posting_rate: None,
        member_moderation_action: None,
        default_member_action: ModerationAction::Defer,
        default_nonmember_action: ModerationAction::Defer,
    }
}

fn set_header(ctx: &mut PostingContext, name: &str, value: &str) {
    ctx.message
        .headers
        .retain(|(field, _)| !field.eq_ignore_ascii_case(name));
    ctx.message.headers.push((name.into(), value.into()));
}

fn remove_header(ctx: &mut PostingContext, name: &str) {
    ctx.message
        .headers
        .retain(|(field, _)| !field.eq_ignore_ascii_case(name));
}

#[test]
fn a_clean_nonmember_post_with_a_defer_default_is_accepted() {
    assert_eq!(decide_posting(&base()), Disposition::Accept);
}

// ---------------------------------------------------------------- no-senders

#[test]
fn a_message_with_no_sender_headers_is_discarded_even_with_an_envelope() {
    let mut ctx = base();
    remove_header(&mut ctx, "From");
    assert_eq!(
        decide_posting(&ctx),
        Disposition::Discard("The message has no valid senders".into())
    );
}

#[test]
fn a_sender_or_reply_to_header_counts_as_a_sender() {
    for name in ["Sender", "Reply-To"] {
        let mut ctx = base();
        remove_header(&mut ctx, "From");
        set_header(&mut ctx, name, SENDER);
        assert_eq!(decide_posting(&ctx), Disposition::Accept, "{name}");
    }
}

#[test]
fn a_blank_from_header_does_not_count_as_a_sender() {
    let mut ctx = base();
    set_header(&mut ctx, "From", "   ");
    assert!(matches!(decide_posting(&ctx), Disposition::Discard(_)));
}

#[test]
fn a_null_reverse_path_is_discarded_before_any_header_is_consulted() {
    let mut ctx = base();
    ctx.envelope_sender = None;
    assert_eq!(
        decide_posting(&ctx),
        Disposition::Discard("null reverse path".into())
    );
}

// ------------------------------------------------------------------ approved

#[test]
fn an_approved_key_bypasses_emergency_bans_and_deferred_checks() {
    let mut ctx = base();
    ctx.sender.is_approved = true;
    ctx.list.emergency = true;
    ctx.sender.is_banned = true;
    ctx.list.message_too_large = true;
    remove_header(&mut ctx, "Subject");
    let outcome = decide_posting_traced(&ctx);
    assert_eq!(outcome.disposition, Disposition::Accept);
    assert_eq!(outcome.hits, vec!["approved"]);
}

#[test]
fn an_approved_key_does_not_rescue_a_null_reverse_path() {
    let mut ctx = base();
    ctx.sender.is_approved = true;
    ctx.envelope_sender = None;
    assert!(matches!(decide_posting(&ctx), Disposition::Discard(_)));
}

// ---------------------------------------------------------------- no-subject

#[test]
fn a_missing_or_blank_subject_is_held() {
    let mut ctx = base();
    remove_header(&mut ctx, "Subject");
    assert_eq!(
        decide_posting(&ctx),
        Disposition::Hold("Message has no subject".into())
    );
    set_header(&mut ctx, "Subject", " \t ");
    assert_eq!(
        decide_posting(&ctx),
        Disposition::Hold("Message has no subject".into())
    );
}

#[test]
fn subject_lookup_is_case_insensitive() {
    let mut ctx = base();
    remove_header(&mut ctx, "Subject");
    ctx.message
        .headers
        .push(("SUBJECT".into(), "shouting".into()));
    assert_eq!(decide_posting(&ctx), Disposition::Accept);
}

// ------------------------------------------------------------- administrivia

#[test]
fn a_short_body_that_is_an_email_command_is_held() {
    let mut ctx = base();
    ctx.message.body_lines = vec!["unsubscribe".into()];
    assert_eq!(
        decide_posting(&ctx),
        Disposition::Hold("Message contains administrivia".into())
    );
}

#[test]
fn a_command_subject_is_held_even_with_an_ordinary_body() {
    let mut ctx = base();
    set_header(&mut ctx, "Subject", "Subscribe");
    assert!(matches!(decide_posting(&ctx), Disposition::Hold(_)));
}

#[test]
fn argument_counts_distinguish_commands_from_prose() {
    // `set` needs exactly three arguments; `who` takes at most one.
    for (body, expected_hold) in [
        ("set delivery off alice@example.invalid", true),
        ("set delivery off", false),
        ("who", true),
        ("who is on this list anyway", false),
        ("confirm 0123456789abcdef", true),
        ("confirm", false),
        ("help", true),
        ("helpful hints for new members", false),
    ] {
        let mut ctx = base();
        ctx.message.body_lines = vec![body.into()];
        assert_eq!(
            matches!(decide_posting(&ctx), Disposition::Hold(_)),
            expected_hold,
            "{body:?}"
        );
    }
}

#[test]
fn a_long_body_is_never_administrivia() {
    let mut ctx = base();
    ctx.message.body_lines = std::iter::once("help".to_owned())
        .chain((0..10).map(|n| format!("line {n}")))
        .collect();
    assert_eq!(decide_posting(&ctx), Disposition::Accept);
}

#[test]
fn administrivia_can_be_disabled_per_list() {
    let mut ctx = base();
    ctx.list.administrivia = false;
    ctx.message.body_lines = vec!["unsubscribe".into()];
    assert_eq!(decide_posting(&ctx), Disposition::Accept);
}

// ------------------------------------------------------------- implicit-dest

#[test]
fn a_post_that_reached_the_list_by_bcc_is_held() {
    let mut ctx = base();
    ctx.message.recipients = vec!["someone-else@example.invalid".into()];
    assert_eq!(
        decide_posting(&ctx),
        Disposition::Hold("Message has implicit destination".into())
    );
}

#[test]
fn the_posting_address_in_cc_counts_as_explicit_regardless_of_case() {
    let mut ctx = base();
    ctx.message.recipients = vec![
        "friend@example.invalid".into(),
        "DEV@Example.Invalid".to_ascii_lowercase(),
    ];
    assert_eq!(decide_posting(&ctx), Disposition::Accept);
}

#[test]
fn acceptable_aliases_match_exactly_or_by_anchored_regex() {
    let mut ctx = base();
    ctx.message.recipients = vec!["announce@example.invalid".into()];
    ctx.list.acceptable_aliases = vec!["Announce@example.invalid".into()];
    assert_eq!(decide_posting(&ctx), Disposition::Accept, "exact");

    ctx.list.acceptable_aliases = vec!["^.*@example\\.invalid$".into()];
    assert_eq!(decide_posting(&ctx), Disposition::Accept, "regex");

    ctx.list.acceptable_aliases = vec!["^announce@other\\.invalid$".into()];
    assert!(
        matches!(decide_posting(&ctx), Disposition::Hold(_)),
        "no match"
    );

    ctx.list.acceptable_aliases = vec!["^(".into()];
    assert!(
        matches!(decide_posting(&ctx), Disposition::Hold(_)),
        "an invalid alias regex never matches"
    );
}

#[test]
fn implicit_destination_can_be_disabled_per_list() {
    let mut ctx = base();
    ctx.list.require_explicit_destination = false;
    ctx.message.recipients = Vec::new();
    assert_eq!(decide_posting(&ctx), Disposition::Accept);
}

// ------------------------------------------------- deferred checks together

#[test]
fn several_deferred_hits_produce_one_hold_listing_each_reason_in_chain_order() {
    let mut ctx = base();
    ctx.list.message_too_large = true;
    ctx.list.too_many_recipients = true;
    remove_header(&mut ctx, "Subject");
    ctx.message.body_lines = vec!["help".into()];
    ctx.message.recipients = Vec::new();
    let outcome = decide_posting_traced(&ctx);
    assert_eq!(
        outcome.disposition,
        Disposition::Hold(
            [
                "Message contains administrivia",
                "Message has implicit destination",
                "message exceeds list max_num_recipients or has malformed To/Cc",
                "message exceeds list max_message_size",
                "Message has no subject",
            ]
            .join("; ")
        )
    );
    assert_eq!(
        outcome.hits,
        vec![
            "administrivia",
            "implicit-dest",
            "max-recipients",
            "max-size",
            "no-subject",
            "any",
        ]
    );
}

#[test]
fn a_member_with_an_explicit_accept_bypasses_the_deferred_checks() {
    let mut ctx = base();
    ctx.member_moderation_action = Some(Some(ModerationAction::Accept));
    ctx.list.message_too_large = true;
    remove_header(&mut ctx, "Subject");
    assert_eq!(decide_posting(&ctx), Disposition::Accept);
}

#[test]
fn a_member_on_defer_is_still_subject_to_the_deferred_checks() {
    let mut ctx = base();
    ctx.member_moderation_action = Some(None);
    ctx.default_member_action = ModerationAction::Defer;
    remove_header(&mut ctx, "Subject");
    assert!(matches!(decide_posting(&ctx), Disposition::Hold(_)));
}

// ------------------------------------------------------- nonmember lists

#[test]
fn legacy_nonmember_lists_win_over_the_role_row_and_the_default() {
    let mut ctx = base();
    ctx.sender.nonmember_action = Some(ModerationAction::Hold);
    ctx.default_nonmember_action = ModerationAction::Discard;

    ctx.list.accept_these_nonmembers = vec![SENDER.to_ascii_uppercase()];
    assert_eq!(decide_posting(&ctx), Disposition::Accept, "accept list");

    ctx.list.accept_these_nonmembers.clear();
    ctx.list.reject_these_nonmembers = vec!["^alice@".into()];
    assert!(
        matches!(decide_posting(&ctx), Disposition::Reject(_)),
        "reject regex"
    );

    ctx.list.reject_these_nonmembers.clear();
    assert!(
        matches!(decide_posting(&ctx), Disposition::Hold(_)),
        "role row override"
    );

    ctx.sender.nonmember_action = None;
    assert!(
        matches!(decide_posting(&ctx), Disposition::Discard(_)),
        "list default"
    );
}

#[test]
fn nonmember_lists_are_ignored_for_members() {
    let mut ctx = base();
    ctx.member_moderation_action = Some(None);
    ctx.list.discard_these_nonmembers = vec![SENDER.into()];
    assert_eq!(decide_posting(&ctx), Disposition::Accept);
}

#[test]
fn an_accept_list_entry_still_does_not_bypass_emergency() {
    let mut ctx = base();
    ctx.list.emergency = true;
    ctx.list.accept_these_nonmembers = vec![SENDER.into()];
    assert!(matches!(decide_posting(&ctx), Disposition::Hold(_)));
}

// ------------------------------------------------------ header rules

fn rule(header: &str, pattern: &str, chain: Option<&str>, tag: Option<&str>) -> HeaderMatch {
    HeaderMatch {
        header: header.into(),
        pattern: pattern.into(),
        chain: chain.map(str::to_owned),
        tag: tag.map(str::to_owned),
    }
}

#[test]
fn a_matching_list_header_rule_holds_by_default_with_a_named_reason() {
    let mut ctx = base();
    set_header(&mut ctx, "X-Spam-Flag", "YES");
    ctx.list.header_matches = vec![rule("x-spam-flag", "^yes$", None, None)];
    let outcome = decide_posting_traced(&ctx);
    assert_eq!(
        outcome.disposition,
        Disposition::Hold("Header \"x-spam-flag\" matched a header rule".into())
    );
    assert!(outcome.hits.contains(&"header-match".to_owned()));
}

#[test]
fn a_list_header_rule_can_name_its_own_chain_and_record_a_tag() {
    let mut ctx = base();
    set_header(&mut ctx, "X-Spam-Score", "12.5");
    ctx.list.header_matches = vec![rule(
        "X-Spam-Score",
        "^1[0-9]",
        Some("discard"),
        Some("spam"),
    )];
    let outcome = decide_posting_traced(&ctx);
    assert!(matches!(outcome.disposition, Disposition::Discard(_)));
    assert_eq!(outcome.tags, vec!["spam"]);
}

#[test]
fn header_rules_are_evaluated_in_position_order_and_the_first_match_wins() {
    let mut ctx = base();
    set_header(&mut ctx, "X-Flag", "both");
    ctx.list.header_matches = vec![
        rule("X-Flag", "nothing", Some("discard"), None),
        rule("X-Flag", "bo", Some("reject"), None),
        rule("X-Flag", "both", Some("discard"), None),
    ];
    assert!(matches!(decide_posting(&ctx), Disposition::Reject(_)));
}

#[test]
fn a_header_rule_matches_any_occurrence_of_a_repeated_header() {
    let mut ctx = base();
    ctx.message
        .headers
        .push(("Received".into(), "from a.example.invalid".into()));
    ctx.message
        .headers
        .push(("Received".into(), "from bad.example.invalid".into()));
    ctx.list.header_matches = vec![rule("received", "bad\\.example", None, None)];
    assert!(matches!(decide_posting(&ctx), Disposition::Hold(_)));
}

#[test]
fn an_unmatched_header_rule_lets_the_post_through() {
    let mut ctx = base();
    ctx.list.header_matches = vec![rule("X-Spam-Flag", "^yes$", Some("discard"), None)];
    assert_eq!(decide_posting(&ctx), Disposition::Accept);
}

#[test]
fn an_invalid_header_rule_pattern_fails_closed_to_hold_naming_the_row() {
    let mut ctx = base();
    ctx.list.header_matches = vec![
        rule("X-Ok", "fine", None, None),
        rule("Subject", "^(", Some("discard"), None),
    ];
    assert_eq!(
        decide_posting(&ctx),
        Disposition::Hold("Header rule 2 for \"Subject\" has an invalid pattern".into())
    );
}

#[test]
fn a_header_rule_naming_an_unknown_chain_fails_closed_to_hold() {
    let mut ctx = base();
    ctx.list.header_matches = vec![rule("Subject", "notes", Some("no-such-chain"), None)];
    assert!(
        matches!(decide_posting(&ctx), Disposition::Hold(reason) if reason.contains("unknown chain"))
    );
}

#[test]
fn a_header_rule_may_accept_which_still_ends_evaluation() {
    let mut ctx = base();
    ctx.list.header_matches = vec![rule("From", "alice", Some("accept"), None)];
    ctx.list.message_too_large = true;
    // The deferred checks come before the header-match detour, so size still holds.
    assert!(matches!(decide_posting(&ctx), Disposition::Hold(_)));
    ctx.list.message_too_large = false;
    assert_eq!(decide_posting(&ctx), Disposition::Accept);
}

#[test]
fn site_header_checks_are_a_deferred_rule_named_suspicious_header() {
    let mut ctx = base();
    set_header(&mut ctx, "X-Spam-Flag", "YES");
    ctx.site_header_checks = vec![rule("X-Spam-Flag", "yes", None, None)];
    let outcome = decide_posting_traced(&ctx);
    assert_eq!(
        outcome.disposition,
        Disposition::Hold("Header \"X-Spam-Flag\" matched a header rule".into())
    );
    assert!(outcome.hits.contains(&"suspicious-header".to_owned()));
}

#[test]
fn header_patterns_are_searched_case_insensitively() {
    let mut ctx = base();
    set_header(&mut ctx, "X-Mailer", "BulkBlaster 3000");
    ctx.list.header_matches = vec![rule("x-mailer", "bulkblaster", None, None)];
    assert!(matches!(decide_posting(&ctx), Disposition::Hold(_)));
}

#[test]
fn a_list_header_rule_without_a_chain_uses_the_site_jump_chain() {
    let mut ctx = base();
    set_header(&mut ctx, "X-Spam-Flag", "YES");
    ctx.list.header_matches = vec![rule("X-Spam-Flag", "yes", None, None)];
    ctx.site_jump_chain = "discard".into();
    assert!(matches!(decide_posting(&ctx), Disposition::Discard(_)));
}

#[test]
fn dmarc_mitigation_tags_munged_posts_and_refuses_when_the_list_says_so() {
    use listmngr_core::DmarcMitigateAction as Action;
    use listmngr_pipeline::rules::{DMARC_REJECT_REASON, DMARC_TAG};

    // No mitigation: the policy is ignored entirely.
    let mut ctx = base();
    ctx.sender.dmarc_policy_restrictive = true;
    let outcome = decide_posting_traced(&ctx);
    assert_eq!(outcome.disposition, Disposition::Accept);
    assert!(!outcome.tags.iter().any(|tag| tag == DMARC_TAG));
    assert!(outcome.misses.contains(&"dmarc-mitigation".to_owned()));

    // munge_from + restrictive policy: accepted, tagged for the handler.
    ctx.list.dmarc_action = Action::MungeFrom;
    let outcome = decide_posting_traced(&ctx);
    assert_eq!(outcome.disposition, Disposition::Accept);
    assert!(outcome.tags.iter().any(|tag| tag == DMARC_TAG));
    assert!(
        outcome.misses.contains(&"dmarc-mitigation".to_owned()),
        "munging is not a hit"
    );

    // No policy, but unconditional or a listed address still tags.
    let mut plain = base();
    plain.list.dmarc_action = Action::MungeFrom;
    assert!(
        !decide_posting_traced(&plain)
            .tags
            .iter()
            .any(|tag| tag == DMARC_TAG)
    );
    plain.list.dmarc_unconditional = true;
    assert!(
        decide_posting_traced(&plain)
            .tags
            .iter()
            .any(|tag| tag == DMARC_TAG)
    );
    plain.list.dmarc_unconditional = false;
    plain.list.dmarc_addresses = vec!["^.*@example\\.invalid$".into()];
    assert!(
        decide_posting_traced(&plain)
            .tags
            .iter()
            .any(|tag| tag == DMARC_TAG)
    );
    plain.list.dmarc_addresses = vec!["other@example.invalid".into()];
    assert!(
        !decide_posting_traced(&plain)
            .tags
            .iter()
            .any(|tag| tag == DMARC_TAG)
    );

    // reject / discard jump to the dmarc-mitigation chain with the notice.
    ctx.list.dmarc_action = Action::Reject;
    let outcome = decide_posting_traced(&ctx);
    assert_eq!(
        outcome.disposition,
        Disposition::Reject(DMARC_REJECT_REASON.into())
    );
    assert!(outcome.hits.contains(&"dmarc-mitigation".to_owned()));
    ctx.list.dmarc_moderation_notice = "Use the web form.".into();
    assert_eq!(
        decide_posting(&ctx),
        Disposition::Reject("Use the web form.".into())
    );
    ctx.list.dmarc_action = Action::Discard;
    assert_eq!(
        decide_posting(&ctx),
        Disposition::Discard("Use the web form.".into())
    );
    // The reject action without an applicable policy lets the post through.
    ctx.sender.dmarc_policy_restrictive = false;
    assert_eq!(decide_posting(&ctx), Disposition::Accept);
}

/// Mailman's `news-moderation`: a list that gateways to a moderated
/// newsgroup holds every post for a moderator, with Mailman's reason; an
/// open or unmoderated group changes nothing.
#[test]
fn a_moderated_newsgroup_gateway_holds_every_post() {
    let mut ctx = base();
    ctx.list.newsgroup_moderation = listmngr_core::NewsgroupModeration::Moderated;
    let outcome = decide_posting_traced(&ctx);
    assert_eq!(
        outcome.disposition,
        Disposition::Hold("Post to a moderated newsgroup gateway".into())
    );
    assert!(
        outcome.hits.iter().any(|rule| rule == "news-moderation"),
        "{:?}",
        outcome.hits
    );
    for open in [
        listmngr_core::NewsgroupModeration::OpenModerated,
        listmngr_core::NewsgroupModeration::None,
    ] {
        ctx.list.newsgroup_moderation = open;
        assert_eq!(decide_posting(&ctx), Disposition::Accept, "{open:?}");
    }
}

/// `max-hops`: more `Received:` headers than `[mta] max_received_hops`
/// discards the post as a loop; exactly the limit passes; zero turns the
/// rule off; it sits after `loop` and before `banned-address`.
#[test]
fn too_many_received_headers_discard_the_post_as_a_loop() {
    let mut ctx = base();
    ctx.site_max_received_hops = 3;
    for hop in 0..3 {
        ctx.message
            .headers
            .push(("Received".into(), format!("from hop{hop}")));
    }
    assert_eq!(decide_posting(&ctx), Disposition::Accept);
    ctx.message
        .headers
        .push(("received".into(), "from hop3".into()));
    let outcome = decide_posting_traced(&ctx);
    assert_eq!(
        outcome.disposition,
        Disposition::Discard("Too many Received: headers (4, at most 3): a mail loop".into())
    );
    assert!(outcome.hits.contains(&"max-hops".to_owned()));
    ctx.sender.is_banned = true;
    assert!(
        matches!(decide_posting(&ctx), Disposition::Discard(_)),
        "the loop is discarded before the ban rejects"
    );
    ctx.sender.is_banned = false;
    ctx.site_max_received_hops = 0;
    assert_eq!(decide_posting(&ctx), Disposition::Accept, "zero is off");
}

/// `posting-rate`: with `[security] rate_limit.post` set, a sender whose
/// accepted posts in the window already reach the count is held with a
/// reason naming the numbers; one fewer passes; no limit means no rule;
/// it is a deferred check, so an explicitly accepted member bypasses it
/// and several deferred hits share one hold.
#[test]
fn a_sender_over_the_posting_rate_is_held_with_the_numbers() {
    let mut ctx = base();
    ctx.sender.recent_posts = 100;
    assert_eq!(
        decide_posting(&ctx),
        Disposition::Accept,
        "no limit, no rule"
    );
    ctx.site_posting_rate = Some(listmngr_core::RateLimit::parse("post", "3/hour").unwrap());
    ctx.sender.recent_posts = 2;
    assert_eq!(decide_posting(&ctx), Disposition::Accept);
    ctx.sender.recent_posts = 3;
    let outcome = decide_posting_traced(&ctx);
    assert_eq!(
        outcome.disposition,
        Disposition::Hold(
            "Posting rate exceeded: 3 posts accepted in the last hour (at most 3)".into()
        )
    );
    assert!(outcome.hits.contains(&"posting-rate".to_owned()));
    ctx.list.message_too_large = true;
    assert_eq!(
        decide_posting(&ctx),
        Disposition::Hold(
            "message exceeds list max_message_size; Posting rate exceeded: 3 posts accepted in the last hour (at most 3)".into()
        ),
        "deferred hits share one hold, in chain order"
    );
    ctx.list.message_too_large = false;
    ctx.member_moderation_action = Some(Some(ModerationAction::Accept));
    assert_eq!(
        decide_posting(&ctx),
        Disposition::Accept,
        "an explicit accept bypasses the deferred checks, as in Mailman"
    );
    for (spec, name) in [
        ("1/s", "second"),
        ("2/min", "minute"),
        ("3/h", "hour"),
        ("4/day", "day"),
    ] {
        assert_eq!(
            listmngr_core::RateLimit::parse("post", spec)
                .unwrap()
                .window_name(),
            name
        );
    }
}
