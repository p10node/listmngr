//! `listmngr secrets`: the secrets the database keeps under the master key
//! — what is sealed, sealing what is not, moving everything to a new key,
//! and minting a key. Key material never reaches stdout except from
//! `new-key`, whose only output it is.
use anyhow::Result;
use clap::Subcommand;
use listmngr_core::Config;
use listmngr_db::{AuditContext, Database, keyring::MasterKey};
use serde_json::json;
use std::path::PathBuf;

#[derive(Debug, Subcommand)]
pub enum Command {
    /// How many TOTP secrets are sealed and how many are in the clear, and
    /// whether a master key is configured.
    Status,
    /// Seal every TOTP secret still in the clear under `security.master_key`.
    Encrypt,
    /// Open every sealed secret with the previous key and seal it under the
    /// current one; run after changing `security.master_key`.
    Rewrap {
        /// The file holding the previous key (64 hexadecimal digits).
        #[arg(long)]
        previous_key_file: PathBuf,
    },
    /// Print a new random master key (64 hexadecimal digits) and nothing else.
    NewKey,
}

pub async fn run(db: &Database, config: &Config, command: Command) -> Result<()> {
    let context = AuditContext::system();
    match command {
        Command::Status => {
            let state = db.secrets().status().await?;
            println!(
                "{}",
                json!({
                    "master_key": config.security.master_key.is_some(),
                    "totp_sealed": state.totp_sealed,
                    "totp_plain": state.totp_plain,
                })
            );
        }
        Command::Encrypt => {
            let sealed = db.secrets().encrypt(&context).await?;
            println!("{}", json!({ "sealed": sealed }));
        }
        Command::Rewrap { previous_key_file } => {
            let hex = listmngr_core::read_secret_file(&previous_key_file, "previous key file")?;
            let previous = MasterKey::from_hex(&hex)?;
            let rewrapped = db.secrets().rewrap(&previous, &context).await?;
            println!("{}", json!({ "rewrapped": rewrapped }));
        }
        Command::NewKey => {
            let hex = MasterKey::generate_hex()?;
            println!("{}", hex.as_str());
        }
    }
    Ok(())
}
