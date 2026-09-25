//! Remote archivers: what a list does with a post besides keeping it in
//! the local archive.
//!
//! Mailman's `archivers` names, and one more: `hyperkitty` posts the
//! archived copy to a `HyperKitty` the way `mailman-hyperkitty` does.
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
    /// `[archive] archivers.hyperkitty_url`: the `HyperKitty` to post to.
    pub hyperkitty_url: String,
    /// Its `MAILMAN_ARCHIVER_KEY`, sent as `Authorization: Token`.
    pub hyperkitty_key: String,
}

impl Settings {
    /// Whether any archiver here could run at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.mhonarc.is_empty()
            && self.prototype.trim().is_empty()
            && self.hyperkitty_url.trim().is_empty()
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
    let plugins: Vec<Box<dyn listmngr_pipeline::plugins::Archiver>> =
        listmngr_pipeline::plugins::installed()
            .iter()
            .flat_map(|plugin| plugin.archivers())
            .collect();
    if settings.is_empty() && plugins.is_empty() {
        return Ok(Vec::new());
    }
    let enabled = db.lists().archivers(list).await?;
    let on = |name: &str| enabled.iter().any(|(stored, on)| stored == name && *on);
    if !on("mhonarc")
        && !on("prototype")
        && !on("hyperkitty")
        && !plugins.iter().any(|archiver| on(archiver.name()))
    {
        return Ok(Vec::new());
    }
    let Some(raw) = db.archive().archived_copy(list, hash).await? else {
        return Ok(Vec::new());
    };
    let mut names = run_builtin(settings, &enabled, list, hash, &raw).await;
    names.extend(run_plugins(plugins, &enabled, list, hash, &raw).await);
    Ok(names)
}

/// The built-in archivers a list switched on, each run and its failure
/// logged.
async fn run_builtin(
    settings: &Settings,
    enabled: &[(String, bool)],
    list: &ListId,
    hash: &str,
    raw: &[u8],
) -> Vec<&'static str> {
    let on = |name: &str| enabled.iter().any(|(stored, on)| stored == name && *on);
    let mut names = Vec::new();
    if on("mhonarc") && !settings.mhonarc.is_empty() {
        let result = mhonarc(&settings.mhonarc, list, hash, raw).await;
        names.extend(logged("mhonarc", list, result));
    }
    if on("prototype") && !settings.prototype.trim().is_empty() {
        let result = prototype(Path::new(settings.prototype.trim()), list, hash, raw).await;
        names.extend(logged("prototype", list, result));
    }
    if on("hyperkitty") && !settings.hyperkitty_url.trim().is_empty() {
        let result = hyperkitty(settings, list, hash, raw).await.map(|url| {
            tracing::info!(list=%list, hash, url, "hyperkitty archived the post");
        });
        names.extend(logged("hyperkitty", list, result));
    }
    names
}

/// The archiver's name when it ran, its failure logged when it did not.
fn logged(name: &'static str, list: &ListId, result: Result<()>) -> Option<&'static str> {
    match result {
        Ok(()) => Some(name),
        Err(error) => {
            tracing::warn!(%error, list=%list, archiver=name, "archiver failed");
            None
        }
    }
}

/// The plugins' archivers that are on for the list, each run off the
/// runtime's threads like a command would be; a failure is its own.
async fn run_plugins(
    plugins: Vec<Box<dyn listmngr_pipeline::plugins::Archiver>>,
    enabled: &[(String, bool)],
    list: &ListId,
    hash: &str,
    raw: &[u8],
) -> Vec<&'static str> {
    let mut names = Vec::new();
    for archiver in plugins {
        let name = archiver.name();
        if !enabled.iter().any(|(stored, on)| stored == name && *on) {
            continue;
        }
        let (list_id, hash, raw) = (list.to_string(), hash.to_owned(), raw.to_vec());
        match tokio::task::spawn_blocking(move || archiver.archive(&list_id, &hash, &raw)).await {
            Ok(Ok(())) => names.push(name),
            Ok(Err(error)) => {
                tracing::warn!(error, list=%list, archiver=name, "plugin archiver failed");
            }
            Err(error) => {
                tracing::warn!(%error, list=%list, archiver=name, "plugin archiver panicked");
            }
        }
    }
    names
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

/// Post the archived copy to `HyperKitty`'s `/api/mailman/archive` as
/// `mailman-hyperkitty` 1.2 does — `Authorization: Token <key>`, a
/// multipart form with `mlist` (the list's posting address) and the
/// message as a file — and return the permalink `HyperKitty` answers with.
async fn hyperkitty(settings: &Settings, list: &ListId, hash: &str, raw: &[u8]) -> Result<String> {
    use sha2::{Digest, Sha256};
    let base = settings.hyperkitty_url.trim().trim_end_matches('/');
    // A boundary that cannot occur in the message: a digest of it.
    let digest = Sha256::digest([raw, hash.as_bytes()].concat());
    let boundary = digest
        .iter()
        .take(16)
        .fold(String::from("listmngr-"), |mut out, byte| {
            use std::fmt::Write as _;
            let _ = write!(out, "{byte:02x}");
            out
        });
    let fqdn = format!("{}@{}", list.list_name(), list.mail_host());
    let mut body = Vec::with_capacity(raw.len() + 512);
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"mlist\"\r\n\r\n{fqdn}\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"message\"; filename=\"{hash}.eml\"\r\nContent-Type: message/rfc822\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(raw);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(30))
        .user_agent("listmngr")
        .build()
        .map_err(|error| Error::Validation(format!("hyperkitty client: {error}")))?;
    let response = client
        .post(format!("{base}/api/mailman/archive"))
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Token {}", settings.hyperkitty_key),
        )
        .header(
            reqwest::header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .await
        .map_err(|error| Error::Validation(format!("hyperkitty request: {error}")))?;
    let status = response.status();
    if !status.is_success() {
        return Err(Error::Validation(format!(
            "hyperkitty answered {}",
            status.as_u16()
        )));
    }
    let answer: serde_json::Value = response.json().await.unwrap_or_default();
    Ok(answer
        .get("url")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned())
}
