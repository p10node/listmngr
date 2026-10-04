//! `[mta]`'s structure ceilings: the defaults are the security design's
//! and each is validated at load to be at least one and bounded.
use listmngr_core::MtaConfig;

#[test]
fn the_defaults_are_the_designs() {
    let mta = MtaConfig::default();
    assert_eq!(
        (mta.max_header_count, mta.max_mime_parts, mta.max_mime_depth),
        (500, 1000, 20)
    );
    mta.validate().unwrap();
}

#[test]
fn each_ceiling_is_at_least_one_and_bounded() {
    type Set = fn(&mut MtaConfig, u32);
    let keys: [(&str, Set, u32); 3] = [
        (
            "mta.max_header_count must be 1..65536",
            |m, v| m.max_header_count = v,
            65_537,
        ),
        (
            "mta.max_mime_parts must be 1..1000000",
            |m, v| m.max_mime_parts = v,
            1_000_001,
        ),
        (
            "mta.max_mime_depth must be 1..1000",
            |m, v| m.max_mime_depth = v,
            1001,
        ),
    ];
    for (message, set, too_many) in keys {
        for value in [0, too_many] {
            let mut mta = MtaConfig::default();
            set(&mut mta, value);
            let error = mta.validate().unwrap_err().to_string();
            assert!(error.contains(message), "{value}: {error}");
        }
        let mut mta = MtaConfig::default();
        set(&mut mta, 1);
        mta.validate().unwrap();
        set(&mut mta, too_many - 1);
        mta.validate().unwrap();
    }
}
