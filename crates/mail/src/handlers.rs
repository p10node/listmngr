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

/// Mailman's `validate-authenticity`: write the `Authentication-Results`
/// the `in` runner computed, after removing any inbound field that claims
/// the same authserv-id (RFC 8601 §5).
#[derive(Debug)]
pub struct ValidateAuthenticity;

impl Handler for ValidateAuthenticity {
    fn name(&self) -> &'static str {
        "validate-authenticity"
    }
    fn process(
        &self,
        message: &mut Working<'_>,
        ctx: &PipelineContext<'_>,
        _data: &mut MsgData,
    ) -> std::result::Result<(), HandlerError> {
        let Some(results) = ctx.authentication_results else {
            return Ok(());
        };
        let authserv_id = results
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let (blank_start, _) = cook::header_body_split(&message.raw)
            .ok_or_else(|| refuse(self.name(), &Error::InvalidMessageId))?;
        let mut stripped = cook::strip_fields_where(&message.raw[..blank_start], |name, value| {
            name == "authentication-results"
                && value
                    .split(';')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .eq_ignore_ascii_case(&authserv_id)
        });
        stripped.extend_from_slice(&message.raw[blank_start..]);
        message.raw = cook::append_headers(
            &stripped,
            &[("Authentication-Results".to_owned(), results.to_owned())],
        )
        .map_err(|error| refuse(self.name(), &error))?;
        Ok(())
    }
}

/// Mailman's `tagger`: name the list topics a post hits in `X-Topics`.
#[derive(Debug)]
pub struct Tagger;

impl Handler for Tagger {
    fn name(&self) -> &'static str {
        "tagger"
    }
    fn process(
        &self,
        message: &mut Working<'_>,
        ctx: &PipelineContext<'_>,
        _data: &mut MsgData,
    ) -> std::result::Result<(), HandlerError> {
        let hits = crate::topics::hits(&message.raw, ctx.list);
        if hits.is_empty() {
            return Ok(());
        }
        message.raw = cook::append_headers(
            &message.raw,
            &[(crate::topics::TOPICS_HEADER.to_owned(), hits.join(", "))],
        )
        .map_err(|error| refuse(self.name(), &error))?;
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

/// Mailman's `cook-headers`.
///
/// Anonymous-list identity replacement, the `Sender` rewrite, the `Reply-To`
/// policy, `X-Mailman-Version`, `Message-ID-Hash`, `Precedence: list`, and
/// the `X-BeenThere` loop history (read from the original bytes, since
/// `cleanse` already removed the inbound markers from the working copy).
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
                ("To".into(), list.id.posting_address()),
                (
                    "Message-ID".into(),
                    format!("<{}@{}>", ctx.identity, list.id.mail_host()),
                ),
            ]);
        }
        message.raw = crate::reply_to::apply(&message.raw, list)
            .map_err(|error| refuse(self.name(), &error))?;
        if list.alter_messages.include_sender_header {
            message.raw = cook::strip_named_headers(&message.raw, &["sender"])
                .map_err(|error| refuse(self.name(), &error))?;
            additions.push(("Sender".into(), list.id.bounces_address()));
        }
        additions.push((
            "X-Mailman-Version".into(),
            format!("listmngr {}", env!("CARGO_PKG_VERSION")),
        ));
        let message_id = if list.anonymous_list {
            Some(format!("<{}@{}>", ctx.identity, list.id.mail_host()))
        } else {
            crate::header_value(&message.raw, "message-id")
        };
        if let Some(hash) = message_id.and_then(|id| crate::message_id_hash(&id).ok()) {
            message.raw =
                cook::strip_named_headers(&message.raw, &["message-id-hash", "x-message-id-hash"])
                    .map_err(|error| refuse(self.name(), &error))?;
            additions.push(("Message-ID-Hash".into(), hash.clone()));
            additions.push(("X-Message-ID-Hash".into(), hash));
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

/// Mailman's `rfc-2369`: the `List-*` set, `List-Archive`/`Archived-At`
/// when the site's base URL is known and the list archives.
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
        // `cleanse` already dropped the inbound `List-*` fields; `Archived-At`
        // is ours to replace as well.
        message.raw = cook::strip_named_headers(&message.raw, &["archived-at"])
            .map_err(|error| refuse(self.name(), &error))?;
        let message_id = crate::header_value(&message.raw, "message-id");
        let additions = crate::list_headers::rfc2369(ctx.list, ctx.base_url, message_id.as_deref());
        message.raw = cook::append_headers(&message.raw, &additions)
            .map_err(|error| refuse(self.name(), &error))?;
        Ok(())
    }
}

/// Delivery-only DMARC mitigation (`munge_from` or `wrap_message`). Runs
/// after the archive and digest copies were taken, so only subscribers see
/// the rewritten author or the wrapper.
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
        if list.anonymous_list {
            return Ok(());
        }
        // The `dmarc-mitigation` rule decided at admission whether the
        // From domain's policy applies; an unconditional list mitigates
        // every post.
        if !list.dmarc.unconditional && !ctx.dmarc_mitigate {
            return Ok(());
        }
        let posting_address = list.id.posting_address();
        message.raw = match list.dmarc.action {
            DmarcMitigateAction::MungeFrom => munge::rewrite(&message.raw, &posting_address),
            DmarcMitigateAction::WrapMessage => munge::wrap(
                &message.raw,
                &posting_address,
                list.id.mail_host(),
                &list.dmarc.dmarc_wrapped_message_text,
            ),
            DmarcMitigateAction::NoMitigation
            | DmarcMitigateAction::Reject
            | DmarcMitigateAction::Discard => return Ok(()),
        }
        .map_err(|error| refuse(self.name(), &error))?;
        Ok(())
    }
}

/// The handlers this crate ships.
#[must_use]
pub fn mail_handlers() -> Vec<Box<dyn Handler>> {
    vec![
        Box::new(ValidateAuthenticity),
        Box::new(MimeDelete),
        Box::new(Tagger),
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
    cook_for_site(target, raw, list, identity, None)
}

/// [`cook_for`] with the site's public base URL, which enables
/// `List-Archive` and `Archived-At`.
/// # Errors
/// As [`cook_for`].
pub fn cook_for_site(
    target: Target,
    raw: &[u8],
    list: &MailingList,
    identity: &str,
    base_url: Option<&str>,
) -> Result<Vec<u8>> {
    cook_with(
        target,
        raw,
        list,
        identity,
        &Admission {
            base_url,
            dmarc_mitigate: false,
            authentication_results: None,
        },
    )
}

/// What the `in` runner decided about a post at admission, carried to every
/// consumer's cook so all copies agree.
#[derive(Debug, Clone, Copy, Default)]
pub struct Admission<'a> {
    pub base_url: Option<&'a str>,
    /// The `dmarc-mitigation` rule tagged the post.
    pub dmarc_mitigate: bool,
    /// The `Authentication-Results` value `validate-authenticity` writes.
    pub authentication_results: Option<&'a str>,
}

/// [`cook_for`] with everything the admission recorded.
/// # Errors
/// As [`cook_for`].
pub fn cook_with(
    target: Target,
    raw: &[u8],
    list: &MailingList,
    identity: &str,
    admission: &Admission<'_>,
) -> Result<Vec<u8>> {
    let (bytes, _) = builtin_registry()
        .run(
            &list.posting_pipeline,
            raw,
            &PipelineContext {
                list,
                identity,
                target,
                base_url: admission.base_url,
                dmarc_mitigate: admission.dmarc_mitigate,
                authentication_results: admission.authentication_results,
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
                base_url: None,
                dmarc_mitigate: false,
                authentication_results: None,
            },
        )
        .map_err(pipeline_error)?;
    Ok(data)
}
