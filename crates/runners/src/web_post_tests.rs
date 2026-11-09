//! A web post carries member authority only (security finding F-23): it is
//! not an `Approved:` post, so emergency moderation, the size check and the
//! posting-rate limit decide it exactly as they decide the member's mail,
//! and a moderated member's web post is held like their mail.
use listmngr_core::ModerationAction;
use listmngr_pipeline::policy::{Disposition, PostingContext, decide_posting};

/// A verified member's context, with their own moderation override.
fn member(own: Option<ModerationAction>) -> PostingContext {
    let mut ctx = PostingContext {
        envelope_sender: Some("alice@example.org".into()),
        member_moderation_action: Some(own),
        ..PostingContext::default()
    };
    ctx.message
        .headers
        .push(("From".into(), "alice@example.org".into()));
    ctx.message.headers.push(("Subject".into(), "hello".into()));
    ctx.list.posting_address = "dev@example.org".into();
    ctx.message.recipients.push("dev@example.org".into());
    ctx
}

fn is_hold(ctx: &PostingContext) -> bool {
    matches!(decide_posting(ctx), Disposition::Hold(_))
}

#[test]
fn a_web_post_is_not_an_approved_post() {
    let ctx = member(None);
    assert!(!ctx.sender.is_approved);
    assert!(
        matches!(decide_posting(&ctx), Disposition::Accept),
        "{:?}",
        decide_posting(&ctx)
    );
    let mut ctx = member(None);
    ctx.list.emergency = true;
    assert!(is_hold(&ctx), "emergency moderation holds it");
    let mut ctx = member(None);
    ctx.list.message_too_large = true;
    assert!(is_hold(&ctx), "the size limit holds it");
    let mut ctx = member(Some(ModerationAction::Hold));
    assert!(is_hold(&ctx), "a moderated member is held");
    ctx.sender.is_approved = true;
    assert!(
        matches!(decide_posting(&ctx), Disposition::Accept),
        "only a verified posting key approves"
    );
}
