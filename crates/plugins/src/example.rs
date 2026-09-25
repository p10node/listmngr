//! The example plugin: one of each thing a plugin can add, as small as
//! it can be.
//!
//! A post whose subject carries `[hold-me]` is held; the
//! `example-posting-pipeline` is the default one with a handler that
//! stamps `X-Listmngr-Plugin: example`; the `example` archiver appends a
//! line per archived post to the file `LISTMNGR_PLUGIN_EXAMPLE_ARCHIVE`
//! names.
use listmngr_pipeline::PostingContext;
use listmngr_pipeline::chain::{EvalState, Link, LinkAction, Rule};
use listmngr_pipeline::handlers::{
    DEFAULT_POSTING_PIPELINE, Handler, HandlerError, MsgData, Pipeline, PipelineContext, Working,
    builtin_pipelines,
};
use listmngr_pipeline::plugins::{Archiver, Plugin};
use std::io::Write as _;

/// What a subject must carry to be held.
pub const KEYWORD: &str = "[hold-me]";
/// The header the handler stamps.
pub const HEADER: &str = "X-Listmngr-Plugin";

#[derive(Debug)]
pub struct Example;

#[derive(Debug)]
struct KeywordHold;

impl Rule for KeywordHold {
    fn name(&self) -> &'static str {
        "keyword-hold"
    }
    fn check(&self, ctx: &PostingContext, _state: &mut EvalState) -> Option<String> {
        ctx.message
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("subject"))
            .filter(|(_, value)| value.contains(KEYWORD))
            .map(|_| "the subject asks to be held".to_owned())
    }
}

#[derive(Debug)]
struct ExampleHeader;

impl Handler for ExampleHeader {
    fn name(&self) -> &'static str {
        "plugin-example-header"
    }
    fn process(
        &self,
        message: &mut Working<'_>,
        _ctx: &PipelineContext<'_>,
        _data: &mut MsgData,
    ) -> Result<(), HandlerError> {
        let crlf = message
            .raw
            .iter()
            .position(|byte| *byte == b'\n')
            .is_some_and(|at| at > 0 && message.raw[at - 1] == b'\r');
        let line = format!("{HEADER}: example{}", if crlf { "\r\n" } else { "\n" });
        message.raw.splice(0..0, line.into_bytes());
        Ok(())
    }
}

/// Appends `<list> <hash> <bytes>` per archived post to a file: the one
/// `LISTMNGR_PLUGIN_EXAMPLE_ARCHIVE` names when the plugin is installed,
/// or the one given.
#[derive(Debug)]
pub struct ExampleArchiver {
    path: Option<std::path::PathBuf>,
}

impl ExampleArchiver {
    #[must_use]
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            path: Some(path.into()),
        }
    }

    fn from_environment() -> Self {
        Self {
            path: std::env::var_os("LISTMNGR_PLUGIN_EXAMPLE_ARCHIVE").map(Into::into),
        }
    }
}

impl Archiver for ExampleArchiver {
    fn name(&self) -> &'static str {
        "example"
    }
    fn archive(&self, list_id: &str, hash: &str, raw: &[u8]) -> Result<(), String> {
        let path = self
            .path
            .as_ref()
            .ok_or_else(|| "LISTMNGR_PLUGIN_EXAMPLE_ARCHIVE is not set".to_owned())?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|error| error.to_string())?;
        writeln!(file, "{list_id} {hash} {}", raw.len()).map_err(|error| error.to_string())
    }
}

impl Plugin for Example {
    fn name(&self) -> &'static str {
        "example"
    }
    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }
    fn rules(&self) -> Vec<Box<dyn Rule>> {
        vec![Box::new(KeywordHold)]
    }
    fn links(&self) -> Vec<Link> {
        vec![Link::new("keyword-hold", LinkAction::Jump("hold"))]
    }
    fn handlers(&self) -> Vec<Box<dyn Handler>> {
        vec![Box::new(ExampleHeader)]
    }
    fn pipelines(&self) -> Vec<Pipeline> {
        // The default pipeline with the stamp before delivery.
        let mut handlers: Vec<&'static str> = builtin_pipelines()
            .iter()
            .find(|pipeline| pipeline.name() == DEFAULT_POSTING_PIPELINE)
            .map(|pipeline| pipeline.handlers().to_vec())
            .unwrap_or_default();
        let at = handlers
            .iter()
            .position(|handler| *handler == "to-outgoing")
            .unwrap_or(handlers.len());
        handlers.insert(at, "plugin-example-header");
        vec![Pipeline::new("example-posting-pipeline", handlers)]
    }
    fn archivers(&self) -> Vec<Box<dyn Archiver>> {
        vec![Box::new(ExampleArchiver::from_environment())]
    }
}
