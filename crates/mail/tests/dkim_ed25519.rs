//! Ed25519 beside RSA: a generated Ed25519 key signs `ed25519-sha256`, two
//! selectors of one domain sign side by side and both verify against the
//! records the keys publish, a key of another kind is refused, and a
//! repeated selector is refused.
#![cfg(unix)]
use listmngr_core::DkimSigningConfig;
use listmngr_mail::dkim::{Algorithm, SigningKeys, algorithm_of, generate_key, record_value};
use mail_auth::common::{parse::TxtRecordParser, verify::DomainKey};
use mail_auth::{
    AuthenticatedMessage, DkimResult, MessageAuthenticator, Parameters, ResolverCache, Txt,
};
use std::os::unix::fs::PermissionsExt as _;
use std::{borrow::Borrow, collections::HashMap, hash::Hash, path::Path};

struct ControlledDns(HashMap<Box<str>, Txt>);
impl ResolverCache<Box<str>, Txt> for ControlledDns {
    fn get<Q>(&self, name: &Q) -> Option<Txt>
    where
        Box<str>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        Some(
            self.0
                .get(name)
                .expect("unexpected DNS request; external DNS forbidden")
                .clone(),
        )
    }
    fn remove<Q>(&self, _: &Q) -> Option<Txt>
    where
        Box<str>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        panic!("unexpected cache remove")
    }
    fn insert(&self, _: Box<str>, _: Txt, _: std::time::Instant) {
        panic!("external DNS reached")
    }
}

fn write_private(path: &Path, pem: &str) {
    std::fs::write(path, pem).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

fn openssl_key(path: &Path, args: &[&str]) {
    let status = std::process::Command::new("openssl")
        .args(["genpkey", "-algorithm"])
        .args(args)
        .arg("-out")
        .arg(path)
        .status()
        .expect("openssl");
    assert!(status.success());
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

fn entry(domain: &str, selector: &str, path: &Path) -> DkimSigningConfig {
    DkimSigningConfig {
        domain: domain.into(),
        selector: selector.into(),
        private_key_file: path.to_path_buf(),
    }
}

const MESSAGE: &[u8] = b"From: author@example.invalid\r\nTo: dev@example.invalid\r\nSubject: signed twice\r\nDate: Tue, 2 Sep 2026 09:00:00 +0000\r\nMessage-ID: <twice@example.invalid>\r\n\r\nbody\r\n";

async fn results(signed: &[u8], records: &[(&str, &str)]) -> Vec<DkimResult> {
    let dns = ControlledDns(
        records
            .iter()
            .map(|(name, txt)| {
                let key: Txt = DomainKey::parse(txt.as_bytes()).unwrap().into();
                (Box::from(*name), key)
            })
            .collect(),
    );
    let resolver = MessageAuthenticator::new(
        mail_auth::hickory_resolver::config::ResolverConfig::default(),
        mail_auth::hickory_resolver::config::ResolverOpts::default(),
    )
    .unwrap();
    let message = AuthenticatedMessage::parse(signed).unwrap();
    resolver
        .verify_dkim(Parameters::new(&message).with_txt_cache(&dns))
        .await
        .into_iter()
        .map(|output| output.result().clone())
        .collect()
}

#[tokio::test]
async fn an_ed25519_key_and_an_rsa_key_sign_side_by_side_and_both_verify() {
    let dir = tempfile::tempdir().unwrap();
    let ed = dir.path().join("ed.pem");
    let ed_pem = generate_key(Algorithm::Ed25519, 0).unwrap();
    write_private(&ed, &ed_pem);
    let rsa = dir.path().join("rsa.pem");
    openssl_key(&rsa, &["RSA", "-pkeyopt", "rsa_keygen_bits:2048"]);
    let rsa_pem = std::fs::read(&rsa).unwrap();
    assert_eq!(algorithm_of(ed_pem.as_bytes()).unwrap(), Algorithm::Ed25519);
    assert_eq!(algorithm_of(&rsa_pem).unwrap(), Algorithm::Rsa);
    let ed_record = record_value(ed_pem.as_bytes()).unwrap();
    assert!(
        ed_record.starts_with("v=DKIM1; k=ed25519; p="),
        "{ed_record}"
    );
    {
        use base64::Engine as _;
        let p = ed_record.rsplit("p=").next().unwrap();
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(p)
                .unwrap()
                .len(),
            32
        );
    }
    let rsa_record = record_value(&rsa_pem).unwrap();
    assert!(rsa_record.starts_with("v=DKIM1; k=rsa; p="));

    let keys = SigningKeys::load(&[
        entry("example.invalid", "e", &ed),
        entry("example.invalid", "r", &rsa),
    ])
    .unwrap();
    let signed = keys.sign("example.invalid", MESSAGE.to_vec()).unwrap();
    let text = String::from_utf8_lossy(&signed);
    assert_eq!(text.matches("DKIM-Signature:").count(), 2, "{text}");
    assert!(text.contains("a=ed25519-sha256"), "{text}");
    assert!(text.contains("a=rsa-sha256"), "{text}");
    assert!(text.ends_with("body\r\n"));
    let verdicts = results(
        &signed,
        &[
            ("e._domainkey.example.invalid.", ed_record.as_str()),
            ("r._domainkey.example.invalid.", rsa_record.as_str()),
        ],
    )
    .await;
    assert_eq!(verdicts.len(), 2, "{verdicts:?}");
    assert!(
        verdicts.iter().all(|v| matches!(v, DkimResult::Pass)),
        "{verdicts:?}"
    );
    // Another domain is untouched; one selector alone signs once.
    assert_eq!(
        keys.sign("other.invalid", MESSAGE.to_vec()).unwrap(),
        MESSAGE.to_vec()
    );
    let alone = SigningKeys::load(&[entry("example.invalid", "e", &ed)]).unwrap();
    let once = alone.sign("example.invalid", MESSAGE.to_vec()).unwrap();
    assert_eq!(
        String::from_utf8_lossy(&once)
            .matches("DKIM-Signature:")
            .count(),
        1
    );
    assert_eq!(
        results(
            &once,
            &[("e._domainkey.example.invalid.", ed_record.as_str())]
        )
        .await,
        vec![DkimResult::Pass]
    );
}

#[test]
fn other_key_kinds_repeated_selectors_and_small_rsa_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let ec = dir.path().join("ec.pem");
    openssl_key(&ec, &["EC", "-pkeyopt", "ec_paramgen_curve:P-256"]);
    assert!(SigningKeys::load(&[entry("example.invalid", "ec", &ec)]).is_err());
    assert!(record_value(&std::fs::read(&ec).unwrap()).is_err());
    let ed = dir.path().join("ed.pem");
    write_private(&ed, &generate_key(Algorithm::Ed25519, 0).unwrap());
    assert!(
        SigningKeys::load(&[
            entry("example.invalid", "same", &ed),
            entry("example.invalid", "same", &ed),
        ])
        .is_err(),
        "a repeated selector on one domain is a configuration error"
    );
    assert!(
        SigningKeys::load(&[
            entry("example.invalid", "a", &ed),
            entry("other.invalid", "a", &ed),
        ])
        .is_ok(),
        "the same selector on two domains is fine"
    );
    assert!(generate_key(Algorithm::Rsa, 1024).is_err());
    assert!(generate_key(Algorithm::Rsa, 8192).is_err());
}

#[test]
fn a_generated_rsa_key_loads_and_publishes_as_rsa() {
    let pem = generate_key(Algorithm::Rsa, 2048).unwrap();
    assert!(pem.starts_with("-----BEGIN PRIVATE KEY-----\n"));
    assert_eq!(algorithm_of(pem.as_bytes()).unwrap(), Algorithm::Rsa);
    assert!(
        record_value(pem.as_bytes())
            .unwrap()
            .starts_with("v=DKIM1; k=rsa; p=")
    );
}
