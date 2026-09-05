use listmngr_mail::message_id_hash;

#[test]
fn hash_rejects_control_or_unicode_outer_whitespace() {
    for id in ["\na@b", "a@b\r\n", "\u{a0}a@b"] {
        assert!(message_id_hash(id).is_err());
    }
}

#[test]
fn invalid_ids_are_rejected() {
    for id in [
        "",
        "<>",
        "missing-at",
        "a@@b",
        "<a@b",
        "a@b>",
        "<<a@b>>",
        "a b@c",
        "a\r\n@b",
        "a@b\0",
        "é@b",
        ".a@b",
        "a..b@c",
        "a@b.",
    ] {
        assert!(message_id_hash(id).is_err(), "accepted {id:?}");
    }
}

#[test]
fn mailman_python_oracles() {
    // Python: base64.b32encode(hashlib.sha1(id.encode()).digest()).decode()
    for (id, expected) in [
        ("test@example.com", "KZYVTVRC765VBMI3B36TA67DLBREUJXO"),
        ("ABC.123@Example.COM", "DS3KV6UHKYDTGGQ2TAU64I7DP2HBC72Z"),
        ("a@b", "2AZYWIZED4JE6LED7JXVQIMSAA437BO3"),
    ] {
        assert_eq!(message_id_hash(id).unwrap(), expected);
        assert_eq!(message_id_hash(&format!("<{id}>")).unwrap(), expected);
        assert_eq!(message_id_hash(&format!(" \t{id} \t")).unwrap(), expected);
    }
}
