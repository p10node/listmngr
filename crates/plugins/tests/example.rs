//! The example plugin, installed: described, and reaching every registry
//! — a rule in the posting chain, a handler in a pipeline a list may
//! choose, an archiver by name.
#![cfg(feature = "example")]
use listmngr_core::{MailingList, ModerationAction};
use listmngr_pipeline::handlers::{MsgData, PipelineContext, Target, Working};
use listmngr_pipeline::plugins::Archiver;
use listmngr_pipeline::{
    Disposition, ListChecks, MessageChecks, PostingContext, SenderChecks, decide_posting,
};
use listmngr_plugins::example::{Example, ExampleArchiver, HEADER, KEYWORD};
use listmngr_plugins::{Plugin, describe, install};

/// A nonmember post on a list that accepts nonmembers.
fn ctx(subject: &str) -> PostingContext {
    PostingContext {
        envelope_sender: Some("alice@example.invalid".into()),
        sender: SenderChecks::default(),
        list: ListChecks {
            posting_address: "dev@example.invalid".into(),
            ..ListChecks::default()
        },
        message: MessageChecks {
            headers: vec![
                ("From".into(), "alice@example.invalid".into()),
                ("To".into(), "dev@example.invalid".into()),
                ("Subject".into(), subject.into()),
            ],
            body_lines: vec!["An ordinary post.".into()],
            recipients: vec!["dev@example.invalid".into()],
        },
        site_header_checks: Vec::new(),
        site_jump_chain: "hold".into(),
        member_moderation_action: None,
        default_member_action: ModerationAction::Defer,
        default_nonmember_action: ModerationAction::Accept,
    }
}

#[test]
fn the_example_plugin_is_installed_described_and_wired_through_every_registry() {
    install().unwrap();
    assert!(install().is_err(), "once");
    let described = describe();
    assert_eq!(described.len(), 1);
    let example = &described[0];
    assert_eq!(example.name, "example");
    assert_eq!(example.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(example.rules, ["keyword-hold"]);
    assert_eq!(example.links, ["keyword-hold"]);
    assert_eq!(example.handlers, ["plugin-example-header"]);
    assert_eq!(example.pipelines, ["example-posting-pipeline"]);
    assert_eq!(example.archivers, ["example"]);
    // The rule is registered and linked into the posting chain's detour.
    let registry = listmngr_pipeline::builtin();
    assert!(registry.rule("keyword-hold").is_some());
    assert!(registry.chain("plugins").is_some());
    match decide_posting(&ctx(&format!("{KEYWORD} please"))) {
        Disposition::Hold(reason) => assert!(reason.contains("asks to be held"), "{reason}"),
        other => panic!("{other:?}"),
    }
    assert!(
        matches!(
            decide_posting(&ctx("an ordinary subject")),
            Disposition::Accept
        ),
        "no keyword, no hold"
    );
    // The handler is registered, placed in the plugin's pipeline before
    // delivery, and stamps its header.
    let handlers = listmngr_mail::handlers::builtin_registry();
    let pipeline = handlers.pipeline("example-posting-pipeline").unwrap();
    assert!(handlers.is_executable(pipeline) && pipeline.delivers_posts());
    let at = |name: &str| pipeline.handlers().iter().position(|h| *h == name).unwrap();
    assert_eq!(at("plugin-example-header") + 1, at("to-outgoing"));
    let list = MailingList::new("dev.example.invalid".parse().unwrap(), "Dev".into());
    let context = PipelineContext {
        list: &list,
        identity: "identity",
        target: Target::Plan,
        base_url: None,
        dmarc_mitigate: false,
        authentication_results: None,
        keep_arc: false,
        from_usenet: false,
    };
    let original = b"Subject: x\r\n\r\nbody\r\n";
    let mut working = Working {
        raw: original.to_vec(),
        original,
    };
    handlers
        .handler("plugin-example-header")
        .unwrap()
        .process(&mut working, &context, &mut MsgData::default())
        .unwrap();
    assert!(
        working
            .raw
            .starts_with(format!("{HEADER}: example\r\nSubject: x\r\n").as_bytes()),
        "{}",
        String::from_utf8_lossy(&working.raw)
    );
    // The archiver keeps a line per post where it is told to.
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("archive.log");
    let archiver = ExampleArchiver::new(&log);
    assert_eq!(archiver.name(), "example");
    archiver
        .archive("dev.example.invalid", "HASH", b"raw bytes")
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        "dev.example.invalid HASH 9\n"
    );
    assert_eq!(Example.archivers().len(), 1);
}
