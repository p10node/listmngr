//! Template placeholders (`$name`, `${name}`, `$$`) expanded the way
//! Python's `safe_substitute` does, on arbitrary templates and values.
#![no_main]
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use listmngr_mail::templates::{Placeholders, expand};

#[derive(Arbitrary, Debug)]
struct Input {
    template: String,
    values: Vec<(String, String)>,
}

fuzz_target!(|input: Input| {
    let mut values = Placeholders::new();
    for (name, value) in input.values.iter().take(32) {
        values = values.set(name, value.as_str());
    }
    let output = expand(&input.template, &values);
    // Substituted values are never re-scanned, so a value cannot grow
    // the output beyond the template plus every value once per
    // placeholder; at the very least the output is bounded.
    assert!(
        output.len()
            <= input.template.len()
                + input.values.iter().map(|(_, v)| v.len() + 2).sum::<usize>()
                    * (input.template.len() + 1)
    );
});
