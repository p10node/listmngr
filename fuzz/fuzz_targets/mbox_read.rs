//! The mbox reader and the importer's per-message preparation on
//! arbitrary bytes: at most sixty-four messages are taken from one input.
#![no_main]
use libfuzzer_sys::fuzz_target;
use listmngr_archive::mbox::{Reader, prepare};
use listmngr_core::ListId;
use std::io::Cursor;
use std::sync::OnceLock;

fn list() -> &'static ListId {
    static LIST: OnceLock<ListId> = OnceLock::new();
    LIST.get_or_init(|| "fuzz.example.invalid".parse().expect("a list id"))
}

fuzz_target!(|data: &[u8]| {
    let reader = Reader::new(Cursor::new(data));
    for message in reader.take(64) {
        let Ok(raw) = message else {
            break;
        };
        let _ = prepare(list(), raw, 1_700_000_000_000);
    }
});
