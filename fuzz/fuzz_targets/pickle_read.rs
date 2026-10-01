//! The Python 2 pickle reader `import21` uses on arbitrary bytes.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(item) = listmngr_import::pickle::read(data) {
        let _ = format!("{item:?}");
    }
});
