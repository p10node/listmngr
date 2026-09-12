//! Explicit, non-activating Postfix map publication. Each run publishes a new
//! generation; operators select all three files from the same generation.
use anyhow::Result;
use clap::Subcommand;
use listmngr_core::{Config, Error};
use listmngr_db::Database;
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io::Write as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Write exact regexp maps into a fresh generation and print its directory.
    /// Does not change Postfix configuration, run postmap, or reload a service.
    Regen {
        /// Operator-owned parent directory for immutable map generations.
        #[arg(long)]
        output: PathBuf,
        /// Concrete destination as seen by Postfix; defaults to `mta.lmtp_listen`.
        #[arg(long)]
        lmtp_target: Option<SocketAddr>,
    },
}

pub async fn run(db: &Database, config: &Config, command: Command) -> Result<()> {
    let Command::Regen {
        output,
        lmtp_target,
    } = command;
    let target: SocketAddr = match lmtp_target {
        Some(target) => target,
        None => config
            .mta
            .lmtp_listen
            .parse()
            .map_err(|_| Error::Validation("invalid LMTP map target".into()))?,
    };
    if target.port() == 0
        || target.ip().is_unspecified()
        || target.ip().is_multicast()
        || matches!(target, SocketAddr::V6(address) if address.scope_id() != 0)
    {
        return Err(
            Error::Validation("LMTP map target must be a concrete destination".into()).into(),
        );
    }
    // One SELECT supplies the identity snapshot for all three maps, including
    // unadvertised lists. No business/audit write or migration is performed.
    let lists = db.lists().list(None).await?;
    let mut addresses = BTreeSet::new();
    let mut hosts = BTreeSet::new();
    let mut verp = BTreeSet::new();
    for list in lists {
        hosts.insert(list.id.mail_host().to_owned());
        addresses.insert(list.id.posting_address());
        for (suffix, _) in listmngr_runners::COMMAND_SUFFIXES {
            addresses.insert(list.id.address_with_suffix(suffix));
        }
        // VERP bounces: `list-bounces<delimiter>local=domain@host`.
        verp.insert(format!(
            "{}-bounces{}[^@=]+=[^@=]+@{}",
            regexp_escape(list.id.list_name()),
            regexp_escape(&config.mta.verp_delimiter),
            regexp_escape(list.id.mail_host())
        ));
    }
    let transport = format!("lmtp:[{}]:{}", target.ip(), target.port());
    let maps = [
        ("domains.regexp", render(&hosts, "OK")),
        (
            "recipients.regexp",
            render(&addresses, "OK") + &render_patterns(&verp, "OK"),
        ),
        (
            "transport.regexp",
            render(&addresses, &transport) + &render_patterns(&verp, &transport),
        ),
    ];
    let generation = publish(&output, &maps)?;
    println!("{}", generation.display());
    Ok(())
}

fn render(keys: &BTreeSet<String>, value: &str) -> String {
    let mut result = String::new();
    for key in keys {
        // ListId permits only ASCII alphanumeric/hyphen list names and DNS
        // domain labels. The dot is the only regexp metacharacter in these keys.
        writeln!(result, "/^{}$/ {value}", key.replace('.', "\\.")).expect("String write");
    }
    result
}

/// Escape the regular-expression metacharacters for a Postfix `regexp:` key.
fn regexp_escape(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            if ".^$*+?()[]{}|\\/".contains(c) {
                vec!['\\', c]
            } else {
                vec![c]
            }
        })
        .collect()
}

/// Rows whose keys are already regular expressions.
fn render_patterns(patterns: &BTreeSet<String>, value: &str) -> String {
    let mut result = String::new();
    for pattern in patterns {
        writeln!(result, "/^{pattern}$/ {value}").expect("String write");
    }
    result
}

fn publish(output: &Path, maps: &[(&str, String)]) -> Result<PathBuf> {
    std::fs::create_dir_all(output)?;
    let output = std::fs::canonicalize(output)?;
    let staging = tempfile::Builder::new()
        .prefix(".staging-")
        .tempdir_in(&output)?;
    for (name, content) in maps {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(staging.path().join(name))?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
    }
    // tempfile's default private directory is deliberate: unadvertised list
    // identities are not made world-readable. Operators grant their MTA access.
    std::fs::File::open(staging.path())?.sync_all()?;
    let destination = output.join(format!("generation-{}", uuid::Uuid::now_v7()));
    std::fs::rename(staging.path(), &destination)?;
    std::fs::File::open(&output)?.sync_all()?;
    Ok(destination)
}
