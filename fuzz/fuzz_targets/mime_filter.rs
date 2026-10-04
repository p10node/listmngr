//! Content filtering (`mime-delete`): a message filtered with the default
//! settings and with aggressive ones, HTML turned into text, and the
//! intake's structure measure, on arbitrary bytes.
#![no_main]
use libfuzzer_sys::fuzz_target;
use listmngr_core::{AlterMessages, FilterAction};
use listmngr_mail::mime_delete;

fn aggressive() -> AlterMessages {
    AlterMessages {
        filter_content: true,
        filter_types: vec!["image".into(), "application/octet-stream".into()],
        pass_types: vec!["text".into(), "multipart".into(), "message/rfc822".into()],
        filter_extensions: vec!["exe".into(), "bat".into()],
        pass_extensions: vec!["txt".into()],
        collapse_alternatives: true,
        convert_html_to_plaintext: true,
        filter_action: FilterAction::Discard,
        ..AlterMessages::default()
    }
}

fuzz_target!(|data: &[u8]| {
    let _ = mime_delete::apply(data, &AlterMessages::default());
    let _ = listmngr_mail::structure::measure(data);
    let _ = mime_delete::apply(data, &aggressive());
    let text = String::from_utf8_lossy(data);
    let _ = listmngr_mail::html_text::html_to_text(&text);
    let _ = listmngr_mail::html_text::decode_entities(&text);
});
