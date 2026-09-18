//! A web post is approved like an `Approved:` post only for the verified
//! sender it was recorded for, never when banned, and never for a member
//! the list or an owner moderates.
use crate::policy_facts::{WebPost, approve_web_post};
use listmngr_core::ModerationAction;
use listmngr_pipeline::policy::PostingContext;

/// A member's context: `member` false for a non-member, else their own
/// moderation override.
fn context(sender: &str, member: bool, own: Option<ModerationAction>) -> PostingContext {
    PostingContext {
        envelope_sender: Some(sender.into()),
        member_moderation_action: member.then_some(own),
        ..PostingContext::default()
    }
}

fn web(address: &str) -> WebPost {
    WebPost {
        user_id: "u1".into(),
        address: address.into(),
    }
}

#[test]
fn a_deferred_member_is_approved_and_others_are_not() {
    let mut ctx = context("alice@example.org", true, None);
    assert!(approve_web_post(&mut ctx, Some(&web("Alice@Example.org"))));
    assert!(ctx.sender.is_approved);
    let mut ctx = context("alice@example.org", true, Some(ModerationAction::Accept));
    assert!(approve_web_post(&mut ctx, Some(&web("alice@example.org"))));
    for held in [
        ModerationAction::Hold,
        ModerationAction::Reject,
        ModerationAction::Discard,
    ] {
        let mut ctx = context("alice@example.org", true, Some(held));
        assert!(!approve_web_post(&mut ctx, Some(&web("alice@example.org"))));
        assert!(!ctx.sender.is_approved, "{held:?}");
    }
    // The list's default applies to a member without an override.
    let mut ctx = context("alice@example.org", true, None);
    ctx.default_member_action = ModerationAction::Hold;
    assert!(!approve_web_post(&mut ctx, Some(&web("alice@example.org"))));
    // Not a member, not the same sender, banned, or no web origin at all.
    let mut ctx = context("alice@example.org", false, None);
    assert!(!approve_web_post(&mut ctx, Some(&web("alice@example.org"))));
    let mut ctx = context("mallory@example.org", true, None);
    assert!(!approve_web_post(&mut ctx, Some(&web("alice@example.org"))));
    let mut ctx = context("alice@example.org", true, None);
    ctx.sender.is_banned = true;
    assert!(!approve_web_post(&mut ctx, Some(&web("alice@example.org"))));
    let mut ctx = context("alice@example.org", true, None);
    assert!(!approve_web_post(&mut ctx, None));
    assert!(!ctx.sender.is_approved);
}

#[test]
fn the_context_names_the_web_origin_or_nothing() {
    let context = serde_json::json!({"version":1,"list_id":"dev.example.org","web_post":{"user_id":"u1","address":"alice@example.org","reply":null}});
    assert_eq!(
        WebPost::from_context(&context),
        Some(web("alice@example.org"))
    );
    assert_eq!(
        WebPost::from_context(&serde_json::json!({"version":1})),
        None
    );
    assert_eq!(
        WebPost::from_context(&serde_json::json!({"web_post":{"user_id":"u1"}})),
        None,
        "an address is required"
    );
}
