//! Mailman's `Reply-To` munging (`cook-headers`).
//!
//! The list's `reply_goes_to_list` policy, `reply_to_address` and
//! `first_strip_reply_to` applied to the inbound `Reply-To` addresses,
//! deduplicated case-insensitively by address and re-emitted as one header.
use crate::{Result, cook};
use listmngr_core::{MailingList, ReplyToMunging};
use mail_parser::MessageParser;

/// One mailbox as it will be written back.
#[derive(Debug, Clone)]
struct Mailbox {
    name: Option<String>,
    address: String,
}

fn inbound(raw: &[u8]) -> Vec<Mailbox> {
    let Some(message) = MessageParser::default().parse(raw) else {
        return Vec::new();
    };
    message
        .reply_to()
        .map(|list| {
            list.iter()
                .filter_map(|addr| {
                    let address = addr.address()?.trim();
                    (!address.is_empty() && listmngr_mail_safe_mailbox(address)).then(|| Mailbox {
                        name: addr
                            .name()
                            .map(str::trim)
                            .filter(|name| !name.is_empty())
                            .map(str::to_owned),
                        address: address.to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn listmngr_mail_safe_mailbox(address: &str) -> bool {
    crate::owner::safe_mailbox(address) && address.len() <= 254
}

fn push_unique(set: &mut Vec<Mailbox>, mailbox: Mailbox) {
    if !set
        .iter()
        .any(|known| known.address.eq_ignore_ascii_case(&mailbox.address))
    {
        set.push(mailbox);
    }
}

/// The `Reply-To` addresses the delivered copy should carry, in order:
/// the inbound ones unless stripped, then what the policy adds.
fn munged(raw: &[u8], list: &MailingList) -> Vec<Mailbox> {
    let settings = &list.alter_messages;
    let mut set = Vec::new();
    if list.anonymous_list {
        // An anonymous list already points replies at itself.
        push_unique(
            &mut set,
            Mailbox {
                name: None,
                address: list.id.posting_address(),
            },
        );
    } else if !settings.first_strip_reply_to
        && settings.reply_goes_to_list != ReplyToMunging::ExplicitHeaderOnly
    {
        for mailbox in inbound(raw) {
            push_unique(&mut set, mailbox);
        }
    }
    let explicit = settings.reply_to_address.trim();
    match settings.reply_goes_to_list {
        ReplyToMunging::NoMunging => {}
        ReplyToMunging::PointToList => push_unique(
            &mut set,
            Mailbox {
                name: None,
                address: list.id.posting_address(),
            },
        ),
        ReplyToMunging::ExplicitHeader | ReplyToMunging::ExplicitHeaderOnly => {
            if !explicit.is_empty() && listmngr_mail_safe_mailbox(explicit) {
                push_unique(
                    &mut set,
                    Mailbox {
                        name: None,
                        address: explicit.to_owned(),
                    },
                );
            }
        }
    }
    set
}

/// Apply the policy: remove every inbound `Reply-To` and write the munged
/// set back as one header, or nothing when the set is empty.
/// # Errors
/// Returns an error for a message with no header/body boundary.
pub fn apply(raw: &[u8], list: &MailingList) -> Result<Vec<u8>> {
    let settings = &list.alter_messages;
    let untouched = !list.anonymous_list
        && !settings.first_strip_reply_to
        && settings.reply_goes_to_list == ReplyToMunging::NoMunging;
    if untouched {
        return Ok(raw.to_vec());
    }
    let set = munged(raw, list);
    let stripped = cook::strip_named_headers(raw, &["reply-to"])?;
    if set.is_empty() {
        return Ok(stripped);
    }
    let value = set
        .iter()
        .map(|mailbox| cook::format_mailbox(mailbox.name.as_deref(), &mailbox.address))
        .collect::<Vec<_>>()
        .join(", ");
    cook::append_headers(&stripped, &[("Reply-To".to_owned(), value)])
}
