use listmngr_core::DkimSigningConfig;
use listmngr_mail::dkim::SigningKeys;

#[test]
fn dkim_body_hash_matches_independent_rfc_canonicalization() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("owned-fixture.pem");
    let output = std::process::Command::new("openssl")
        .args([
            "genpkey",
            "-algorithm",
            "RSA",
            "-pkeyopt",
            "rsa_keygen_bits:2048",
            "-out",
        ])
        .arg(&key)
        .output()
        .unwrap();
    assert!(output.status.success(), "fixture key generation failed");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let keys = SigningKeys::load(&[DkimSigningConfig {
        domain: "example.invalid".into(),
        selector: "fixture".into(),
        private_key_file: key,
    }])
    .unwrap();
    // SHA256/base64 expectations generated independently with dkimpy 1.1.8,
    // RFC 6376 simple/relaxed body canonicalization, not the signing dependency.
    let cases: &[(&[u8], &str, &str)] = &[
        (
            b"body\r\n",
            "Ck5SoRNWUpSR4X0COv7R5ub2pUTtl6xz4dTFz++ji4M=",
            "Ck5SoRNWUpSR4X0COv7R5ub2pUTtl6xz4dTFz++ji4M=",
        ),
        (
            b"body\r\n \r\n",
            "Ck5SoRNWUpSR4X0COv7R5ub2pUTtl6xz4dTFz++ji4M=",
            "xr66nIzqabsMN3CmqId6I3JLzwyEUEiMHGjHC0rwshg=",
        ),
        (
            b"body\r\n\t\r\n\r\n",
            "Ck5SoRNWUpSR4X0COv7R5ub2pUTtl6xz4dTFz++ji4M=",
            "zZnHs3Loep9xWOQ9yZkgJcy/a9liSFRGY7DWfzGPQdI=",
        ),
        (
            b"",
            "47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=",
            "frcCV1k9oG9oKj3dpUqdJg1PxRT2RSN/XKdLCPjaYaY=",
        ),
        (
            b" \t\r\n",
            "47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=",
            "u0A/YWu2LwxHPEJTRgHj2P4k+mGt1J9R9M26i9WFzik=",
        ),
        (
            b"one\r\n \r\ntwo\r\n",
            "0JchEGWGbQyol3MvkY+mA/9TrPJUY2tlvNtvRNHUR8c=",
            "obkSKx8rvpLld+PF3d1ruO7np6LtLa4nWZy/IlipAeU=",
        ),
        (
            b"body\r\n\r\n",
            "Ck5SoRNWUpSR4X0COv7R5ub2pUTtl6xz4dTFz++ji4M=",
            "Ck5SoRNWUpSR4X0COv7R5ub2pUTtl6xz4dTFz++ji4M=",
        ),
        (
            b"\xff\x80 body\r\n \r\n",
            "D0oBNI3shulapHBkwR7fQOC1ml03iVstdTLcEcDWbaw=",
            "NQK3jHyF5DElEm8QEphdc59LNZOdu3+XwyGHjf6m3Y4=",
        ),
    ];
    for (body, relaxed, simple) in cases {
        let mut raw = b"From: author@example.invalid\r\nSubject: Body hash\r\n\r\n".to_vec();
        raw.extend_from_slice(body);
        let signed = keys.sign("example.invalid", raw.clone()).unwrap();
        assert!(
            signed.ends_with(&raw),
            "signing changed existing MIME bytes"
        );
        let header = listmngr_mail::header_value(&signed, "DKIM-Signature").unwrap();
        let tags: std::collections::BTreeMap<_, _> = header
            .split(';')
            .filter_map(|tag| tag.trim().split_once('='))
            .collect();
        let expected = match tags["c"] {
            "relaxed/relaxed" => relaxed,
            "relaxed/simple" => simple,
            other => panic!("unexpected canonicalization: {other}"),
        };
        let body_hash: String = tags["bh"]
            .chars()
            .filter(|c| !c.is_ascii_whitespace())
            .collect();
        assert_eq!(body_hash, *expected, "body={body:?}");
    }
}
