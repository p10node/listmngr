//! Owner-supplied address-list patterns are compiled once, not once per
//! message (security finding F-22): the second evaluation of the same list
//! compiles nothing.
use listmngr_pipeline::policy::{PostingContext, decide_posting};

#[test]
fn a_pattern_list_is_not_recompiled_for_every_message() {
    let mut ctx = PostingContext {
        envelope_sender: Some("stranger@example.org".into()),
        ..PostingContext::default()
    };
    ctx.message
        .headers
        .push(("From".into(), "stranger@example.org".into()));
    ctx.message.headers.push(("Subject".into(), "hi".into()));
    ctx.list.accept_these_nonmembers = (0..50)
        .map(|n| format!("^cache-test-{n}@example\\.net"))
        .collect();
    let before = listmngr_pipeline::regex_cache::compilations();
    let first = decide_posting(&ctx);
    let after_first = listmngr_pipeline::regex_cache::compilations();
    assert_eq!(after_first - before, 50, "each pattern compiles once");
    let second = decide_posting(&ctx);
    assert_eq!(
        listmngr_pipeline::regex_cache::compilations(),
        after_first,
        "the next message compiles nothing"
    );
    assert_eq!(format!("{first:?}"), format!("{second:?}"));
}
