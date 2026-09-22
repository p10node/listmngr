//! Migration from Mailman.
//!
//! `import21` reads a Mailman 2.1 list's `config.pck` and applies it the
//! way Mailman 3's own importer does (`mailman/utilities/importer.py`);
//! `import3` reads a whole Mailman 3 site over its REST API (`rest3`) or
//! straight from its database and message store (`db3`) and applies it
//! here.
pub mod config21;
pub mod db3;
pub mod import21;
pub mod import3;
pub mod pickle;
pub mod rest3;

pub use config21::Config21;
pub use import21::{ImportReport, Plan, apply, plan};

/// What can go wrong reading or applying a Mailman site.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a Mailman 2.1 config.pck: {0}")]
    Pickle(String),
    #[error("the Mailman 3 core could not be read: {0}")]
    Rest(String),
    #[error("the Mailman 3 database could not be read: {0}")]
    Database(String),
    #[error(transparent)]
    Core(#[from] listmngr_core::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
