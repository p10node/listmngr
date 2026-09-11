//! Pipeline engine: snapshot semantics per target, effect collection, and
//! failure modes.
//!
//! Byte-transforming handlers are exercised in `listmngr-mail`.
use listmngr_core::{ArchivePolicy, MailingList};
use listmngr_pipeline::handlers::{
    DEFAULT_POSTING_PIPELINE, Effect, FanOut, Handler, HandlerError, HandlerRegistry, MsgData,
    Pipeline, PipelineContext, PipelineError, Target, Working, builtin_pipelines, fan_out_handlers,
};

fn list() -> MailingList {
    MailingList::new("dev.example.invalid".parse().unwrap(), "Dev".into())
}

/// Appends its name to the body so the snapshot position is observable.
#[derive(Debug)]
struct Stamp(&'static str);

impl Handler for Stamp {
    fn name(&self) -> &'static str {
        self.0
    }
    fn process(
        &self,
        message: &mut Working<'_>,
        _ctx: &PipelineContext<'_>,
        _data: &mut MsgData,
    ) -> Result<(), HandlerError> {
        message.raw.extend_from_slice(self.0.as_bytes());
        message.raw.push(b'|');
        Ok(())
    }
}

#[derive(Debug)]
struct Refuse;

impl Handler for Refuse {
    fn name(&self) -> &'static str {
        "refuse"
    }
    fn process(
        &self,
        _message: &mut Working<'_>,
        _ctx: &PipelineContext<'_>,
        _data: &mut MsgData,
    ) -> Result<(), HandlerError> {
        Err(HandlerError {
            refusal: listmngr_pipeline::handlers::Refusal::Shunt,
            handler: "refuse",
            reason: "nope".into(),
        })
    }
}

fn registry() -> HandlerRegistry {
    let mut registry = HandlerRegistry::new();
    for handler in fan_out_handlers() {
        registry.register_handler(handler);
    }
    for name in ["first", "second", "third"] {
        registry.register_handler(Box::new(Stamp(name)));
    }
    registry.register_handler(Box::new(Refuse));
    registry.register_pipeline(Pipeline::new(
        "p",
        vec![
            "member-recipients",
            "first",
            "to-archive",
            "second",
            "to-digest",
            "third",
            "to-outgoing",
        ],
    ));
    registry
}

const fn ctx(list: &MailingList, target: Target) -> PipelineContext<'_> {
    PipelineContext {
        list,
        identity: "identity",
        target,
    }
}

#[test]
fn each_consumer_gets_the_bytes_as_of_its_own_fan_out_handler() {
    let list = list();
    let registry = registry();
    for (target, expected) in [
        (Target::Archive, "|first|"),
        (Target::Digest, "|first|second|"),
        (Target::Out, "|first|second|third|"),
    ] {
        let (bytes, data) = registry.run("p", b"|", &ctx(&list, target)).unwrap();
        assert_eq!(bytes, expected.as_bytes(), "{target:?}");
        assert!(
            !data.effects.contains(&Effect::Enqueue(match target {
                Target::Archive => FanOut::Archive,
                Target::Digest => FanOut::Digest,
                _ => FanOut::Out,
            })),
            "{target:?}: the snapshot handler itself must not run"
        );
    }
}

#[test]
fn planning_runs_every_handler_and_collects_the_effects_in_order() {
    let list = list();
    let (bytes, data) = registry()
        .run("p", b"|", &ctx(&list, Target::Plan))
        .unwrap();
    assert_eq!(bytes, b"|first|second|third|");
    assert_eq!(
        data.effects,
        vec![
            Effect::PlanRecipients,
            Effect::Enqueue(FanOut::Archive),
            Effect::Enqueue(FanOut::Digest),
            Effect::Enqueue(FanOut::Out),
        ]
    );
    assert_eq!(
        data.ran,
        vec![
            "member-recipients",
            "first",
            "to-archive",
            "second",
            "to-digest",
            "third",
            "to-outgoing"
        ]
    );
}

#[test]
fn a_never_archived_list_gets_no_archive_effect() {
    let mut list = list();
    list.archive_policy = ArchivePolicy::Never;
    let (_, data) = registry()
        .run("p", b"|", &ctx(&list, Target::Plan))
        .unwrap();
    assert!(!data.effects.contains(&Effect::Enqueue(FanOut::Archive)));
    assert!(data.effects.contains(&Effect::Enqueue(FanOut::Out)));
}

#[test]
fn handlers_see_the_original_bytes_even_after_earlier_handlers_changed_the_copy() {
    #[derive(Debug)]
    struct Original;
    impl Handler for Original {
        fn name(&self) -> &'static str {
            "original"
        }
        fn process(
            &self,
            message: &mut Working<'_>,
            _ctx: &PipelineContext<'_>,
            _data: &mut MsgData,
        ) -> Result<(), HandlerError> {
            assert_eq!(message.original, b"|");
            assert_eq!(message.raw, b"|first|");
            Ok(())
        }
    }
    let list = list();
    let mut registry = registry();
    registry.register_handler(Box::new(Original));
    registry.register_pipeline(Pipeline::new("q", vec!["first", "original", "to-outgoing"]));
    registry.run("q", b"|", &ctx(&list, Target::Out)).unwrap();
}

#[test]
fn a_consumer_whose_fan_out_handler_is_missing_has_no_snapshot() {
    let list = list();
    let mut registry = registry();
    registry.register_pipeline(Pipeline::new("no-archive", vec!["first", "to-outgoing"]));
    assert!(matches!(
        registry.run("no-archive", b"|", &ctx(&list, Target::Archive)),
        Err(PipelineError::NoSnapshot { .. })
    ));
    // Planning never needs a snapshot.
    assert!(
        registry
            .run("no-archive", b"|", &ctx(&list, Target::Plan))
            .is_ok()
    );
}

#[test]
fn unknown_pipelines_and_handlers_are_errors_not_silent_passthrough() {
    let list = list();
    let mut registry = registry();
    assert!(matches!(
        registry.run("ghost", b"|", &ctx(&list, Target::Plan)),
        Err(PipelineError::UnknownPipeline { .. })
    ));
    registry.register_pipeline(Pipeline::new(
        "broken",
        vec!["first", "ghost", "to-outgoing"],
    ));
    assert!(matches!(
        registry.run("broken", b"|", &ctx(&list, Target::Out)),
        Err(PipelineError::UnknownHandler { .. })
    ));
    assert!(!registry.is_executable(registry.pipeline("broken").unwrap()));
    assert!(registry.is_executable(registry.pipeline("p").unwrap()));
}

#[test]
fn a_refusing_handler_stops_the_run_with_its_name_and_reason() {
    let list = list();
    let mut registry = registry();
    registry.register_pipeline(Pipeline::new("r", vec!["first", "refuse", "to-outgoing"]));
    match registry.run("r", b"|", &ctx(&list, Target::Out)) {
        Err(PipelineError::Handler(error)) => {
            assert_eq!(error.handler, "refuse");
            assert_eq!(error.reason, "nope");
        }
        other => panic!("expected handler refusal, got {other:?}"),
    }
}

#[test]
fn a_handler_that_stops_before_the_snapshot_never_affects_an_earlier_consumer() {
    // `refuse` sits after `to-archive`, so the archive copy is still produced.
    let list = list();
    let mut registry = registry();
    registry.register_pipeline(Pipeline::new(
        "late-failure",
        vec!["first", "to-archive", "refuse", "to-outgoing"],
    ));
    assert!(
        registry
            .run("late-failure", b"|", &ctx(&list, Target::Archive))
            .is_ok()
    );
    assert!(
        registry
            .run("late-failure", b"|", &ctx(&list, Target::Out))
            .is_err()
    );
}

#[test]
fn the_builtin_posting_pipeline_keeps_mailman_order_with_dmarc_after_the_copies() {
    let pipelines = builtin_pipelines();
    let posting = pipelines
        .iter()
        .find(|pipeline| pipeline.name() == DEFAULT_POSTING_PIPELINE)
        .expect("default posting pipeline");
    assert_eq!(
        posting.handlers(),
        &[
            "mime-delete",
            "member-recipients",
            "cleanse",
            "cleanse-dkim",
            "cook-headers",
            "subject-prefix",
            "rfc-2369",
            "to-archive",
            "to-digest",
            "dmarc",
            "to-outgoing",
        ]
    );
    assert_eq!(
        pipelines
            .iter()
            .find(|pipeline| pipeline.name() == "virgin")
            .unwrap()
            .handlers(),
        &["to-outgoing"]
    );
}
