//! VERP (variable envelope return path).
//!
//! The per-recipient bounce address `list-bounces+local=domain@host` Mailman
//! uses so an unrecognizable bounce still names the recipient it concerns.
//! Format and delimiter come from `[mta] verp_format` / `verp_delimiter`
//! (Mailman's defaults).

use crate::{Error, ListId, Result};

/// Mailman's default `verp_format`.
pub const DEFAULT_FORMAT: &str = "{bounces}+{local}={domain}";
/// The pseudo local part a bounce probe is addressed with: the probe token
/// takes the domain's place, so `list-bounces+probe=TOKEN@host` matches
/// the same MTA routes as any VERP bounce.
pub const PROBE_LOCAL: &str = "probe";

/// The placeholders `verp_format` must contain.
const BOUNCES: &str = "{bounces}";
const LOCAL: &str = "{local}";
const DOMAIN: &str = "{domain}";
const PLACEHOLDERS: [&str; 3] = [BOUNCES, LOCAL, DOMAIN];

/// Validate `[mta] verp_format` and `verp_delimiter`.
/// # Errors
/// Returns a validation error for a format missing a placeholder, or a
/// delimiter that is not one printable ASCII character allowed in a local
/// part, or that the format does not use.
pub fn validate(format: &str, delimiter: &str) -> Result<()> {
    for placeholder in PLACEHOLDERS {
        if !format.contains(placeholder) {
            return Err(Error::Validation(format!(
                "mta.verp_format must contain {placeholder}"
            )));
        }
    }
    let ok = delimiter.len() == 1
        && delimiter
            .bytes()
            .all(|b| b.is_ascii_graphic() && !b"@<>()[]\\,;:\"".contains(&b));
    if !ok {
        return Err(Error::Validation(
            "mta.verp_delimiter must be one printable ASCII character".into(),
        ));
    }
    if !format.contains(delimiter) {
        return Err(Error::Validation(
            "mta.verp_format must use mta.verp_delimiter".into(),
        ));
    }
    if format.contains(['@', ' ', '\r', '\n']) {
        return Err(Error::Validation(
            "mta.verp_format is a local part: no @, spaces or line breaks".into(),
        ));
    }
    Ok(())
}

/// The envelope sender for `recipient`'s copy of a post to `list`, or
/// `None` when the recipient cannot be encoded reversibly (a local part
/// carrying `=`, or no `@`).
#[must_use]
pub fn encode(format: &str, list: &ListId, recipient: &str) -> Option<String> {
    let (local, domain) = recipient.rsplit_once('@')?;
    if local.is_empty()
        || domain.is_empty()
        || local.contains(['=', '@'])
        || recipient
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return None;
    }
    let bounces = format!("{}-bounces", list.list_name());
    let encoded = format
        .replace(BOUNCES, &bounces)
        .replace(LOCAL, local)
        .replace(DOMAIN, domain);
    Some(format!("{encoded}@{}", list.mail_host()))
}

/// The recipient a VERP local part names, with its bounces local part.
///
/// `dev-bounces+alice=example.com` → (`dev-bounces`, `alice@example.com`).
/// Mailman's regexp: the bounces part ends at the first delimiter, the
/// local part at the last `=`.
#[must_use]
pub fn decode(local_part: &str, delimiter: &str) -> Option<(String, String)> {
    let (bounces, rest) = local_part.split_once(delimiter)?;
    let (local, domain) = rest.rsplit_once('=')?;
    if bounces.is_empty()
        || local.is_empty()
        || domain.is_empty()
        || domain.contains(['=', '@'])
        || !bounces.ends_with("-bounces")
    {
        return None;
    }
    Some((bounces.to_owned(), format!("{local}@{domain}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FORMAT: &str = "{bounces}+{local}={domain}";

    #[test]
    fn mailman_defaults_round_trip() {
        let list: ListId = "dev.example.invalid".parse().unwrap();
        let sender = encode(FORMAT, &list, "alice+tag@example.com").unwrap();
        assert_eq!(sender, "dev-bounces+alice+tag=example.com@example.invalid");
        let local = sender.split('@').next().unwrap();
        assert_eq!(
            decode(local, "+"),
            Some(("dev-bounces".into(), "alice+tag@example.com".into()))
        );
        assert_eq!(
            encode(FORMAT, &list, "a=b@example.com"),
            None,
            "= cannot round trip"
        );
        assert_eq!(encode(FORMAT, &list, "nobody"), None);
        assert_eq!(decode("dev-bounces", "+"), None);
        assert_eq!(decode("dev-request+alice=example.com", "+"), None);
        assert_eq!(decode("dev-bounces+alice", "+"), None);
    }

    #[test]
    fn configuration_is_validated() {
        assert!(validate(FORMAT, "+").is_ok());
        assert!(validate("{bounces}-{local}={domain}", "-").is_ok());
        assert!(validate("{bounces}+{local}", "+").is_err());
        assert!(validate(FORMAT, "++").is_err());
        assert!(validate(FORMAT, "@").is_err());
        assert!(
            validate(FORMAT, "-").is_err(),
            "delimiter unused by the format"
        );
        assert!(validate("{bounces}+{local}={domain}@x", "+").is_err());
    }
}
