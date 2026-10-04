//! `listmngr dkim`: keys for outbound signing — generate one and print its
//! DNS record, print the records of every configured key, and compare
//! what DNS publishes with what the keys say. Private material never
//! reaches stdout.
use anyhow::{Context as _, Result};
use clap::{Subcommand, ValueEnum};
use listmngr_core::{Config, DkimSigningConfig};
use listmngr_mail::dkim::{Algorithm, SigningKeys, generate_key};
use serde_json::json;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum KeyAlgorithm {
    /// `rsa-sha256`, verified everywhere; 2048 bits unless `--bits` says more.
    Rsa,
    /// `ed25519-sha256` (RFC 8463): small records, not yet verified everywhere.
    Ed25519,
}

impl From<KeyAlgorithm> for Algorithm {
    fn from(algorithm: KeyAlgorithm) -> Self {
        match algorithm {
            KeyAlgorithm::Rsa => Self::Rsa,
            KeyAlgorithm::Ed25519 => Self::Ed25519,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Generate a private key into a new file readable by nobody else and
    /// print the DNS record to publish; the key itself is never printed.
    Gen {
        #[arg(long)]
        domain: String,
        #[arg(long)]
        selector: String,
        #[arg(long, value_enum, default_value_t = KeyAlgorithm::Rsa)]
        algorithm: KeyAlgorithm,
        /// RSA key size, 2048 to 4096.
        #[arg(long, default_value_t = 2048)]
        bits: u32,
        /// Where to write the PKCS#8 PEM; must not exist.
        #[arg(long)]
        out: PathBuf,
    },
    /// The DNS TXT record of every configured `[[mta.dkim_signing]]` key.
    Records,
    /// Look each configured record up in DNS and compare it with the key:
    /// `ok`, `missing`, `mismatch` or `error` per record; exit 12 unless
    /// every one is `ok`.
    Dns {
        /// Query this DNS server (IP:port) instead of the system resolver.
        #[arg(long)]
        dns_server: Option<std::net::SocketAddr>,
    },
}

fn record_json(entry: &DkimSigningConfig) -> Result<serde_json::Value> {
    let (name, txt) = SigningKeys::dns_record(entry)?;
    let algorithm = txt
        .split(';')
        .find_map(|tag| tag.trim().strip_prefix("k="))
        .unwrap_or_default()
        .to_owned();
    Ok(json!({
        "domain": entry.domain,
        "selector": entry.selector,
        "algorithm": algorithm,
        "file": entry.private_key_file,
        "name": name,
        "txt": txt,
    }))
}

fn write_new_private_file(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()
}

fn generate(
    domain: String,
    selector: String,
    algorithm: KeyAlgorithm,
    bits: u32,
    out: PathBuf,
) -> Result<()> {
    let pem = generate_key(algorithm.into(), bits)?;
    write_new_private_file(&out, &pem)
        .with_context(|| format!("cannot create {}", out.display()))?;
    let entry = DkimSigningConfig {
        domain,
        selector,
        private_key_file: out,
    };
    println!("{}", record_json(&entry)?);
    Ok(())
}

/// The `p=` tag of a record, without the whitespace a long record may fold.
fn public_key_tag(txt: &str) -> Option<String> {
    txt.split(';')
        .find_map(|tag| tag.trim().strip_prefix("p="))
        .map(|p| p.chars().filter(|c| !c.is_whitespace()).collect())
}

/// The TXT strings published at `name`, each record's chunks joined.
fn published_txt(records: &hickory_resolver::lookup::Lookup) -> Vec<String> {
    use hickory_resolver::proto::rr::RData;
    records
        .answers()
        .iter()
        .filter_map(|record| match &record.data {
            RData::TXT(txt) => Some(
                txt.txt_data
                    .iter()
                    .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect()
}

async fn dns(config: &Config, server: Option<std::net::SocketAddr>) -> Result<()> {
    use hickory_resolver::{
        net::{DnsError, NetError},
        proto::rr::Name,
    };
    let resolver = crate::doctor::dns::resolver(server)
        .map_err(|()| anyhow::anyhow!("resolver unavailable"))?;
    let mut all_ok = true;
    for entry in &config.mta.dkim_signing {
        let (name, txt) = SigningKeys::dns_record(entry)?;
        let expected = public_key_tag(&txt).unwrap_or_default();
        let fqdn = Name::from_ascii(format!("{name}."))?;
        let (status, detail) = match resolver.txt_lookup(fqdn).await {
            Ok(records) => {
                let published = published_txt(&records);
                if published
                    .iter()
                    .any(|record| public_key_tag(record).as_deref() == Some(expected.as_str()))
                {
                    ("ok", "public key published".to_owned())
                } else if published.is_empty() {
                    ("missing", "no TXT record".to_owned())
                } else {
                    (
                        "mismatch",
                        format!("{} TXT record(s), none with this key", published.len()),
                    )
                }
            }
            Err(NetError::Dns(DnsError::NoRecordsFound(_))) => {
                ("missing", "no TXT record".to_owned())
            }
            Err(error) => ("error", error.to_string()),
        };
        all_ok &= status == "ok";
        println!(
            "{}",
            json!({"domain": entry.domain, "selector": entry.selector, "name": name, "status": status, "detail": detail})
        );
    }
    if all_ok {
        Ok(())
    } else {
        Err(crate::doctor::Failure.into())
    }
}

pub async fn run(config: &Config, command: Command) -> Result<()> {
    match command {
        Command::Gen {
            domain,
            selector,
            algorithm,
            bits,
            out,
        } => generate(domain, selector, algorithm, bits, out),
        Command::Records => {
            for entry in &config.mta.dkim_signing {
                println!("{}", record_json(entry)?);
            }
            Ok(())
        }
        Command::Dns { dns_server } => dns(config, dns_server).await,
    }
}
