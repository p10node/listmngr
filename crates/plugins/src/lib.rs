#![forbid(unsafe_code)]

//! The plugins this build links in.
//!
//! A plugin is a Rust type implementing [`Plugin`], listed in [`all`] and
//! installed once by the binary at start ([`install`]); the registries
//! take its parts when they are first built. A build carries only the
//! plugins its features name — none by default — so `GET /plugins` and
//! `listmngr plugins` describe exactly what this binary can do.
pub use listmngr_pipeline::plugins::{
    AlreadyInstalled, Archiver, Description, Plugin, describe, installed,
};

#[cfg(feature = "example")]
pub mod example;

/// Every plugin this build carries, in installation order.
#[must_use]
pub fn all() -> Vec<Box<dyn Plugin>> {
    #[cfg(feature = "example")]
    {
        vec![Box::new(example::Example) as Box<dyn Plugin>]
    }
    #[cfg(not(feature = "example"))]
    {
        Vec::new()
    }
}

/// Install [`all`], once, before any registry is built.
/// # Errors
/// [`AlreadyInstalled`] the second time, or after a registry was built.
pub fn install() -> Result<(), AlreadyInstalled> {
    listmngr_pipeline::plugins::install(all())
}
