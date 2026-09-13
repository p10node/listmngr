//! `validate-authenticity` over a seeded DNS cache: a message signed with an
//! owned fixture key passes DKIM, SPF and DMARC; a tampered body fails; a
//! domain without a DMARC record yields `dmarc=none` and no mitigation.
use base64::Engine;
use listmngr_mail::authenticity::{Client, TxtCache, Verifier};
use listmngr_mail::dkim::SigningKeys;
use mail_auth::common::parse::TxtRecordParser;
use mail_auth::hickory_resolver::proto::op::ResponseCode;
use mail_auth::{DnsError, Error, Txt};
use std::sync::Arc;

const DOMAIN: &str = "sender.invalid";

fn fixture_key(dir: &std::path::Path) -> (std::path::PathBuf, String) {
    let path = dir.join("owned-test.pem");
    let generated = std::process::Command::new("openssl")
        .args([
            "genpkey",
            "-algorithm",
            "RSA",
            "-pkeyopt",
            "rsa_keygen_bits:2048",
            "-out",
        ])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        generated.status.success(),
        "owned RSA fixture generation failed"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let public = std::process::Command::new("openssl")
        .args(["pkey", "-in"])
        .arg(&path)
        .args(["-pubout", "-outform", "DER"])
        .output()
        .unwrap();
    assert!(public.status.success());
    let record = format!(
        "v=DKIM1; k=rsa; p={}",
        base64::engine::general_purpose::STANDARD.encode(public.stdout)
    );
    (path, record)
}

const fn not_found() -> Txt {
    Txt::Error(Error::Dns(DnsError::RecordNotFound(ResponseCode::NXDomain)))
}

fn seeded(record: &str, dmarc: Option<&str>) -> TxtCache {
    let cache = TxtCache::default();
    cache.seed(
        &format!("fixture._domainkey.{DOMAIN}."),
        Txt::DomainKey(Arc::new(
            mail_auth::common::verify::DomainKey::parse(record.as_bytes()).unwrap(),
        )),
    );
    cache.seed(
        &format!("{DOMAIN}."),
        Txt::Spf(Arc::new(
            mail_auth::spf::Spf::parse(b"v=spf1 ip4:192.0.2.25 -all").unwrap(),
        )),
    );
    if let Some(policy) = dmarc {
        cache.seed(
            &format!("_dmarc.{DOMAIN}."),
            Txt::Dmarc(Arc::new(
                mail_auth::dmarc::Dmarc::parse(policy.as_bytes()).unwrap(),
            )),
        );
    } else {
        // The tree walk climbs to the top-level domain; answer every step.
        cache.seed(&format!("_dmarc.{DOMAIN}."), not_found());
        cache.seed("_dmarc.invalid.", not_found());
    }
    cache
}

fn signed(key: &std::path::Path) -> Vec<u8> {
    let keys = SigningKeys::load(&[listmngr_core::DkimSigningConfig {
        domain: DOMAIN.into(),
        selector: "fixture".into(),
        private_key_file: key.to_path_buf(),
    }])
    .unwrap();
    let raw = format!(
        "Received: from mail.{DOMAIN} (mail.{DOMAIN} [192.0.2.25])\r\n\tby mx.example.invalid (Postfix) with ESMTPS id X\r\n\tfor <dev@example.invalid>; Mon, 1 Sep 2026 10:00:00 +0000\r\nFrom: Alice <alice@{DOMAIN}>\r\nTo: dev@example.invalid\r\nSubject: signed\r\nMessage-ID: <signed@{DOMAIN}>\r\nDate: Mon, 1 Sep 2026 10:00:00 +0000\r\n\r\nhello list\r\n"
    );
    keys.sign(DOMAIN, raw.into_bytes()).unwrap()
}

fn client() -> Client {
    Client {
        ip: "192.0.2.25".parse().unwrap(),
        helo: Some(format!("mail.{DOMAIN}")),
    }
}

#[tokio::test]
async fn a_properly_signed_message_passes_every_check_and_a_reject_policy_is_restrictive() {
    let dir = tempfile::tempdir().unwrap();
    let (key, record) = fixture_key(dir.path());
    let raw = signed(&key);
    let verifier = Verifier::system("mx.example.invalid")
        .unwrap()
        .with_txt_cache(seeded(&record, Some("v=DMARC1; p=reject")));
    let verdict = verifier
        .verify(&raw, Some(&format!("alice@{DOMAIN}")), Some(&client()))
        .await;
    let header = verdict.header.expect("header");
    assert!(header.starts_with("mx.example.invalid;"), "{header}");
    assert!(
        header.contains("dkim=pass header.d=sender.invalid header.s=fixture"),
        "{header}"
    );
    assert!(header.contains("spf=pass"), "{header}");
    assert!(
        header.contains("smtp.mailfrom=alice@sender.invalid"),
        "{header}"
    );
    assert!(header.contains("dmarc=pass"), "{header}");
    assert!(
        !header.contains('\n'),
        "the value is unfolded for the pipeline: {header:?}"
    );
    assert!(verdict.dmarc_policy_restrictive);
    assert_eq!(verdict.dmarc_domain.as_deref(), Some(DOMAIN));

    // A quarantine policy is restrictive too; `none` is not.
    for (policy, restrictive) in [
        ("v=DMARC1; p=quarantine", true),
        ("v=DMARC1; p=none", false),
    ] {
        let verifier = Verifier::system("mx.example.invalid")
            .unwrap()
            .with_txt_cache(seeded(&record, Some(policy)));
        let verdict = verifier
            .verify(&raw, Some(&format!("alice@{DOMAIN}")), Some(&client()))
            .await;
        assert_eq!(verdict.dmarc_policy_restrictive, restrictive, "{policy}");
    }
}

#[tokio::test]
async fn a_tampered_body_fails_dkim_and_an_unknown_client_skips_spf() {
    let dir = tempfile::tempdir().unwrap();
    let (key, record) = fixture_key(dir.path());
    let mut raw = signed(&key);
    let body = raw.windows(10).position(|w| w == b"hello list").unwrap();
    raw[body] = b'J';
    let verifier = Verifier::system("mx.example.invalid")
        .unwrap()
        .with_txt_cache(seeded(&record, Some("v=DMARC1; p=reject")));
    let verdict = verifier
        .verify(&raw, Some(&format!("alice@{DOMAIN}")), None)
        .await;
    let header = verdict.header.expect("header");
    // mail-auth reports a body-hash mismatch as `neutral` with the reason.
    assert!(header.contains("body hash did not verify"), "{header}");
    assert!(!header.contains("dkim=pass"), "{header}");
    assert!(!header.contains("spf="), "no client, no SPF: {header}");
    assert!(!header.contains("dmarc=pass"), "{header}");
    assert!(header.contains("policy.dmarc=reject"), "{header}");
    assert!(
        verdict.dmarc_policy_restrictive,
        "the policy is a fact about the domain, not the message"
    );
}

#[tokio::test]
async fn a_domain_without_a_policy_is_never_mitigated() {
    let dir = tempfile::tempdir().unwrap();
    let (key, record) = fixture_key(dir.path());
    let raw = signed(&key);
    let verifier = Verifier::system("mx.example.invalid")
        .unwrap()
        .with_txt_cache(seeded(&record, None));
    let verdict = verifier
        .verify(&raw, Some(&format!("alice@{DOMAIN}")), Some(&client()))
        .await;
    let header = verdict.header.expect("header");
    assert!(header.contains("dkim=pass"), "{header}");
    assert!(header.contains("dmarc=none"), "{header}");
    assert!(!verdict.dmarc_policy_restrictive);
    assert_eq!(
        verifier.verify(b"not a message", None, None).await.header,
        None
    );
}
