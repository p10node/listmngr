//! Notice templates: Mailman's names, the built-in English catalog,
//! `$placeholder` expansion, and the bounded loaders behind template URIs.
//!
//! Resolution across scopes (list → domain → site → built-in) needs the
//! database and lives in `listmngr-db`; everything here is pure.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Longest template body accepted from any source.
pub const MAX_BODY_BYTES: usize = 65_536;

/// Every template name this runtime knows.
///
/// The Mailman names come from `docs/PLAN.md` §4.9; `list:user:notice:help`
/// and `list:user:notice:receipt` are listmngr additions for the email-command
/// help reply and the confirmation completion receipt, which Mailman
/// generates in code. The `site:user:action:*` names are listmngr additions
/// for mail the site sends to a person outside any list — Mailman leaves
/// address verification and password reset to Django.
pub const NAMES: &[&str] = &[
    "domain:admin:notice:new-list",
    "list:admin:action:post",
    "list:admin:action:subscribe",
    "list:admin:action:unsubscribe",
    "list:admin:notice:disable",
    "list:admin:notice:increment",
    "list:admin:notice:pending",
    "list:admin:notice:removal",
    "list:admin:notice:subscribe",
    "list:admin:notice:unrecognized",
    "list:admin:notice:unsubscribe",
    "list:member:digest:footer",
    "list:member:digest:header",
    "list:member:digest:masthead",
    "list:member:generic:footer",
    "list:member:regular:footer",
    "list:member:regular:header",
    "list:user:action:invite",
    "list:user:notice:autoresponse",
    "list:user:action:subscribe",
    "list:user:action:unsubscribe",
    "list:user:notice:echo",
    "list:user:notice:goodbye",
    "list:user:notice:help",
    "list:user:notice:hold",
    "list:user:notice:no-more-today",
    "list:user:notice:post",
    "list:user:notice:probe",
    "list:user:notice:receipt",
    "list:user:notice:refuse",
    "list:user:notice:rejected",
    "list:user:notice:warning",
    "list:user:notice:welcome",
    "site:user:action:reset",
    "site:user:action:verify",
];

#[must_use]
pub fn is_known_name(name: &str) -> bool {
    NAMES.contains(&name)
}

/// The built-in English body for `name`.
///
/// Ported from Mailman 3's `en` templates with product wording adjusted (no
/// member passwords, no attachment of the original on rejection) and
/// listmngr's HTTP confirmation instructions appended to the confirmation
/// challenges. One arm per template keeps the catalog greppable by name.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn builtin(name: &str) -> Option<&'static str> {
    Some(match name {
        "domain:admin:notice:new-list" => {
            "The mailing list '$listname' has just been created for you.  The\n\
             following is some basic information about your mailing list.\n\
             \n\
             There is an email-based interface for users (not administrators) of\n\
             your list; you can get info about using it by sending a message with\n\
             just the word 'help' as subject or in the body, to:\n\
             \n    $request_email\n\
             \n\
             Please address all questions to $site_email.\n"
        }
        "list:admin:action:post" => {
            "As list administrator, your authorization is requested for the\n\
             following mailing list posting:\n\
             \n    List:    $listname\n    From:    $sender_email\n    Subject: $subject\n\
             \n\
             The message is being held because:\n\
             \n$reasons\n\
             \n\
             At your convenience, visit your dashboard to approve or deny the\n\
             request.\n"
        }
        "list:admin:action:subscribe" => {
            "Your authorization is required for a mailing list subscription request\n\
             approval:\n\
             \n    For:  $member\n    List: $listname\n"
        }
        "list:admin:action:unsubscribe" => {
            "Your authorization is required for a mailing list unsubscription\n\
             request approval:\n\
             \n    For:  $member\n    List: $listname\n"
        }
        "list:admin:notice:disable" => {
            "$member's subscription has been disabled on $listname due to their\n\
             bounce score exceeding the mailing list's bounce_score_threshold.\n"
        }
        "list:admin:notice:increment" => {
            "$member's bounce score on $listname has been incremented to $score.\n"
        }
        "list:admin:notice:pending" => {
            "The $listname list has $count moderation requests waiting.\n\
             \n$data\n\
             \n\
             Please attend to this at your earliest convenience.\n"
        }
        "list:admin:notice:removal" => {
            "$member has been removed from $listname due to excessive bounces.\n"
        }
        "list:admin:notice:subscribe" => {
            "$member has been successfully subscribed to $display_name.\n"
        }
        "list:admin:notice:unrecognized" => {
            "The attached message was received as a bounce, but either the bounce\n\
             format was not recognized, or no member addresses could be extracted\n\
             from it.  This mailing list has been configured to send all\n\
             unrecognized bounce messages to the list administrator(s).\n"
        }
        "list:admin:notice:unsubscribe" => "$member has been removed from $display_name.\n",
        "list:member:digest:footer"
        | "list:member:digest:header"
        | "list:member:regular:header" => "",
        "list:member:digest:masthead" => {
            "Send $display_name mailing list submissions to\n\
             \t$listname\n\
             \n\
             To subscribe or unsubscribe via email, send a message with subject or\n\
             body 'help' to\n\
             \t$request_email\n\
             \n\
             You can reach the person managing the list at\n\
             \t$owner_email\n\
             \n\
             When replying, please edit your Subject line so it is more specific\n\
             than \"Re: Contents of $display_name digest...\"\n"
        }
        "list:member:generic:footer" | "list:member:regular:footer" => {
            "_______________________________________________\n\
             $display_name mailing list -- $listname\n\
             To unsubscribe send an email to ${short_listname}-leave@${domain}\n"
        }
        "list:user:action:invite" => {
            "Your address \"$user_email\" has been invited to join the $listname\n\
             mailing list at $domain by the $listname mailing list owner.  You may\n\
             accept the invitation by simply replying to this message, keeping the\n\
             Subject: header intact.\n\
             \n\
             Or you should include the following line -- and only the following\n\
             line -- in a message to $request_email:\n\
             \n    confirm $token\n\
             \n\
             Note that simply sending a `reply' to this message should work from\n\
             most mail readers.\n\
             \n\
             If you want to decline this invitation, please simply disregard this\n\
             message.  If you have any questions, please send them to\n\
             $owner_email.\n"
        }
        "list:user:action:subscribe" => {
            "Email Address Registration Confirmation\n\
             \n\
             Hello, this is the mailing list server at $domain.\n\
             \n\
             We have received a registration request for the email address\n\
             \n    $user_email\n\
             \n\
             Before you can start using the $listname mailing list, you must first\n\
             confirm that this is your email address.  You can do this by replying\n\
             to this message, keeping the Subject header intact.\n\
             \n\
             You can also confirm over HTTPS by sending\n\
             POST $confirm_uri with the JSON body {\"token\":\"$token\"}.\n\
             Token: $token\n\
             This token expires in 24 hours and works only once.\n\
             \n\
             If you do not wish to register this email address, simply disregard\n\
             this message.  If you think you are being maliciously subscribed to\n\
             the list, or have any other questions, you may contact\n\
             \n    $owner_email\n"
        }
        "list:user:action:unsubscribe" => {
            "Email Address Unsubscription Confirmation\n\
             \n\
             Hello, this is the mailing list server at $domain.\n\
             \n\
             We have received an unsubscription request for the email address\n\
             \n    $user_email\n\
             \n\
             Before you can be removed from the $listname mailing list, you must\n\
             first confirm that this is your email address.  You can do this by\n\
             replying to this message, keeping the Subject header intact.\n\
             \n\
             You can also confirm over HTTPS by sending\n\
             POST $confirm_uri with the JSON body {\"token\":\"$token\"}.\n\
             Token: $token\n\
             This token expires in 24 hours and works only once.\n\
             \n\
             If you do not wish to unsubscribe this email address, simply disregard\n\
             this message.  If you have any questions, you may contact\n\
             \n    $owner_email\n"
        }
        "list:user:notice:goodbye" => {
            "You have been unsubscribed from the \"$display_name\" mailing list\n\
             ($listname).\n\
             \n\
             If you have any questions, you may contact\n\
             \n    $owner_email\n"
        }
        "list:user:notice:autoresponse" => {
            "This is an automatic reply from the $listname mailing list at $domain.\n\
             Your message has been received. Nobody has read it yet; it will be dealt\n\
             with in due course. If it needs a person now, write to $owner_email.\n\
             \n\
             You will not be answered again by this address for a while.\n"
        }
        "list:user:notice:echo" => {
            "This is the $listname command bot at $domain answering your `echo`\n\
             command with the text it carried:\n\
             \n    $echo\n\
             \n\
             Nothing was changed. Send `help` to $request_email for the commands\n\
             this list understands.\n"
        }
        "list:user:notice:help" => {
            "Send one command in the subject, or first nonblank text/plain body line\n\
             with an empty subject, to $request_email.\n\
             To use Reply, replace the subject with a single command (for example:\n\
             join). Do not keep the help subject.\n\
             For a human administrator, write separately to $owner_email; replies to\n\
             this help go to the command bot.\n\
             \n\
             join or subscribe: request membership for your envelope mailbox.\n\
             leave or unsubscribe: request removal of your envelope mailbox.\n\
             confirm TOKEN: confirm the one-time challenge sent to that mailbox.\n\
             help: this bounded help, at most once per mailbox/list/hour.\n\
             echo TEXT: send that text straight back, under the same budget.\n\
             end or stop: stop reading commands here (before a signature, say).\n\
             No mailbox arguments, passwords, moderator commands or multi-command\n\
             scripts are supported.\n"
        }
        "list:user:notice:hold" => {
            "Your mail to '$listname' with the subject\n\
             \n    $subject\n\
             \n\
             Is being held until the list moderator can review it for approval.\n\
             \n\
             The message is being held because:\n\
             \n$reasons\n\
             \n\
             Either the message will get posted to the list, or you will receive\n\
             notification of the moderator's decision.\n"
        }
        "list:user:notice:no-more-today" => {
            "We have received a message from your address <$sender_email>\n\
             requesting an automated response from the $listname mailing list.\n\
             \n\
             The number we have seen today: $count.  In order to avoid problems such\n\
             as mail loops between email robots, we will not be sending you any\n\
             further responses today.  Please try again tomorrow.\n\
             \n\
             If you believe this message is in error, or if you have any questions,\n\
             please contact the list owner at $owner_email.\n"
        }
        "list:user:notice:post" => {
            "Your message entitled\n\
             \n    $subject\n\
             \n\
             was successfully received by the $display_name mailing list.\n"
        }
        "list:user:notice:probe" => {
            "This is a probe message.  You can ignore this message.\n\
             \n\
             The $listname mailing list has received a number of bounces from you,\n\
             indicating that there may be a problem delivering messages to\n\
             $sender_email.  Please examine this message to make sure there are no\n\
             problems with your email address.  You may want to check with your\n\
             mail administrator for more help.\n\
             \n\
             You don't need to do anything to remain an enabled member of the\n\
             mailing list.\n\
             \n\
             If you have any questions or problems, you can contact the mailing\n\
             list owner at\n\
             \n    $owner_email\n"
        }
        "list:user:notice:receipt" => {
            "$outcome $listname.\n\
             This receipt records the result at confirmation time.\n\
             For help, email $request_email with subject help.\n\
             For a human administrator, write to $owner_email.\n"
        }
        "list:user:notice:refuse" => {
            "Your request to the $listname mailing list\n\
             \n    $request\n\
             \n\
             has been rejected by the list moderator.  The moderator gave the\n\
             following reason for rejecting your request:\n\
             \n\"$reason\"\n\
             \n\
             Any questions or comments should be directed to the list administrator\n\
             at:\n\
             \n    $owner_email\n"
        }
        "list:user:notice:rejected" => {
            "Your message to the $listname mailing-list was rejected for the\n\
             following reasons:\n\
             \n$reasons\n"
        }
        "list:user:notice:warning" => {
            "Your membership in the mailing list $listname has been disabled due\n\
             to excessive bounces.  You will not get any more messages from this\n\
             list until you re-enable your membership.\n\
             \n\
             To re-enable your membership, visit your options page or contact the\n\
             list owner.\n\
             \n\
             If you have any questions or problems, you can contact the list owner\n\
             at\n\
             \n    $owner_email\n"
        }
        "list:user:notice:welcome" => {
            "Welcome to the \"$display_name\" mailing list!\n\
             \n\
             To post to this list, send your email to:\n\
             \n  $listname\n\
             \n\
             You can unsubscribe or make adjustments to your options via email by\n\
             sending a message to:\n\
             \n  $request_email\n\
             \n\
             with the word 'help' in the subject or body (don't include the\n\
             quotes), and you will get back a message with instructions.\n"
        }
        "site:user:action:verify" => {
            "Email Address Verification\n\
             \n\
             Hello, this is the mailing list server at $domain.\n\
             \n\
             Someone, most likely you, asked $site_name to verify that\n\
             \n    $user_email\n\
             \n\
             is your email address. To confirm, open\n\
             \n    $verify_url\n\
             \n\
             and enter this token:\n\
             \n    $token\n\
             \n\
             The token works once and expires. If you did not ask for this, you\n\
             can ignore this message; nothing changes until the token is used.\n"
        }
        "site:user:action:reset" => {
            "Password Reset\n\
             \n\
             Hello, this is the mailing list server at $domain.\n\
             \n\
             Someone, most likely you, asked $site_name to reset the password of\n\
             the account for\n\
             \n    $user_email\n\
             \n\
             To choose a new password, open\n\
             \n    $reset_url\n\
             \n\
             and enter this token:\n\
             \n    $token\n\
             \n\
             The token works once and expires. If you did not ask for this, you\n\
             can ignore this message; your password stays as it is.\n"
        }
        _ => return None,
    })
}

/// The built-in body for `name` in `language`, falling back to English.
/// Only `en` and `vi` ship; other tags are served English.
#[must_use]
pub fn builtin_in(name: &str, language: &str) -> Option<&'static str> {
    let language = listmngr_i18n::negotiate(language);
    if language == "vi"
        && let Some(body) = crate::templates_vi::builtin(name)
    {
        return Some(body);
    }
    builtin(name)
}

/// Every placeholder name a list template may use: the list's own, then
/// the ones the notice producers add for a member, a post or a token.
pub const PLACEHOLDER_NAMES: &[&str] = &[
    "listname",
    "fqdn_listname",
    "list_id",
    "short_listname",
    "list_name",
    "display_name",
    "description",
    "info",
    "domain",
    "list_domain",
    "request_email",
    "list_requests",
    "owner_email",
    "bounces_email",
    "join_email",
    "leave_email",
    "user_email",
    "user_name",
    "user_address",
    "user_delivered_to",
    "user_language",
    "subject",
    "sender_email",
    "reasons",
    "token",
    "site_name",
];

/// The placeholders every list notice and decoration can use (Mailman names).
#[must_use]
pub fn list_placeholders(list: &listmngr_core::MailingList) -> Placeholders {
    let id = &list.id;
    Placeholders::new()
        .set("listname", id.posting_address())
        .set("fqdn_listname", id.posting_address())
        .set("list_id", id.to_string())
        .set("short_listname", id.list_name())
        .set("list_name", id.list_name())
        .set("display_name", list.display_name.clone())
        .set("description", list.description.clone())
        .set("info", list.info.clone())
        .set("domain", id.mail_host())
        .set("list_domain", id.mail_host())
        .set("request_email", id.request_address())
        .set("list_requests", id.request_address())
        .set("owner_email", id.owner_address())
        .set("bounces_email", id.bounces_address())
        .set("join_email", id.join_address())
        .set("leave_email", id.leave_address())
}

/// Values for `$placeholder` expansion.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Placeholders(BTreeMap<String, String>);

impl Placeholders {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn set(mut self, name: &str, value: impl Into<String>) -> Self {
        self.0.insert(name.to_owned(), value.into());
        self
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }

    /// `defaults` for every name this set does not already carry.
    #[must_use]
    pub fn with_defaults(mut self, defaults: Self) -> Self {
        for (name, value) in defaults.0 {
            self.0.entry(name).or_insert(value);
        }
        self
    }
}

const fn is_identifier_start(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphabetic()
}

const fn is_identifier_byte(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphanumeric()
}

/// Expand `$name`, `${name}` and `$$` the way Python's
/// `string.Template.safe_substitute` does: unknown or malformed placeholders
/// are left exactly as written, and substituted values are never re-scanned.
#[must_use]
pub fn expand(template: &str, values: &Placeholders) -> String {
    let bytes = template.as_bytes();
    let mut output = String::with_capacity(template.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'$' {
            // Copy one whole UTF-8 character.
            let char_len = template[index..].chars().next().map_or(1, char::len_utf8);
            output.push_str(&template[index..index + char_len]);
            index += char_len;
            continue;
        }
        match bytes.get(index + 1) {
            Some(b'$') => {
                output.push('$');
                index += 2;
            }
            Some(b'{') => {
                let start = index + 2;
                let end = bytes[start..]
                    .iter()
                    .position(|b| *b == b'}')
                    .map(|offset| start + offset);
                match end {
                    Some(end)
                        if end > start
                            && is_identifier_start(bytes[start])
                            && bytes[start..end].iter().all(|b| is_identifier_byte(*b)) =>
                    {
                        let name = &template[start..end];
                        match values.get(name) {
                            Some(value) => output.push_str(value),
                            None => output.push_str(&template[index..=end]),
                        }
                        index = end + 1;
                    }
                    _ => {
                        output.push('$');
                        index += 1;
                    }
                }
            }
            Some(byte) if is_identifier_start(*byte) => {
                let start = index + 1;
                let mut end = start;
                while end < bytes.len() && is_identifier_byte(bytes[end]) {
                    end += 1;
                }
                let name = &template[start..end];
                match values.get(name) {
                    Some(value) => output.push_str(value),
                    None => output.push_str(&template[index..end]),
                }
                index = end;
            }
            _ => {
                output.push('$');
                index += 1;
            }
        }
    }
    output
}

/// Where a template's text comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// `mailman:///[language/]name`: the shipped catalog.
    Builtin(String),
    /// `file:///absolute/path`: a file the operator placed on the host.
    File(PathBuf),
    /// `https://...`: fetched by Mailman at send time; not fetched by this
    /// runtime (see [`load`]).
    Https(String),
}

/// Why a template URI or body was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateError {
    InvalidUri,
    UnknownTemplate,
    /// `https://` templates are declared but never fetched here.
    RemoteNotSupported,
    /// The file is missing, unreadable, over [`MAX_BODY_BYTES`] or not UTF-8.
    Unreadable,
}

impl std::fmt::Display for TemplateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidUri => "invalid template URI",
            Self::UnknownTemplate => "unknown template name",
            Self::RemoteNotSupported => "remote (https) templates are not fetched by this runtime",
            Self::Unreadable => "template file is missing, too large or not UTF-8",
        })
    }
}

impl std::error::Error for TemplateError {}

fn is_safe_uri_text(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 2048
        && !text.chars().any(|c| c.is_control() || c.is_whitespace())
}

/// Parse a template URI into a [`Source`].
///
/// # Errors
/// Returns [`TemplateError::InvalidUri`] for anything but an absolute
/// `file:///` path without `..`, an `https://` URL, or `mailman:///name`, and
/// [`TemplateError::UnknownTemplate`] for a `mailman:///` name outside the
/// catalog.
pub fn parse_uri(uri: &str) -> Result<Source, TemplateError> {
    if !is_safe_uri_text(uri) {
        return Err(TemplateError::InvalidUri);
    }
    if let Some(rest) = uri.strip_prefix("mailman:///") {
        // An optional leading language segment (`vi/name`) is accepted and
        // ignored: the catalog is English-only today.
        let name = rest.rsplit_once('/').map_or(rest, |(_, name)| name);
        if rest.matches('/').count() > 1 || name.is_empty() {
            return Err(TemplateError::InvalidUri);
        }
        if !is_known_name(name) {
            return Err(TemplateError::UnknownTemplate);
        }
        return Ok(Source::Builtin(name.to_owned()));
    }
    if let Some(path) = uri.strip_prefix("file://") {
        let path = Path::new(path);
        if !path.is_absolute()
            || path.components().any(|component| {
                !matches!(
                    component,
                    std::path::Component::RootDir | std::path::Component::Normal(_)
                )
            })
        {
            return Err(TemplateError::InvalidUri);
        }
        return Ok(Source::File(path.to_path_buf()));
    }
    if let Some(rest) = uri.strip_prefix("https://") {
        if rest.is_empty() || rest.starts_with('/') {
            return Err(TemplateError::InvalidUri);
        }
        return Ok(Source::Https(uri.to_owned()));
    }
    Err(TemplateError::InvalidUri)
}

/// Load the text behind a source. `language` selects a catalog translation
/// when one exists (`vi` today); English is the fallback.
///
/// # Errors
/// Returns [`TemplateError`] for an unknown built-in, an unreadable or
/// oversized file, or an `https://` source.
pub fn load(source: &Source, language: &str) -> Result<String, TemplateError> {
    match source {
        Source::Builtin(name) => builtin_in(name, language)
            .map(str::to_owned)
            .ok_or(TemplateError::UnknownTemplate),
        Source::File(path) => {
            let metadata = std::fs::metadata(path).map_err(|_| TemplateError::Unreadable)?;
            if !metadata.is_file() || metadata.len() > MAX_BODY_BYTES as u64 {
                return Err(TemplateError::Unreadable);
            }
            let bytes = std::fs::read(path).map_err(|_| TemplateError::Unreadable)?;
            if bytes.len() > MAX_BODY_BYTES {
                return Err(TemplateError::Unreadable);
            }
            String::from_utf8(bytes).map_err(|_| TemplateError::Unreadable)
        }
        Source::Https(_) => Err(TemplateError::RemoteNotSupported),
    }
}
