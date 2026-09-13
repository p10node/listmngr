//! Mailman's `mailman aliases`: explicit MTA map publication. Each run
//! publishes a new generation, switches the `current` symlink to it and
//! prints the generation directory; it never edits MTA configuration or
//! reloads a service.
use anyhow::{Context as _, Result};
use clap::Subcommand;
use listmngr_core::Config;
use listmngr_db::Database;
use listmngr_mail::mta::MapWriter;
use std::path::PathBuf;

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Publish a fresh map generation and print its directory. Uses `[mta]`
    /// (`incoming`, `map_directory`, `lmtp_map_target`, `transport_file_type`)
    /// unless overridden; with `incoming = "none"` Postfix maps are written.
    Regen {
        /// Operator-owned parent directory for map generations.
        #[arg(long)]
        output: Option<PathBuf>,
        /// The LMTP destination as the MTA sees it (`host:port`).
        #[arg(long)]
        lmtp_target: Option<String>,
        /// Which MTA's format to write: `postfix` or `exim`.
        #[arg(long)]
        mta: Option<String>,
    },
}

pub async fn run(db: &Database, config: &Config, command: Command) -> Result<()> {
    let Command::Regen {
        output,
        lmtp_target,
        mta,
    } = command;
    let mut settings = config.mta.clone();
    if settings.incoming == "none" {
        settings.incoming = "postfix".into();
    }
    if let Some(mta) = mta {
        settings.incoming = mta;
    }
    if let Some(target) = lmtp_target {
        settings.lmtp_map_target = Some(target);
    }
    let mut writer = MapWriter::from_config(&settings)?.expect("incoming is set");
    if let Some(output) = output {
        writer.directory = output;
    }
    let generation = regenerate(db, &writer).await?;
    println!("{}", generation.display());
    Ok(())
}

/// Publish the current list identities through `writer`. One SELECT, no
/// business or audit write.
pub async fn regenerate(db: &Database, writer: &MapWriter) -> Result<PathBuf> {
    let lists: Vec<listmngr_core::ListId> = db
        .lists()
        .list(None)
        .await?
        .into_iter()
        .map(|list| list.id)
        .collect();
    let writer = writer.clone();
    Ok(tokio::task::spawn_blocking(move || writer.publish(&lists)).await??)
}

/// Regenerate after a list came or went when `[mta] incoming` names an MTA.
/// A failure is reported, not fatal: the list change is already committed
/// and `aliases regen` repairs the maps.
pub async fn refresh_after_list_change(db: &Database, config: &Config) {
    let writer = match MapWriter::from_config(&config.mta) {
        Ok(Some(writer)) => writer,
        Ok(None) => return,
        Err(error) => {
            eprintln!("warning: MTA maps not regenerated: {error}");
            return;
        }
    };
    match regenerate(db, &writer).await {
        Ok(generation) => eprintln!("MTA maps regenerated: {}", generation.display()),
        Err(error) => {
            eprintln!("warning: MTA maps not regenerated: {error}; run `listmngr aliases regen`");
        }
    }
}

/// Publish at startup when `[mta] incoming` names an MTA, so a restored
/// database or a fresh deployment is routable at once. A map directory the
/// process cannot write is a startup failure.
pub async fn publish_at_startup(db: &Database, config: &Config) -> Result<()> {
    if let Some(writer) = MapWriter::from_config(&config.mta)? {
        let generation = regenerate(db, &writer)
            .await
            .context("MTA map generation failed")?;
        tracing::info!(generation = %generation.display(), "MTA maps published");
    }
    Ok(())
}
