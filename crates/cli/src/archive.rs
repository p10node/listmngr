//! `listmngr archive`: the search index's rebuild. The archive runner keeps
//! the index current while `serve` runs; this command rebuilds it from
//! every archived post, for a fresh deployment, a lost index or a schema
//! change. It needs the index's write lock, so `serve` must not be holding
//! it.
use anyhow::{Context as _, Result};
use clap::Subcommand;
use listmngr_archive::search::SearchIndex;
use listmngr_core::Config;
use listmngr_db::Database;
use std::path::{Path, PathBuf};

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Rebuild the search index from every archived post and print how
    /// many were indexed. Uses `[archive] index_path` unless overridden.
    Reindex {
        /// Where the index lives; created when missing.
        #[arg(long)]
        index: Option<PathBuf>,
    },
}

pub async fn run(db: &Database, config: &Config, command: Command) -> Result<()> {
    match command {
        Command::Reindex { index } => {
            let path = index.unwrap_or_else(|| PathBuf::from(&config.archive.index_path));
            let index = open(&path)?;
            let count = listmngr_archive::reindex(db, &index)
                .await
                .context("rebuilding the search index")?;
            println!("indexed {count} messages into {}", path.display());
        }
    }
    Ok(())
}

fn open(path: &Path) -> Result<SearchIndex> {
    SearchIndex::open(path)
        .with_context(|| format!("opening the search index at {}", path.display()))
}
