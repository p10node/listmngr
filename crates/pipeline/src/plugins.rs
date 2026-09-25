//! Compile-time plugins: the rules, chain links, handlers, pipelines and
//! archivers a build adds to the built-in ones.
//!
//! A plugin is a Rust type linked into the binary — `listmngr-plugins`
//! lists the ones a build carries — and installed once at start, before
//! the registries are first built: [`crate::builtin`] registers each
//! plugin's rules and puts its links into a `plugins` detour of the
//! default posting chain, the mail handler registry takes its handlers
//! and pipelines, and the archive runner its archivers. Nothing is loaded
//! at run time, so a plugin is reviewed, compiled and shipped like the
//! rest of the program (Mailman's Python plugins have no counterpart).
use crate::chain::{Link, Rule};
use crate::handlers::{Handler, Pipeline};
use std::fmt;
use std::sync::OnceLock;

/// Something that keeps a copy of an archived post outside this site,
/// switched on per list by its name like the built-in archivers.
pub trait Archiver: Send + Sync {
    fn name(&self) -> &'static str;
    /// Keep `raw`, the archived copy of the post `hash` on `list_id`.
    /// # Errors
    /// The archiver's own reason, which is logged; the post is archived
    /// here regardless.
    fn archive(&self, list_id: &str, hash: &str, raw: &[u8]) -> Result<(), String>;
}

/// What a plugin adds. Every part is optional.
pub trait Plugin: Send + Sync {
    fn name(&self) -> &'static str;
    fn version(&self) -> &'static str;
    /// Rules to register; a rule named like a built-in replaces it.
    fn rules(&self) -> Vec<Box<dyn Rule>> {
        Vec::new()
    }
    /// Links appended, in order, to the `plugins` detour the default
    /// posting chain takes after its ban check and before moderation.
    fn links(&self) -> Vec<Link> {
        Vec::new()
    }
    /// Handlers to register; a handler named like a built-in replaces it.
    fn handlers(&self) -> Vec<Box<dyn Handler>> {
        Vec::new()
    }
    /// Pipelines a list may choose as its `posting_pipeline`.
    fn pipelines(&self) -> Vec<Pipeline> {
        Vec::new()
    }
    fn archivers(&self) -> Vec<Box<dyn Archiver>> {
        Vec::new()
    }
}

static INSTALLED: OnceLock<Vec<Box<dyn Plugin>>> = OnceLock::new();

/// Plugins were installed already, or a registry was built first and
/// sealed the set as empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlreadyInstalled;

impl fmt::Display for AlreadyInstalled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("plugins are already installed")
    }
}

impl std::error::Error for AlreadyInstalled {}

/// Install the build's plugins, once, before any registry is built.
/// # Errors
/// [`AlreadyInstalled`] when called twice, or after a registry sealed
/// the set.
pub fn install(plugins: Vec<Box<dyn Plugin>>) -> Result<(), AlreadyInstalled> {
    INSTALLED.set(plugins).map_err(|_| AlreadyInstalled)
}

/// The installed plugins. Asking before [`install`] seals the set as
/// empty, which is what a registry built first does.
#[must_use]
pub fn installed() -> &'static [Box<dyn Plugin>] {
    INSTALLED.get_or_init(Vec::new)
}

/// What one plugin adds, by name, for `GET /plugins` and the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Description {
    pub name: &'static str,
    pub version: &'static str,
    pub rules: Vec<&'static str>,
    pub links: Vec<&'static str>,
    pub handlers: Vec<&'static str>,
    pub pipelines: Vec<&'static str>,
    pub archivers: Vec<&'static str>,
}

/// Every installed plugin, described.
#[must_use]
pub fn describe() -> Vec<Description> {
    installed()
        .iter()
        .map(|plugin| Description {
            name: plugin.name(),
            version: plugin.version(),
            rules: plugin.rules().iter().map(|rule| rule.name()).collect(),
            links: plugin.links().iter().map(Link::rule).collect(),
            handlers: plugin
                .handlers()
                .iter()
                .map(|handler| handler.name())
                .collect(),
            pipelines: plugin.pipelines().iter().map(Pipeline::name).collect(),
            archivers: plugin
                .archivers()
                .iter()
                .map(|archiver| archiver.name())
                .collect(),
        })
        .collect()
}
