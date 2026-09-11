//! The byte-transforming pipeline handlers, and the shipped registry.
//!
//! The engine, the fan-out handlers and the pipeline definitions live in
//! `listmngr_pipeline::handlers`; this module supplies the handlers that need
//! MIME and header surgery, and [`cook_for`], the single entry point every
//! consumer uses to obtain its copy of an accepted post.
use crate::{Error, Result, cook, facts, mime_delete, munge};
use listmngr_core::{DmarcMitigateAction, FilterAction, MailingList};
use listmngr_pipeline::handlers::{
    Handler, HandlerError, HandlerRegistry, MsgData, PipelineContext, PipelineError, Working,
    builtin_pipelines, fan_out_handlers,
};
pub use listmngr_pipeline::handlers::{Refusal, Target};
use std::sync::OnceLock;

fn refuse(handler: &'static str, error: &Error) -> HandlerError {
    HandlerError {
        handler,
        reason: error.to_string(),
        refusal: Refusal::Shunt,
    }
}

/// Mailman's `mime-delete`: the list's content filter.
///
/// Runs first so every consumer (archive, digest, subscribers) sees the same
/// filtered message, and ends the pipeline with the list's `filter_action`
/// when nothing deliverable remains.
#[derive(Debug)]
pub struct MimeDelete;

impl Handler for MimeDelete {
    fn name(&self) -> &'static str {
        "mime-delete"
    }
    fn process(
        &self,
        message: &mut Working<'_>,
        ctx: &PipelineContext<'_>,
        _data: &mut MsgData,
    ) -> std::result::Result<(), HandlerError> {
        let settings = &ctx.list.alter_messages;
        match mime_delete::apply(&message.raw, settings)
            .map_err(|error| refuse(self.name(), &error))?
        {
            mime_delete::Verdict::Unchanged => {}
            mime_delete::Verdict::Changed(bytes) => message.raw = bytes,
            mime_delete::Verdict::Disposed(reason) => {
                return Err(HandlerError {
                    handler: self.name(),
                    reason,
                    refusal: match settings.filter_action {
                        FilterAction::Discard => Refusal::Discard,
                        FilterAction::Reject => Refusal::Reject,
                        FilterAction::Forward => Refusal::Forward,
                        FilterAction::Preserve => Refusal::Preserve,
                    },
                });
            }
        }
        Ok(())
    }
}

/// Drop list controls, private recipients and moderator-only fields, and the
/// `Approved:` posting key wherever it was carried.
#[derive(Debug)]
pub struct Cleanse;

impl Handler for Cleanse {
    fn name(&self) -> &'static str {
        "cleanse"
    }
    fn process(
        &self,
        message: &mut Working<'_>,
        _ctx: &PipelineContext<'_>,
        _data: &mut MsgData,
    ) -> std::result::Result<(), HandlerError> {
        let stripped = facts::strip_approved_line(&message.raw);
        message.raw =
            cook::strip_control_headers(&stripped).map_err(|error| refuse(self.name(), &error))?;
        Ok(())
    }
}

/// Drop the original DKIM/ARC signatures that redistribution invalidates.
#[derive(Debug)]
pub struct CleanseDkim;

impl Handler for CleanseDkim {
    fn name(&self) -> &'static str {
        "cleanse-dkim"
    }
    fn process(
        &self,
        message: &mut Working<'_>,
        _ctx: &PipelineContext<'_>,
        _data: &mut MsgData,
    ) -> std::result::Result<(), HandlerError> {
        message.raw = cook::strip_signature_headers(&message.raw)
            .map_err(|error| refuse(self.name(), &error))?;
        Ok(())
    }
}

/// Anonymous-list identity replacement, `Precedence: list`, and the
/// `X-BeenThere` loop history (read from the original bytes, since `cleanse`
/// already removed the inbound markers from the working copy).
#[derive(Debug)]
pub struct CookHeaders;

impl Handler for CookHeaders {
    fn name(&self) -> &'static str {
        "cook-headers"
    }
    fn process(
        &self,
        message: &mut Working<'_>,
        ctx: &PipelineContext<'_>,
        _data: &mut MsgData,
    ) -> std::result::Result<(), HandlerError> {
        let list = ctx.list;
        let mut additions: Vec<(String, String)> = Vec::new();
        if list.anonymous_list {
            message.raw = cook::anonymous_message(&message.raw);
            additions.extend([
                ("From".into(), list.id.posting_address()),
                ("Reply-To".into(), list.id.posting_address()),
                ("To".into(), list.id.posting_address()),
                (
                    "Message-ID".into(),
                    format!("<{}@{}>", ctx.identity, list.id.mail_host()),
                ),
            ]);
        }
        additions.push(("Precedence".into(), "list".into()));
        let mut history = facts::loop_markers(message.original);
        history.push(list.id.posting_address());
        history.sort();
        history.dedup();
        additions.extend(
            history
                .into_iter()
                .map(|address| ("X-BeenThere".to_owned(), address)),
        );
        message.raw = cook::append_headers(&message.raw, &additions)
            .map_err(|error| refuse(self.name(), &error))?;
        Ok(())
    }
}

/// Prepend the list's subject prefix once.
#[derive(Debug)]
pub struct SubjectPrefix;

impl Handler for SubjectPrefix {
    fn name(&self) -> &'static str {
        "subject-prefix"
    }
    fn process(
        &self,
        message: &mut Working<'_>,
        ctx: &PipelineContext<'_>,
        _data: &mut MsgData,
    ) -> std::result::Result<(), HandlerError> {
        message.raw = cook::prefix_subject(&message.raw, &ctx.list.subject_prefix)
            .map_err(|error| refuse(self.name(), &error))?;
        Ok(())
    }
}

/// RFC 2369 `List-*` headers. `List-Archive` waits on a public archive URL.
#[derive(Debug)]
pub struct Rfc2369;

impl Handler for Rfc2369 {
    fn name(&self) -> &'static str {
        "rfc-2369"
    }
    fn process(
        &self,
        message: &mut Working<'_>,
        ctx: &PipelineContext<'_>,
        _data: &mut MsgData,
    ) -> std::result::Result<(), HandlerError> {
        let id = &ctx.list.id;
        let additions: Vec<(String, String)> =
            listmngr_pipeline::list_headers(&listmngr_pipeline::ListHeaderInfo {
                list_id: id.to_string(),
                posting_address: id.posting_address(),
                subscribe_address: id.join_address(),
                unsubscribe_address: id.leave_address(),
                archive_url: None,
            })
            .into_iter()
            // `Precedence` belongs to `cook-headers`; this handler is RFC 2369 only.
            .filter(|(name, _)| name.starts_with("List-"))
            .collect();
        message.raw = cook::append_headers(&message.raw, &additions)
            .map_err(|error| refuse(self.name(), &error))?;
        Ok(())
    }
}

/// Delivery-only DMARC `From` mitigation. Runs after the archive and digest
/// copies were taken, so only subscribers see the rewritten author.
#[derive(Debug)]
pub struct Dmarc;

impl Handler for Dmarc {
    fn name(&self) -> &'static str {
        "dmarc"
    }
    fn process(
        &self,
        message: &mut Working<'_>,
        ctx: &PipelineContext<'_>,
        _data: &mut MsgData,
    ) -> std::result::Result<(), HandlerError> {
        let list = ctx.list;
        if list.anonymous_list || list.dmarc.action != DmarcMitigateAction::MungeFrom {
            return Ok(());
        }
        if !list.dmarc.unconditional {
            // Conditional (DNS-driven) mitigation is not implemented; a list
            // configured for it must not silently deliver unmitigated.
            return Err(refuse(self.name(), &Error::UnsafeHeaderContent));
        }
        message.raw = munge::rewrite(&message.raw, &list.id.posting_address())
            .map_err(|error| refuse(self.name(), &error))?;
        Ok(())
    }
}

/// The handlers this crate ships.
#[must_use]
pub fn mail_handlers() -> Vec<Box<dyn Handler>> {
    vec![
        Box::new(MimeDelete),
        Box::new(Cleanse),
        Box::new(CleanseDkim),
        Box::new(CookHeaders),
        Box::new(SubjectPrefix),
        Box::new(Rfc2369),
        Box::new(Dmarc),
    ]
}

/// The shipped handler registry: mail handlers, fan-out handlers, and the
/// built-in pipeline definitions.
#[must_use]
pub fn builtin_registry() -> &'static HandlerRegistry {
    static BUILTIN: OnceLock<HandlerRegistry> = OnceLock::new();
    BUILTIN.get_or_init(|| {
        let mut registry = HandlerRegistry::new();
        for handler in fan_out_handlers() {
            registry.register_handler(handler);
        }
        for handler in mail_handlers() {
            registry.register_handler(handler);
        }
        for pipeline in builtin_pipelines() {
            registry.register_pipeline(pipeline);
        }
        registry
    })
}

/// A pipeline refusal as the callers see it: a handler that ended the
/// pipeline with a disposition becomes [`Error::Refused`]; everything else
/// (unknown pipeline, missing snapshot, unsafe content) is
/// [`Error::UnsafeHeaderContent`], which callers shunt.
fn pipeline_error(error: PipelineError) -> Error {
    match error {
        PipelineError::Handler(HandlerError {
            handler,
            reason,
            refusal,
        }) if refusal != Refusal::Shunt => Error::Refused {
            handler,
            reason,
            refusal,
        },
        _ => Error::UnsafeHeaderContent,
    }
}

/// Run the list's posting pipeline for one consumer and return that
/// consumer's copy of the message.
/// # Errors
/// Returns [`Error::Refused`] when a handler ended the pipeline with a
/// disposition, and [`Error::UnsafeHeaderContent`] for any other refusal: an
/// unknown pipeline, a missing snapshot handler, or a handler that could not
/// process the message. Callers shunt the latter rather than deliver.
pub fn cook_for(target: Target, raw: &[u8], list: &MailingList, identity: &str) -> Result<Vec<u8>> {
    let (bytes, _) = builtin_registry()
        .run(
            &list.posting_pipeline,
            raw,
            &PipelineContext {
                list,
                identity,
                target,
            },
        )
        .map_err(pipeline_error)?;
    Ok(bytes)
}

/// Run the list's posting pipeline in planning mode and return what the `in`
/// runner must do with the accepted post.
/// # Errors
/// Returns [`Error::Refused`] when a handler ended the pipeline with a
/// disposition, else [`Error::UnsafeHeaderContent`] for any other refusal.
pub fn plan(raw: &[u8], list: &MailingList, identity: &str) -> Result<MsgData> {
    let (_, data) = builtin_registry()
        .run(
            &list.posting_pipeline,
            raw,
            &PipelineContext {
                list,
                identity,
                target: Target::Plan,
            },
        )
        .map_err(pipeline_error)?;
    Ok(data)
}
