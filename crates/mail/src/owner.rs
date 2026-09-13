//! Conservative experimental owner forwarding admission (not a posting policy).

/// Preserve author/reply/thread and MIME presentation, not transport claims or
/// arbitrary private control fields. Body octets are never rewritten.
/// # Errors
/// Missing header/body boundary or invalid additions.
pub fn cook(raw: &[u8]) -> crate::Result<Vec<u8>> {
    let mut filtered = Vec::with_capacity(raw.len());
    let mut keep = false;
    let mut body = false;
    for line in raw.split_inclusive(|b| *b == b'\n') {
        if line == b"\r\n" || line == b"\n" {
            body = true;
        }
        if !body && !line.starts_with(b" ") && !line.starts_with(b"\t") {
            let name = line.split(|b| *b == b':').next().unwrap_or_default();
            keep = [
                "from",
                "to",
                "cc",
                "reply-to",
                "subject",
                "date",
                "message-id",
                "in-reply-to",
                "references",
                "mime-version",
                "content-type",
                "content-transfer-encoding",
                "content-disposition",
                "content-language",
            ]
            .iter()
            .any(|allowed| name.eq_ignore_ascii_case(allowed.as_bytes()));
        }
        if body || keep {
            filtered.extend_from_slice(line);
        }
    }
    crate::cook_headers(
        &filtered,
        None,
        &[("Auto-Submitted".into(), "auto-forwarded".into())],
    )
}

use listmngr_core::{Address, ListId};

/// Only ASCII dot-atom transport mailboxes; no SMTP/header delimiters.
#[must_use]
pub fn safe_mailbox(email: &str) -> bool {
    let Some((local, _)) = email.split_once('@') else {
        return false;
    };
    email.is_ascii()
        && !email
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
        && Address::new(email, String::new()).is_ok()
        && !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..")
        && local
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".!#$%&'*+-/=?^_`{|}~".contains(&b))
}

/// Also exclude unknown suffix/plus routes: forwarding must never depend on an
/// MTA's extension fallback, nor return to this aggregate's control addresses.
#[must_use]
pub fn points_to_list(email: &str, list: &ListId) -> bool {
    let email = email.to_ascii_lowercase();
    let Some((local, host)) = email.split_once('@') else {
        return false;
    };
    listmngr_core::normalize_domain(host).is_ok_and(|host| host == list.mail_host())
        && (local == list.list_name()
            || local.starts_with(&format!("{}-", list.list_name()))
            || local.starts_with(&format!("{}+", list.list_name())))
}

/// Reject automatic/list traffic and malformed header syntax, inspecting every
/// field rather than trusting a first parsed header or message context marker.
#[must_use]
pub fn allows_forward(raw: &[u8], sender: Option<&str>, list: &ListId) -> bool {
    if !sender.is_some_and(|s| safe_mailbox(s) && !points_to_list(s, list))
        || !crate::commands::allows_reply(raw)
        || crate::parse_message_id(raw).is_err()
    {
        return false;
    }
    let mut has_field = false;
    for line in raw.split_inclusive(|b| *b == b'\n') {
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            return true;
        }
        if line
            .iter()
            .any(|b| *b == b'\r' || (*b < 32 && *b != b'\t') || *b == 127)
        {
            return false;
        }
        if line.starts_with(b" ") || line.starts_with(b"\t") {
            if !has_field {
                return false;
            }
            continue;
        }
        let Some(colon) = line.iter().position(|b| *b == b':') else {
            return false;
        };
        let name = &line[..colon];
        if name.is_empty() || !name.iter().all(|b| (33..=126).contains(b)) {
            return false;
        }
        has_field = true;
        let name = String::from_utf8_lossy(name).to_ascii_lowercase();
        if name.starts_with("list-")
            || name.starts_with("resent-")
            || matches!(
                name.as_str(),
                "x-beenthere"
                    | "precedence"
                    | "x-auto-response-suppress"
                    | "x-autoreply"
                    | "x-autorespond"
                    | "x-loop"
            )
        {
            return false;
        }
    }
    false
}
