use listmngr_core::{DeliveryMode, DeliveryStatus, ModerationAction};
use listmngr_pipeline::{
    CandidateRecipient, Disposition, ListChecks, ListHeaderInfo, PostingContext, SenderChecks,
    decide_posting, list_headers, select_recipients,
};

fn base_ctx() -> PostingContext {
    PostingContext {
        envelope_sender: Some("alice@example.invalid".into()),
        sender: SenderChecks::default(),
        list: ListChecks::default(),
        member_moderation_action: None,
        default_member_action: ModerationAction::Defer,
        default_nonmember_action: ModerationAction::Hold,
    }
}

#[test]
fn null_reverse_path_is_discarded_not_bounced() {
    let mut ctx = base_ctx();
    ctx.envelope_sender = None;
    assert_eq!(
        decide_posting(&ctx),
        Disposition::Discard("null reverse path".into())
    );
}

#[test]
fn ban_takes_priority_over_membership() {
    let mut ctx = base_ctx();
    ctx.sender.is_banned = true;
    ctx.member_moderation_action = Some(None);
    assert!(matches!(decide_posting(&ctx), Disposition::Reject(_)));
}

#[test]
fn loop_is_discarded_before_emergency_or_membership() {
    let mut ctx = base_ctx();
    ctx.sender.is_loop = true;
    ctx.list.emergency = true;
    assert!(matches!(decide_posting(&ctx), Disposition::Discard(_)));
}

#[test]
fn emergency_holds_even_a_normally_accepted_member() {
    let mut ctx = base_ctx();
    ctx.list.emergency = true;
    ctx.member_moderation_action = Some(None);
    assert!(matches!(decide_posting(&ctx), Disposition::Hold(_)));
}

#[test]
fn unsupported_header_matches_fail_closed_to_hold() {
    let mut ctx = base_ctx();
    ctx.list.has_unsupported_header_matches = true;
    ctx.member_moderation_action = Some(None);
    assert!(matches!(decide_posting(&ctx), Disposition::Hold(_)));
}

#[test]
fn member_defer_falls_through_to_accept() {
    let mut ctx = base_ctx();
    ctx.member_moderation_action = Some(None);
    ctx.default_member_action = ModerationAction::Defer;
    assert_eq!(decide_posting(&ctx), Disposition::Accept);
}

#[test]
fn member_explicit_override_beats_the_list_default() {
    let mut ctx = base_ctx();
    ctx.default_member_action = ModerationAction::Defer;
    ctx.member_moderation_action = Some(Some(ModerationAction::Reject));
    assert!(matches!(decide_posting(&ctx), Disposition::Reject(_)));
}

#[test]
fn nonmember_uses_the_list_nonmember_default() {
    let mut ctx = base_ctx();
    ctx.member_moderation_action = None;
    ctx.default_nonmember_action = ModerationAction::Hold;
    assert!(matches!(decide_posting(&ctx), Disposition::Hold(_)));
}

#[test]
fn nonmember_default_discard_is_honored() {
    let mut ctx = base_ctx();
    ctx.member_moderation_action = None;
    ctx.default_nonmember_action = ModerationAction::Discard;
    assert!(matches!(decide_posting(&ctx), Disposition::Discard(_)));
}

fn candidate(
    email: &str,
    status: DeliveryStatus,
    mode: DeliveryMode,
    own: bool,
) -> CandidateRecipient {
    CandidateRecipient {
        email: email.into(),
        delivery_status: status,
        delivery_mode: mode,
        receive_own_postings: own,
    }
}

#[test]
fn selects_only_enabled_regular_members() {
    let candidates = vec![
        candidate(
            "a@x.invalid",
            DeliveryStatus::Enabled,
            DeliveryMode::Regular,
            true,
        ),
        candidate(
            "b@x.invalid",
            DeliveryStatus::ByBounces,
            DeliveryMode::Regular,
            true,
        ),
        candidate(
            "c@x.invalid",
            DeliveryStatus::Enabled,
            DeliveryMode::MimeDigests,
            true,
        ),
        candidate(
            "d@x.invalid",
            DeliveryStatus::Enabled,
            DeliveryMode::Regular,
            true,
        ),
    ];
    let mut selected = select_recipients(&candidates, "nobody@x.invalid");
    selected.sort();
    assert_eq!(selected, vec!["a@x.invalid", "d@x.invalid"]);
}

#[test]
fn excludes_sender_who_declined_their_own_copy() {
    let candidates = vec![
        candidate(
            "sender@x.invalid",
            DeliveryStatus::Enabled,
            DeliveryMode::Regular,
            false,
        ),
        candidate(
            "other@x.invalid",
            DeliveryStatus::Enabled,
            DeliveryMode::Regular,
            false,
        ),
    ];
    let selected = select_recipients(&candidates, "sender@x.invalid");
    assert_eq!(selected, vec!["other@x.invalid"]);
}

#[test]
fn includes_sender_when_they_want_their_own_copy() {
    let candidates = vec![candidate(
        "sender@x.invalid",
        DeliveryStatus::Enabled,
        DeliveryMode::Regular,
        true,
    )];
    let selected = select_recipients(&candidates, "sender@x.invalid");
    assert_eq!(selected, vec!["sender@x.invalid"]);
}

#[test]
fn list_headers_include_precedence_and_omit_archive_when_absent() {
    let info = ListHeaderInfo {
        list_id: "dev.example.invalid".into(),
        posting_address: "dev@example.invalid".into(),
        subscribe_address: "dev-join@example.invalid".into(),
        unsubscribe_address: "dev-leave@example.invalid".into(),
        archive_url: None,
    };
    let headers = list_headers(&info);
    assert!(headers.contains(&("List-Id".to_owned(), "dev.example.invalid".to_owned())));
    assert!(headers.contains(&("Precedence".to_owned(), "list".to_owned())));
    assert!(!headers.iter().any(|(name, _)| name == "List-Archive"));
}

#[test]
fn list_headers_include_archive_when_present() {
    let info = ListHeaderInfo {
        list_id: "dev.example.invalid".into(),
        posting_address: "dev@example.invalid".into(),
        subscribe_address: "dev-join@example.invalid".into(),
        unsubscribe_address: "dev-leave@example.invalid".into(),
        archive_url: Some("https://example.invalid/archives/dev".into()),
    };
    let headers = list_headers(&info);
    assert!(headers.contains(&(
        "List-Archive".to_owned(),
        "<https://example.invalid/archives/dev>".to_owned()
    )));
}
