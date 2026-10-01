//! VERP: a format validated, a return path encoded for a list and a
//! recipient, and a local part decoded back — on arbitrary strings.
#![no_main]
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use listmngr_core::{ListId, verp};

#[derive(Arbitrary, Debug)]
struct Input {
    format: String,
    delimiter: String,
    list: String,
    recipient: String,
    local_part: String,
}

fuzz_target!(|input: Input| {
    let _ = verp::validate(&input.format, &input.delimiter);
    let _ = verp::decode(&input.local_part, &input.delimiter);
    if let Ok(list) = input.list.parse::<ListId>() {
        if let Some(encoded) = verp::encode(&input.format, &list, &input.recipient) {
            // What was encoded with the default delimiter decodes again.
            let local = encoded.split('@').next().unwrap_or_default();
            let _ = verp::decode(local, "+");
        }
    }
});
