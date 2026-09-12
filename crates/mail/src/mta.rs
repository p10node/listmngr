//! Mailman's `IMailTransportAgentAliases`: the lookup data a front MTA needs
//! to accept list mail and hand it to LMTP.
//!
//! Postfix reads either `regexp:` files (no compile step) or Mailman's
//! `hash:` files compiled by `postmap`; Exim reads `lsearch` files. Every
//! run publishes a complete, immutable `generation-<uuid>` directory and
//! then switches the `current` symlink, so an MTA never sees a partial map
//! and old generations stay available for rollback until pruned.
use listmngr_core::{Error, ListId, MtaConfig};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io::Write as _;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

/// Address suffix → command for every list.
///
/// Map producers and the LMTP dispatcher share this contract; token-bearing
/// plus extensions other than VERP bounces remain unsupported. The
/// historical constant name is retained.
pub const COMMAND_SUFFIXES: &[(&str, &str)] = &[
    ("owner", "owner"),
    ("bounces", "bounces"),
    ("join", "join"),
    ("subscribe", "join"),
    ("leave", "leave"),
    ("unsubscribe", "leave"),
    ("confirm", "confirm"),
    ("request", "request"),
];

/// The front MTA whose lookup format is written (`[mta] incoming`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mta {
    Postfix,
    Exim,
}

impl std::str::FromStr for Mta {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self, Error> {
        match value {
            "postfix" => Ok(Self::Postfix),
            "exim" => Ok(Self::Exim),
            _ => Err(Error::Validation(
                "mta.incoming must be \"none\", \"postfix\" or \"exim\"".into(),
            )),
        }
    }
}

/// Mailman's `[postfix] transport_file_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostfixMapType {
    /// Anchored `regexp:` rows; read directly, VERP bounces matched by pattern.
    Regexp,
    /// `hash:` rows compiled with `postmap`; VERP relies on `recipient_delimiter`.
    Hash,
}

impl std::str::FromStr for PostfixMapType {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self, Error> {
        match value {
            "regex" => Ok(Self::Regexp),
            "hash" => Ok(Self::Hash),
            _ => Err(Error::Validation(
                "mta.transport_file_type must be \"regex\" or \"hash\"".into(),
            )),
        }
    }
}

/// Who may read a published generation (`[mta] map_permissions`). Hidden
/// list identities are in these files, so `world` is a deliberate choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readers {
    Owner,
    Group,
    World,
}

impl Readers {
    const fn modes(self) -> (u32, u32) {
        match self {
            Self::Owner => (0o700, 0o600),
            Self::Group => (0o750, 0o640),
            Self::World => (0o755, 0o644),
        }
    }
}

impl std::str::FromStr for Readers {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self, Error> {
        match value {
            "owner" => Ok(Self::Owner),
            "group" => Ok(Self::Group),
            "world" => Ok(Self::World),
            _ => Err(Error::Validation(
                "mta.map_permissions must be \"owner\", \"group\" or \"world\"".into(),
            )),
        }
    }
}

/// Where the MTA reaches the LMTP listener: a concrete IP literal or a DNS
/// name (Compose service names included) with a port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LmtpTarget {
    host: String,
    port: u16,
}

impl LmtpTarget {
    /// Parse `host:port`, `ip:port` or `[ipv6]:port`.
    /// # Errors
    /// Rejects wildcard, multicast and scoped addresses, port zero, and
    /// anything that is neither an IP literal nor a hostname.
    pub fn parse(text: &str) -> Result<Self, Error> {
        let invalid = || Error::Validation("LMTP map target must be a concrete destination".into());
        if let Ok(address) = text.parse::<std::net::SocketAddr>() {
            let ip = address.ip();
            if address.port() == 0
                || ip.is_unspecified()
                || ip.is_multicast()
                || matches!(address, std::net::SocketAddr::V6(v6) if v6.scope_id() != 0)
            {
                return Err(invalid());
            }
            return Ok(Self {
                host: ip.to_string(),
                port: address.port(),
            });
        }
        let (host, port) = text.rsplit_once(':').ok_or_else(invalid)?;
        let port: u16 = port.parse().map_err(|_| invalid())?;
        if port == 0 || host.parse::<IpAddr>().is_ok() || !is_hostname(host) {
            return Err(invalid());
        }
        Ok(Self {
            host: host.to_owned(),
            port,
        })
    }

    /// The Postfix transport row value: brackets suppress MX lookups.
    #[must_use]
    pub fn postfix_transport(&self) -> String {
        format!("lmtp:[{}]:{}", self.host, self.port)
    }
}

fn is_hostname(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

/// Renders and publishes map generations for one MTA.
#[derive(Debug, Clone)]
pub struct MapWriter {
    pub mta: Mta,
    /// Operator-owned parent of the generations and the `current` symlink.
    pub directory: PathBuf,
    pub lmtp_target: LmtpTarget,
    pub map_type: PostfixMapType,
    pub postmap_command: PathBuf,
    pub verp_delimiter: String,
    pub readers: Readers,
    /// Generations kept after a publish, the new one included.
    pub keep: usize,
}

impl MapWriter {
    /// The writer `[mta]` configures, or `None` when `incoming = "none"`.
    /// # Errors
    /// Returns the first invalid `[mta]` map setting.
    pub fn from_config(config: &MtaConfig) -> Result<Option<Self>, Error> {
        if config.incoming == "none" {
            return Ok(None);
        }
        let mta: Mta = config.incoming.parse()?;
        let lmtp_target = match &config.lmtp_map_target {
            Some(target) => LmtpTarget::parse(target)?,
            None => LmtpTarget::parse(&config.lmtp_listen).map_err(|_| {
                Error::Validation(
                    "mta.lmtp_map_target is required when mta.lmtp_listen is not a concrete destination".into(),
                )
            })?,
        };
        if !(1..=100).contains(&config.map_generations_kept) {
            return Err(Error::Validation(
                "mta.map_generations_kept must be 1..100".into(),
            ));
        }
        Ok(Some(Self {
            mta,
            directory: PathBuf::from(&config.map_directory),
            lmtp_target,
            map_type: config.transport_file_type.parse()?,
            postmap_command: PathBuf::from(&config.postmap_command),
            verp_delimiter: config.verp_delimiter.clone(),
            readers: config.map_permissions.parse()?,
            keep: config.map_generations_kept as usize,
        }))
    }

    /// The files of one generation as `(name, content)`, sorted and
    /// deduplicated from the list identities alone.
    #[must_use]
    pub fn render(&self, lists: &[ListId]) -> Vec<(String, String)> {
        let mut addresses = BTreeSet::new();
        let mut hosts = BTreeSet::new();
        let mut verp = BTreeSet::new();
        for list in lists {
            hosts.insert(list.mail_host().to_owned());
            addresses.insert(list.posting_address());
            for (suffix, _) in COMMAND_SUFFIXES {
                addresses.insert(list.address_with_suffix(suffix));
            }
            // VERP bounces: `list-bounces<delimiter>local=domain@host`.
            verp.insert(format!(
                "{}-bounces{}[^@=]+=[^@=]+@{}",
                regexp_escape(list.list_name()),
                regexp_escape(&self.verp_delimiter),
                regexp_escape(list.mail_host())
            ));
        }
        let transport = self.lmtp_target.postfix_transport();
        match (self.mta, self.map_type) {
            (Mta::Postfix, PostfixMapType::Regexp) => vec![
                ("domains.regexp".into(), regexp_rows(&hosts, "OK")),
                (
                    "recipients.regexp".into(),
                    regexp_rows(&addresses, "OK") + &pattern_rows(&verp, "OK"),
                ),
                (
                    "transport.regexp".into(),
                    regexp_rows(&addresses, &transport) + &pattern_rows(&verp, &transport),
                ),
            ],
            (Mta::Postfix, PostfixMapType::Hash) => vec![
                (
                    "postfix_domains".into(),
                    hosts.iter().fold(String::new(), |mut out, host| {
                        writeln!(out, "{host} {host}").expect("String write");
                        out
                    }),
                ),
                (
                    "postfix_lmtp".into(),
                    addresses.iter().fold(String::new(), |mut out, address| {
                        writeln!(out, "{address} {transport}").expect("String write");
                        out
                    }),
                ),
            ],
            (Mta::Exim, _) => vec![
                ("exim_domains".into(), lines(&hosts)),
                ("exim_recipients".into(), lines(&addresses)),
            ],
        }
    }

    /// Publish a generation for `lists`, point `current` at it, compile hash
    /// maps, and prune generations beyond `keep`. Returns the generation path.
    /// # Errors
    /// Filesystem and `postmap` failures; a published generation is never
    /// replaced or unselected by a failed run.
    pub fn publish(&self, lists: &[ListId]) -> std::io::Result<PathBuf> {
        let files = self.render(lists);
        std::fs::create_dir_all(&self.directory)?;
        let directory = std::fs::canonicalize(&self.directory)?;
        let staging = tempfile::Builder::new()
            .prefix(".staging-")
            .tempdir_in(&directory)?;
        let (dir_mode, file_mode) = self.readers.modes();
        for (name, content) in &files {
            let path = staging.path().join(name);
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
            file.write_all(content.as_bytes())?;
            file.sync_all()?;
            set_mode(&path, file_mode)?;
        }
        if self.mta == Mta::Postfix && self.map_type == PostfixMapType::Hash {
            self.postmap(staging.path(), &files)?;
            for (name, _) in &files {
                set_mode(&staging.path().join(format!("{name}.db")), file_mode)?;
            }
        }
        set_mode(staging.path(), dir_mode)?;
        std::fs::File::open(staging.path())?.sync_all()?;
        let name = format!("generation-{}", uuid::Uuid::now_v7());
        let destination = directory.join(&name);
        std::fs::rename(staging.path(), &destination)?;
        select_current(&directory, &name)?;
        std::fs::File::open(&directory)?.sync_all()?;
        prune(&directory, &name, self.keep)?;
        Ok(destination)
    }

    fn postmap(&self, staging: &Path, files: &[(String, String)]) -> std::io::Result<()> {
        let mut command = std::process::Command::new(&self.postmap_command);
        for (name, _) in files {
            command.arg(staging.join(name));
        }
        let status = command
            .status()
            .map_err(|error| std::io::Error::other(format!("postmap could not run: {error}")))?;
        if !status.success() {
            return Err(std::io::Error::other(format!(
                "postmap exited with {status}"
            )));
        }
        Ok(())
    }
}

fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

/// Point `current` at `name` through a relative link swapped in atomically.
fn select_current(directory: &Path, name: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let temporary = directory.join(format!(".current-{name}"));
        let _ = std::fs::remove_file(&temporary);
        std::os::unix::fs::symlink(name, &temporary)?;
        std::fs::rename(&temporary, directory.join("current"))?;
    }
    #[cfg(not(unix))]
    let _ = (directory, name);
    Ok(())
}

/// Remove the oldest `generation-*` directories beyond `keep`, never the one
/// just published. `UUIDv7` names sort by creation time.
fn prune(directory: &Path, current: &str, keep: usize) -> std::io::Result<()> {
    let mut generations: Vec<String> = std::fs::read_dir(directory)?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with("generation-"))
        .collect();
    generations.sort();
    generations.reverse();
    for name in generations.iter().skip(keep.max(1)) {
        if name != current {
            std::fs::remove_dir_all(directory.join(name))?;
        }
    }
    Ok(())
}

fn regexp_rows(keys: &BTreeSet<String>, value: &str) -> String {
    let mut result = String::new();
    for key in keys {
        // ListId permits only ASCII alphanumeric/hyphen list names and DNS
        // domain labels. The dot is the only regexp metacharacter in these keys.
        writeln!(result, "/^{}$/ {value}", key.replace('.', "\\.")).expect("String write");
    }
    result
}

/// Rows whose keys are already regular expressions.
fn pattern_rows(patterns: &BTreeSet<String>, value: &str) -> String {
    let mut result = String::new();
    for pattern in patterns {
        writeln!(result, "/^{pattern}$/ {value}").expect("String write");
    }
    result
}

fn lines(keys: &BTreeSet<String>) -> String {
    keys.iter().fold(String::new(), |mut out, key| {
        out.push_str(key);
        out.push('\n');
        out
    })
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
