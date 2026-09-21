//! Migration from Mailman: `import21` reads a Mailman 2.1 list's
//! `config.pck` and applies it to a list here the way Mailman 3's own
//! `import21` does (`mailman/utilities/importer.py`).
pub mod config21;
pub mod import21;
pub mod pickle;

pub use config21::Config21;
pub use import21::{ImportReport, Plan, apply, plan};

/// What can go wrong reading or applying a Mailman 2.1 configuration.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a Mailman 2.1 config.pck: {0}")]
    Pickle(String),
    #[error(transparent)]
    Core(#[from] listmngr_core::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
