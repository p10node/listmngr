//! The email command parser (`subscribe`, `confirm <token>`, `help` and
//! the rest) and the auto-reply guard on arbitrary message bytes.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = listmngr_mail::commands::parse(data);
    let _ = listmngr_mail::commands::confirmation_token(data);
    let _ = listmngr_mail::commands::allows_reply(data);
});
