//! The message side of Mailman's topic matcher.
//!
//! The lines the `tagger` handler feeds to `listmngr_pipeline::topics`: every
//! `Subject:` and `Keywords:` header value plus the header-like lines that
//! open the body.
use listmngr_core::MailingList;
use mail_parser::{HeaderName, MessageParser};

/// Header added by `tagger`, one topic name per hit, comma separated.
pub const TOPICS_HEADER: &str = "X-Topics";

/// The names of the list's topics this message hits, in topic order.
/// A message that does not parse hits nothing.
#[must_use]
pub fn hits(raw: &[u8], list: &MailingList) -> Vec<String> {
    if !list.topics_enabled || list.topics.is_empty() {
        return Vec::new();
    }
    let Some(message) = MessageParser::default().parse(raw) else {
        return Vec::new();
    };
    let mut lines: Vec<String> = Vec::new();
    for name in [HeaderName::Subject, HeaderName::Keywords] {
        lines.extend(
            message
                .header_values(name)
                .filter_map(|value| value.as_text().map(str::to_owned)),
        );
    }
    let body: String = message
        .text_bodies()
        .filter_map(|part| part.text_contents())
        .map(|text| text.replace("\r\n", "\n"))
        .collect::<Vec<_>>()
        .join("\n");
    lines.extend(listmngr_pipeline::topics::header_like_body_lines(
        body.lines(),
        list.topics_bodylines_limit,
    ));
    listmngr_pipeline::topics::hits(&list.topics, &lines)
        .into_iter()
        .map(str::to_owned)
        .collect()
}
