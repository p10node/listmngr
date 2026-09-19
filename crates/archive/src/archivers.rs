//! Remote archivers: what a list does with a post besides keeping it in
//! the local archive.
//!
//! Three are offered, the set Mailman's `archivers` parity names.
//! `prototype` drops the archived copy into a maildir under an operator's
//! directory, one file per post named by its Message-ID-Hash, so a
//! replay overwrites rather than duplicates. `mhonarc` pipes the same
//! bytes to an operator-configured command, given as an argument vector
//! rather than a shell line so nothing in a message can become a shell
//! word; `$listname`, `$hostname` and `$hash` are substituted in each
//! argument. `mail-archive` is not here: sending a public list's copy to
//! the service is a durable write, so `ArchiveRepo::complete` queues it
//! with the archived post in one transaction.
//!
//! Both archivers here run after the post is stored, so a crash between
//! the two loses a forward rather than the archive. Neither failure fails
//! the queue job: the local archive is the record.
use listmngr_core::{Error, ListId, Result};
use listmngr_db::Database;
use std::path::{Path, PathBuf};

/// What the operator configured for the archivers that act outside the
/// database. An empty field switches its archiver off whatever the list
/// says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    /// `[archive] archivers.mhonarc_command`: argv, not a shell line.
    pub mhonarc: Vec<String>,
    /// `[archive] archivers.prototype_path`: the maildir's root.
    pub prototype: String,
}

impl Settings {
    /// Whether any archiver here could run at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.mhonarc.is_empty() && self.prototype.trim().is_empty()
    }
}

/// Run the list's enabled archivers over one archived post.
///
/// Returns the names that ran, in the order they ran. A post that is not
/// archived — an archive policy of `never`, or one hidden since —
/// forwards nothing.
/// # Errors
/// Database failures. A file or command failure is logged and reported
/// as an archiver that did not run, never as an error: the archived post
/// is the record and the queue job is already done.
pub async fn run(
    db: &Database,
    list: &ListId,
    hash: &str,
    settings: &Settings,
) -> Result<Vec<&'static str>> {
    if settings.is_empty() {
        return Ok(Vec::new());
    }
    let enabled = db.lists().archivers(list).await?;
    let on = |name: &str| enabled.iter().any(|(stored, on)| stored == name && *on);
    if !on("mhonarc") && !on("prototype") {
        return Ok(Vec::new());
    }
    let Some(raw) = db.archive().archived_copy(list, hash).await? else {
        return Ok(Vec::new());
    };
    let mut names = Vec::new();
    if on("mhonarc") && !settings.mhonarc.is_empty() {
        match mhonarc(&settings.mhonarc, list, hash, &raw).await {
            Ok(()) => names.push("mhonarc"),
            Err(error) => tracing::warn!(%error, list=%list, "mhonarc archiver failed"),
        }
    }
    if on("prototype") && !settings.prototype.trim().is_empty() {
        match prototype(Path::new(settings.prototype.trim()), list, hash, &raw).await {
            Ok(()) => names.push("prototype"),
            Err(error) => tracing::warn!(%error, list=%list, "prototype archiver failed"),
        }
    }
    Ok(names)
}

/// `$listname`, `$hostname` and `$hash` in one command argument.
fn expand(argument: &str, list: &ListId, hash: &str) -> String {
    argument
        .replace("$listname", list.as_str())
        .replace("$hostname", list.mail_host())
        .replace("$hash", hash)
}

/// Pipe the archived copy to the configured command and wait for it.
async fn mhonarc(command: &[String], list: &ListId, hash: &str, raw: &[u8]) -> Result<()> {
    let (program, arguments) = command
        .split_first()
        .ok_or_else(|| Error::Validation("mhonarc command".into()))?;
    let mut child = tokio::process::Command::new(expand(program, list, hash))
        .args(arguments.iter().map(|a| expand(a, list, hash)))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| Error::Validation(format!("mhonarc: {error}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt as _;
        stdin
            .write_all(raw)
            .await
            .map_err(|error| Error::Validation(format!("mhonarc: {error}")))?;
        stdin
            .shutdown()
            .await
            .map_err(|error| Error::Validation(format!("mhonarc: {error}")))?;
    }
    let status = child
        .wait()
        .await
        .map_err(|error| Error::Validation(format!("mhonarc: {error}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::Validation(format!("mhonarc exited with {status}")))
    }
}

/// Drop the archived copy into `<root>/<list>/new/<hash>`, written under
/// `tmp/` first so a reader never sees half a message.
async fn prototype(root: &Path, list: &ListId, hash: &str, raw: &[u8]) -> Result<()> {
    let list_dir: PathBuf = root.join(list.as_str());
    let (tmp, new) = (list_dir.join("tmp"), list_dir.join("new"));
    let io = |error: std::io::Error| Error::Validation(format!("prototype: {error}"));
    for directory in [&tmp, &new] {
        tokio::fs::create_dir_all(directory).await.map_err(io)?;
    }
    let staged = tmp.join(hash);
    tokio::fs::write(&staged, raw).await.map_err(io)?;
    tokio::fs::rename(&staged, new.join(hash)).await.map_err(io)
}
