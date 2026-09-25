//! Mailman's `nntp` runner, the mail side: a post trimmed and addressed for
//! the news server (`prepare_message`) and an NNTP client that posts it
//! (RFC 3977 `MODE READER`, `AUTHINFO USER`/`PASS`, `POST`).
use crate::{Error, Result, cook};
use listmngr_core::{MailingList, NewsgroupModeration, NntpConfig};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// A header block's fields as (name as written, lowercase name, unfolded
/// value), each with its raw lines, so kept fields go out as they were.
struct Field {
    name: String,
    lower: String,
    value: String,
    raw: Vec<u8>,
}

fn fields(raw: &[u8], blank: usize) -> Result<Vec<Field>> {
    let mut fields: Vec<Field> = Vec::new();
    for line in raw[..blank].split_inclusive(|b| *b == b'\n') {
        let text = std::str::from_utf8(line).map_err(|_| Error::UnsafeHeaderContent)?;
        let text = text.trim_end_matches(['\r', '\n']);
        if text.starts_with([' ', '\t']) {
            let field = fields.last_mut().ok_or(Error::UnsafeHeaderContent)?;
            field.value.push(' ');
            field.value.push_str(text.trim());
            field.raw.extend_from_slice(line);
        } else {
            let (name, value) = text.split_once(':').ok_or(Error::UnsafeHeaderContent)?;
            if !listmngr_core::is_header_name(name) {
                return Err(Error::UnsafeHeaderContent);
            }
            fields.push(Field {
                name: name.to_owned(),
                lower: name.to_ascii_lowercase(),
                value: value.trim().to_owned(),
                raw: line.to_vec(),
            });
        }
    }
    Ok(fields)
}

/// The list's subject prefix taken off a subject, wherever it stands, as
/// Mailman's `stripped_subject` has it.
fn without_prefix(subject: &str, prefix: &str) -> String {
    let prefix = prefix.trim();
    if prefix.is_empty() {
        return subject.to_owned();
    }
    let lower = subject.to_lowercase();
    let needle = prefix.to_lowercase();
    match lower.find(&needle) {
        Some(at) if subject.is_char_boundary(at) && subject.is_char_boundary(at + needle.len()) => {
            let before = subject[..at].trim_end();
            let after = subject[at + needle.len()..].trim_start();
            if before.is_empty() {
                after.to_owned()
            } else if after.is_empty() {
                before.to_owned()
            } else {
                format!("{before} {after}")
            }
        }
        _ => subject.to_owned(),
    }
}

/// Mailman's `prepare_message`.
///
/// `Approved` for a moderated group, the subject with or without the
/// list's prefix, the `Newsgroups` header with the list's group, an
/// unfolded (or new) `Message-ID`, a `Lines` count, the transport headers
/// `[nntp] remove_headers` names removed, and the duplicates
/// `rewrite_duplicate_headers` names moved to their targets.
/// # Errors
/// Returns `UnsafeHeaderContent` for a header block that cannot be read.
pub fn prepare(raw: &[u8], list: &MailingList, config: &NntpConfig) -> Result<Vec<u8>> {
    let (blank, body) = cook::header_body_split(raw).ok_or(Error::UnsafeHeaderContent)?;
    let mut fields = fields(raw, blank)?;
    let crlf = raw[..body].contains(&b'\r');
    let eol: &[u8] = if crlf { b"\r\n" } else { b"\n" };
    let usenet = &list.usenet;
    let mut generated: Vec<(String, String)> = Vec::new();
    // Approved: so a moderated group's server posts instead of forwarding
    // to the group's moderators — the post was moderated here.
    if usenet.newsgroup_moderation != NewsgroupModeration::None {
        fields.retain(|field| field.lower != "approved");
        generated.push(("Approved".into(), list.id.posting_address()));
    }
    if !usenet.nntp_prefix_subject_too
        && let Some(subject) = fields.iter().find(|field| field.lower == "subject")
    {
        let stripped = without_prefix(&subject.value, &list.subject_prefix);
        fields.retain(|field| field.lower != "subject");
        generated.push(("Subject".into(), stripped));
    }
    if let Some(newsgroups) = newsgroups_header(&mut fields, &usenet.linked_newsgroup) {
        generated.push(("Newsgroups".into(), newsgroups));
    }
    if let Some(message_id) = message_id_header(&mut fields, list) {
        generated.push(("Message-ID".into(), message_id));
    }
    if !fields.iter().any(|field| field.lower == "lines") {
        let lines = raw[body..].split(|b| *b == b'\n').count()
            - usize::from(raw[body..].is_empty() || raw.ends_with(b"\n"));
        generated.push(("Lines".into(), lines.to_string()));
    }
    // Transport headers the news server would refuse or rewrite.
    let removed: Vec<String> = config
        .remove_headers
        .iter()
        .map(|name| name.to_ascii_lowercase())
        .collect();
    fields.retain(|field| !removed.contains(&field.lower));
    rewrite_duplicates(&mut fields, config, eol);
    let mut output = Vec::with_capacity(raw.len() + 128);
    for field in &fields {
        output.extend_from_slice(&field.raw);
    }
    for (name, value) in generated {
        if value.bytes().any(|b| b == b'\r' || b == b'\n') {
            return Err(Error::UnsafeHeaderContent);
        }
        output.extend_from_slice(name.as_bytes());
        output.extend_from_slice(b": ");
        output.extend_from_slice(value.as_bytes());
        output.extend_from_slice(eol);
    }
    output.extend_from_slice(&raw[blank..]);
    Ok(output)
}

/// The `Newsgroups` header to write: the list's group when the post has
/// none, or the poster's groups with the list's appended once (in which
/// case the original field is taken out).
fn newsgroups_header(fields: &mut Vec<Field>, newsgroup: &str) -> Option<String> {
    let Some(header) = fields.iter().find(|field| field.lower == "newsgroups") else {
        return Some(newsgroup.to_owned());
    };
    let mut groups: Vec<String> = header
        .value
        .split(',')
        .map(str::trim)
        .filter(|group| !group.is_empty())
        .map(str::to_owned)
        .collect();
    if groups.iter().any(|group| group == newsgroup) {
        return None;
    }
    groups.push(newsgroup.to_owned());
    fields.retain(|field| field.lower != "newsgroups");
    Some(groups.join(", "))
}

/// The `Message-ID` to write: the post's own unfolded when it was folded,
/// or the list's when the post had none; `None` when the post's stands.
fn message_id_header(fields: &mut Vec<Field>, list: &MailingList) -> Option<String> {
    let field = fields.iter().find(|field| field.lower == "message-id");
    let unfolded = field.map(|field| {
        field
            .value
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
    });
    match (field, unfolded) {
        (Some(field), Some(id)) if !id.is_empty() && field.value == id => None,
        (_, Some(id)) if !id.is_empty() => {
            fields.retain(|field| field.lower != "message-id");
            Some(id)
        }
        _ => {
            fields.retain(|field| field.lower != "message-id");
            Some(list_message_id(list))
        }
    }
}

/// A header the server accepts once: the first value stays, the rest move
/// to the configured target.
fn rewrite_duplicates(fields: &mut [Field], config: &NntpConfig, eol: &[u8]) {
    for (source, target) in config.duplicate_rewrites() {
        let mut seen = false;
        for field in fields.iter_mut() {
            if field.lower != source {
                continue;
            }
            if !seen {
                seen = true;
                continue;
            }
            field.lower = target.to_ascii_lowercase();
            field.name.clone_from(&target);
            let mut raw = field.name.as_bytes().to_vec();
            raw.extend_from_slice(b": ");
            raw.extend_from_slice(field.value.as_bytes());
            raw.extend_from_slice(eol);
            field.raw = raw;
        }
    }
}

/// The article with `message_id` (angle brackets included) as its only
/// `Message-ID`: Mailman's answer to a server that already has the
/// original id from a cross-post.
/// # Errors
/// Returns `UnsafeHeaderContent` for an unreadable header block or an id
/// with a line break.
pub fn with_message_id(raw: &[u8], message_id: &str) -> Result<Vec<u8>> {
    if message_id.bytes().any(|b| b == b'\r' || b == b'\n') {
        return Err(Error::UnsafeHeaderContent);
    }
    let (blank, body) = cook::header_body_split(raw).ok_or(Error::UnsafeHeaderContent)?;
    let fields = fields(raw, blank)?;
    let eol: &[u8] = if raw[..body].contains(&b'\r') {
        b"\r\n"
    } else {
        b"\n"
    };
    let mut output = Vec::with_capacity(raw.len() + 80);
    for field in fields.iter().filter(|field| field.lower != "message-id") {
        output.extend_from_slice(&field.raw);
    }
    output.extend_from_slice(b"Message-ID: ");
    output.extend_from_slice(message_id.as_bytes());
    output.extend_from_slice(eol);
    output.extend_from_slice(&raw[blank..]);
    Ok(output)
}

/// A `Message-ID` of the list's own, for a post that arrives without one or
/// whose id the news server already holds.
#[must_use]
pub fn list_message_id(list: &MailingList) -> String {
    format!(
        "<{}.{}@{}>",
        uuid::Uuid::now_v7().simple(),
        list.id.list_name(),
        list.id.mail_host()
    )
}

/// What the news server said to an article.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// `240`: the article is posted.
    Accepted,
    /// The server refused the article (`4xx`), with its reply: a duplicate
    /// `Message-ID`, a group that does not exist, posting not allowed.
    Refused(String),
}

/// An NNTP posting client for the configured news server.
#[derive(Debug, Clone)]
pub struct Client {
    host: String,
    port: u16,
    user: Option<String>,
    password: Option<String>,
    timeout: Duration,
}

fn io(message: &str) -> Error {
    Error::Io(std::io::Error::other(message.to_owned()))
}

impl Client {
    /// A client for `[nntp]`; the password comes from `password` or, when
    /// configured, its file (read now). An unreadable file leaves the
    /// client without credentials; [`Client::try_new`] reports it instead.
    #[must_use]
    pub fn new(config: &NntpConfig) -> Self {
        Self::try_new(config).unwrap_or_else(|_| Self {
            host: config.host.clone(),
            port: config.port,
            user: None,
            password: None,
            timeout: Duration::from_secs(30),
        })
    }

    /// [`Client::new`] that reports an unreadable password file.
    /// # Errors
    /// Returns the configuration error of the password file.
    pub fn try_new(config: &NntpConfig) -> Result<Self> {
        let credentials = config
            .credentials()
            .map_err(|error| io(&error.to_string()))?;
        Ok(Self {
            host: config.host.trim().to_owned(),
            port: config.port,
            user: credentials.as_ref().map(|(user, _)| user.clone()),
            password: credentials.map(|(_, password)| password.expose().to_owned()),
            timeout: Duration::from_secs(30),
        })
    }

    /// Post `article` (headers, blank line, body; LF or CRLF) and return
    /// the server's verdict.
    /// # Errors
    /// Returns an I/O error when the server cannot be reached, closes the
    /// connection, answers out of protocol, or refuses the credentials;
    /// the error never carries the password.
    pub async fn post(&self, article: &[u8]) -> Result<Outcome> {
        let session = async {
            let mut session = self.connect().await?;
            session
                .write
                .write_all(b"POST\r\n")
                .await
                .map_err(Error::Io)?;
            let go = reply(&mut session.read).await?;
            if !go.starts_with("340") {
                session.quit().await;
                return Ok(Outcome::Refused(go));
            }
            session
                .write
                .write_all(&dot_stuffed(article))
                .await
                .map_err(Error::Io)?;
            let verdict = reply(&mut session.read).await?;
            session.quit().await;
            if verdict.starts_with("240") {
                Ok(Outcome::Accepted)
            } else if verdict.starts_with('4') {
                Ok(Outcome::Refused(verdict))
            } else {
                Err(io(&format!(
                    "news server answered the article with {verdict}"
                )))
            }
        };
        tokio::time::timeout(self.timeout, session)
            .await
            .map_err(|_| io("news server timed out"))?
    }

    /// Open a reader session: `GROUP`, `HEAD` and `ARTICLE` until `quit`.
    /// # Errors
    /// As [`Client::post`] for the connection and the credentials.
    pub async fn reader(&self) -> Result<Reader> {
        let session = tokio::time::timeout(self.timeout, self.connect())
            .await
            .map_err(|_| io("news server timed out"))??;
        Ok(Reader {
            session,
            timeout: self.timeout,
        })
    }

    /// Connect, enter reader mode, authenticate when configured.
    async fn connect(&self) -> Result<Session> {
        let stream = TcpStream::connect((self.host.as_str(), self.port))
            .await
            .map_err(Error::Io)?;
        let (read, write) = stream.into_split();
        let mut session = Session {
            read: BufReader::new(read),
            write,
        };
        let greeting = reply(&mut session.read).await?;
        if !greeting.starts_with("20") {
            return Err(io(&format!("news server refused the session: {greeting}")));
        }
        session
            .write
            .write_all(b"MODE READER\r\n")
            .await
            .map_err(Error::Io)?;
        // A server that does not know reader mode still posts.
        let _ = reply(&mut session.read).await?;
        if let (Some(user), Some(password)) = (&self.user, &self.password) {
            session
                .write
                .write_all(format!("AUTHINFO USER {user}\r\n").as_bytes())
                .await
                .map_err(Error::Io)?;
            let wants_password = reply(&mut session.read).await?;
            if wants_password.starts_with("381") {
                session
                    .write
                    .write_all(format!("AUTHINFO PASS {password}\r\n").as_bytes())
                    .await
                    .map_err(Error::Io)?;
                let accepted = reply(&mut session.read).await?;
                if !accepted.starts_with("281") {
                    return Err(refused_credentials(&accepted));
                }
            } else if !wants_password.starts_with("281") {
                return Err(refused_credentials(&wants_password));
            }
        }
        Ok(session)
    }
}

fn refused_credentials(reply: &str) -> Error {
    io(&format!(
        "news server refused the credentials: {}",
        reply.split_whitespace().next().unwrap_or("")
    ))
}

/// An open connection in reader mode.
struct Session {
    read: BufReader<tokio::net::tcp::OwnedReadHalf>,
    write: tokio::net::tcp::OwnedWriteHalf,
}

impl Session {
    async fn quit(&mut self) {
        let _ = self.write.write_all(b"QUIT\r\n").await;
    }

    /// A multi-line response (RFC 3977 §3.1.1): the lines up to the lone
    /// dot, their dot-stuffing undone, as CRLF bytes.
    async fn block(&mut self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        loop {
            let mut line = Vec::new();
            let n = self
                .read
                .read_until(b'\n', &mut line)
                .await
                .map_err(Error::Io)?;
            if n == 0 {
                return Err(io("news server closed the connection"));
            }
            let text = line.strip_suffix(b"\n").unwrap_or(&line);
            let text = text.strip_suffix(b"\r").unwrap_or(text);
            if text == b"." {
                return Ok(out);
            }
            let start = usize::from(text.starts_with(b".."));
            out.extend_from_slice(&text[start..]);
            out.extend_from_slice(b"\r\n");
        }
    }
}

/// A reader session over the news server.
pub struct Reader {
    session: Session,
    timeout: Duration,
}

impl std::fmt::Debug for Reader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reader").finish_non_exhaustive()
    }
}

impl Reader {
    /// Select `group` and return its first and last article numbers.
    /// # Errors
    /// Returns an I/O error for an unknown group or a broken session.
    pub async fn group(&mut self, group: &str) -> Result<(u64, u64)> {
        let answer = self.command(&format!("GROUP {group}")).await?;
        // 211 <number> <low> <high> <group>
        let mut words = answer.split_whitespace();
        if words.next() != Some("211") {
            return Err(io(&format!("news server has no group {group}: {answer}")));
        }
        let _count = words.next();
        let first = words.next().and_then(|n| n.parse().ok());
        let last = words.next().and_then(|n| n.parse().ok());
        match (first, last) {
            (Some(first), Some(last)) => Ok((first, last)),
            _ => Err(io(&format!("news server answered GROUP with {answer}"))),
        }
    }

    /// The headers of article `number` in the selected group, or `None`
    /// when the server says it has no such article.
    /// # Errors
    /// Returns an I/O error for a broken session: closed, timed out, or
    /// answering with anything but a status line for the article.
    pub async fn head(&mut self, number: u64) -> Result<Option<Vec<u8>>> {
        self.fetch("HEAD", number, "221").await
    }

    /// Article `number` whole (headers, blank line, body), or `None` when
    /// the server says it has no such article.
    /// # Errors
    /// Returns an I/O error for a broken session, as [`Reader::head`].
    pub async fn article(&mut self, number: u64) -> Result<Option<Vec<u8>>> {
        self.fetch("ARTICLE", number, "220").await
    }

    /// Close the session.
    pub async fn quit(mut self) {
        self.session.quit().await;
    }

    async fn command(&mut self, command: &str) -> Result<String> {
        tokio::time::timeout(self.timeout, async {
            self.session
                .write
                .write_all(format!("{command}\r\n").as_bytes())
                .await
                .map_err(Error::Io)?;
            reply(&mut self.session.read).await
        })
        .await
        .map_err(|_| io("news server timed out"))?
    }

    async fn fetch(&mut self, verb: &str, number: u64, code: &str) -> Result<Option<Vec<u8>>> {
        let answer = self.command(&format!("{verb} {number}")).await?;
        if answer.starts_with(code) {
            return tokio::time::timeout(self.timeout, self.session.block())
                .await
                .map_err(|_| io("news server timed out"))?
                .map(Some);
        }
        // The server has no such article (RFC 3977 §6.2: 420, 423, 430).
        // A caller may pass it; anything else — a lost session, an unknown
        // reply — it must not read past.
        if ["420", "423", "430"]
            .iter()
            .any(|gone| answer.starts_with(gone))
        {
            return Ok(None);
        }
        Err(io(&format!(
            "news server refused {verb} {number}: {answer}"
        )))
    }
}

/// Mailman's `gatenews` on one article of the list's newsgroup.
///
/// `None` when the article is the list's own (its `List-Id` names the
/// list) or has no `From` to post as; otherwise the article addressed to
/// the list (`To` moved to `X-Originally-To`, `To` set to the posting
/// address) and the address of its `From`, the sender the list admits it
/// as.
/// # Errors
/// Returns `UnsafeHeaderContent` for a header block that cannot be read.
pub fn inbound(raw: &[u8], list: &MailingList) -> Result<Option<(Vec<u8>, String)>> {
    let (blank, body) = cook::header_body_split(raw).ok_or(Error::UnsafeHeaderContent)?;
    let fields = fields(raw, blank)?;
    let ours = format!("<{}>", list.id);
    if fields
        .iter()
        .any(|field| field.lower == "list-id" && field.value.trim_end().ends_with(&ours))
    {
        return Ok(None);
    }
    let Some(sender) = mail_parser::MessageParser::default()
        .parse(raw)
        .and_then(|message| {
            message
                .from()
                .and_then(|from| from.first())
                .and_then(|address| address.address().map(str::to_owned))
        })
        .filter(|address| address.contains('@'))
    else {
        return Ok(None);
    };
    let eol: &[u8] = if raw[..body].contains(&b'\r') {
        b"\r\n"
    } else {
        b"\n"
    };
    let mut output = Vec::with_capacity(raw.len() + 64);
    for field in &fields {
        match field.lower.as_str() {
            "x-originally-to" => {}
            "to" => {
                if field.value.bytes().any(|b| b == b'\r' || b == b'\n') {
                    return Err(Error::UnsafeHeaderContent);
                }
                output.extend_from_slice(b"X-Originally-To: ");
                output.extend_from_slice(field.value.as_bytes());
                output.extend_from_slice(eol);
            }
            _ => output.extend_from_slice(&field.raw),
        }
    }
    output.extend_from_slice(b"To: ");
    output.extend_from_slice(list.id.posting_address().as_bytes());
    output.extend_from_slice(eol);
    output.extend_from_slice(&raw[blank..]);
    Ok(Some((output, sender)))
}

/// One reply line, without its line ending.
async fn reply(read: &mut BufReader<tokio::net::tcp::OwnedReadHalf>) -> Result<String> {
    let mut line = String::new();
    let n = read.read_line(&mut line).await.map_err(Error::Io)?;
    if n == 0 {
        return Err(io("news server closed the connection"));
    }
    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}

/// The article as NNTP wants it: CRLF lines, a leading dot doubled, a
/// final line break, then the lone dot.
fn dot_stuffed(article: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(article.len() + 64);
    for line in article.split_inclusive(|b| *b == b'\n') {
        let text = line.strip_suffix(b"\n").unwrap_or(line);
        let text = text.strip_suffix(b"\r").unwrap_or(text);
        if text.starts_with(b".") {
            out.push(b'.');
        }
        out.extend_from_slice(text);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b".\r\n");
    out
}
