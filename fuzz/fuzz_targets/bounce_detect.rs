//! The bounce detectors and the DSN parser on arbitrary message bytes:
//! Mailman's heuristics ported in `listmngr_mail::bounce`, RFC 3464
//! reports in `listmngr_mail::dsn`.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let detected = listmngr_mail::bounce::detect(data);
    let _ = format!("{detected:?}");
    let _ = listmngr_mail::dsn::parse(data);
    let _ = listmngr_mail::dsn::parse_report(data);
});
