//! Mailman's `arc-sign`: a delivery sealed with the site's key carries an
//! ARC set a receiver verifies; a chain that arrived valid is extended,
//! one that arrived broken is closed with `cv=fail`.
use base64::Engine;
use listmngr_core::ArcConfig;
use listmngr_mail::arc::{Chain, Sealer};
use listmngr_mail::authenticity::{Client, TxtCache, Verifier};
use listmngr_mail::header_value;
use mail_auth::common::parse::TxtRecordParser;
use mail_auth::hickory_resolver::proto::op::ResponseCode;
use mail_auth::{DnsError, Error, Txt};
use std::sync::Arc;

const SITE: &str = "lists.example.invalid";
const SENDER: &str = "sender.invalid";
const RESULTS: &str = "mx.example.invalid; dkim=pass header.d=sender.invalid header.s=fixture; spf=pass smtp.mailfrom=alice@sender.invalid; dmarc=pass header.from=sender.invalid policy.dmarc=none";

fn fixture_key(dir: &std::path::Path, name: &str) -> (std::path::PathBuf, String) {
    let path = dir.join(name);
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
    assert!(generated.status.success());
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
    (
        path,
        format!(
            "v=DKIM1; k=rsa; p={}",
            base64::engine::general_purpose::STANDARD.encode(public.stdout)
        ),
    )
}

fn config(key: &std::path::Path) -> ArcConfig {
    ArcConfig {
        enabled: true,
        domain: SITE.into(),
        selector: "arc".into(),
        private_key_file: Some(key.to_path_buf()),
    }
}

const fn not_found() -> Txt {
    Txt::Error(Error::Dns(DnsError::RecordNotFound(ResponseCode::NXDomain)))
}

/// A cache answering the site's ARC key and everything the other checks
/// walk, so no query leaves the process.
fn seeded(site_record: &str) -> TxtCache {
    let cache = TxtCache::default();
    cache.seed(
        &format!("arc._domainkey.{SITE}."),
        Txt::DomainKey(Arc::new(
            mail_auth::common::verify::DomainKey::parse(site_record.as_bytes()).unwrap(),
        )),
    );
    cache.seed(&format!("{SENDER}."), not_found());
    cache.seed(&format!("_dmarc.{SENDER}."), not_found());
    cache.seed("_dmarc.invalid.", not_found());
    cache
}

fn post() -> Vec<u8> {
    format!(
        "Received: from mail.{SENDER} (mail.{SENDER} [192.0.2.25])\r\n\tby mx.example.invalid (Postfix) with ESMTPS id X\r\n\tfor <dev@example.invalid>; Mon, 1 Sep 2026 10:00:00 +0000\r\nFrom: Alice <alice@{SENDER}>\r\nTo: dev@example.invalid\r\nSubject: [dev] sealed\r\nMessage-ID: <sealed@{SENDER}>\r\nDate: Mon, 1 Sep 2026 10:00:00 +0000\r\nList-Id: <dev.example.invalid>\r\nList-Post: <mailto:dev@example.invalid>\r\n\r\nhello list\r\n"
    )
    .into_bytes()
}

fn client() -> Client {
    Client {
        ip: "192.0.2.25".parse().unwrap(),
        helo: Some(format!("mail.{SENDER}")),
    }
}

/// The parameters of the first (newest) `ARC-Seal`.
fn seal_of(bytes: &[u8]) -> String {
    header_value(bytes, "ARC-Seal").expect("an ARC-Seal")
}

fn tag(header: &str, name: &str) -> String {
    header
        .split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix(&format!("{name}=")))
        .unwrap_or_else(|| panic!("{name}= in {header}"))
        .to_owned()
}

#[tokio::test]
async fn a_delivery_is_sealed_with_the_site_key_and_a_receiver_verifies_the_chain() {
    let dir = tempfile::tempdir().unwrap();
    let (key, record) = fixture_key(dir.path(), "arc.pem");
    let site = Sealer::load(&config(&key)).unwrap().expect("enabled");
    let sealed = site.seal(&post(), RESULTS, Chain::None).unwrap();
    // The set: i=1, cv=none, d= the site, s= the selector, the site's
    // results as the AAR, headers in the seal's order.
    let seal = seal_of(&sealed);
    assert_eq!(tag(&seal, "i"), "1");
    assert_eq!(tag(&seal, "cv"), "none");
    assert_eq!(tag(&seal, "d"), SITE);
    assert_eq!(tag(&seal, "s"), "arc");
    let signature = header_value(&sealed, "ARC-Message-Signature").unwrap();
    assert_eq!(tag(&signature, "i"), "1");
    assert_eq!(tag(&signature, "d"), SITE);
    assert!(
        tag(&signature, "h").contains("From:From"),
        "From oversigned: {signature}"
    );
    assert!(tag(&signature, "h").contains("List-Id"), "{signature}");
    assert_eq!(
        header_value(&sealed, "ARC-Authentication-Results").as_deref(),
        Some(format!("i=1; {RESULTS}").as_str())
    );
    assert!(sealed.ends_with(&post()), "the message itself is untouched");
    let text = String::from_utf8_lossy(&sealed);
    assert!(
        text.starts_with("ARC-Seal:") && text.contains("\r\nARC-Message-Signature:"),
        "{text}"
    );
    // A receiver: the chain verifies against the published key, and the
    // verdict is what the next sealer would carry.
    let verifier = Verifier::system("next.example.invalid")
        .unwrap()
        .with_txt_cache(seeded(&record))
        .verifying_arc(true);
    let verdict = verifier
        .verify(&sealed, Some(&format!("alice@{SENDER}")), Some(&client()))
        .await;
    assert_eq!(verdict.arc_chain, Some(Chain::Pass));
    let header = verdict.header.unwrap();
    assert!(
        header.contains("arc=pass smtp.remote-ip=192.0.2.25"),
        "{header}"
    );
    // The same delivery seals to the same set, but for the timestamps.
    let again = site.seal(&post(), RESULTS, Chain::None).unwrap();
    assert_eq!(
        tag(&seal_of(&again), "d"),
        SITE,
        "{}",
        String::from_utf8_lossy(&again)
    );
}

#[tokio::test]
async fn a_valid_chain_is_extended_and_a_broken_one_is_closed() {
    let dir = tempfile::tempdir().unwrap();
    let (first_key, first_record) = fixture_key(dir.path(), "first.pem");
    let (key, record) = fixture_key(dir.path(), "arc.pem");
    // An earlier intermediary sealed the post (i=1) with its own key.
    let earlier = Sealer::load(&ArcConfig {
        enabled: true,
        domain: "earlier.example.invalid".into(),
        selector: "one".into(),
        private_key_file: Some(first_key),
    })
    .unwrap()
    .unwrap();
    let arrived = earlier
        .seal(
            &post(),
            "earlier.example.invalid; dkim=pass header.d=sender.invalid",
            Chain::None,
        )
        .unwrap();
    assert_eq!(tag(&seal_of(&arrived), "i"), "1");
    // This site verified that chain at intake…
    let cache = seeded(&record);
    cache.seed(
        "one._domainkey.earlier.example.invalid.",
        Txt::DomainKey(Arc::new(
            mail_auth::common::verify::DomainKey::parse(first_record.as_bytes()).unwrap(),
        )),
    );
    let verifier = Verifier::system("mx.example.invalid")
        .unwrap()
        .with_txt_cache(cache)
        .verifying_arc(true);
    let verdict = verifier
        .verify(&arrived, Some(&format!("alice@{SENDER}")), Some(&client()))
        .await;
    assert_eq!(verdict.arc_chain, Some(Chain::Pass));
    // …the list changed the message, and the delivery is sealed as i=2
    // with cv=pass, the earlier set kept in front of the message.
    let mut changed = arrived.clone();
    changed.extend_from_slice(b"-- \r\nlist footer\r\n");
    let site = Sealer::load(&config(&key)).unwrap().unwrap();
    let sealed = site
        .seal(&changed, &verdict.header.clone().unwrap(), Chain::Pass)
        .unwrap();
    let seal = seal_of(&sealed);
    assert_eq!(tag(&seal, "i"), "2");
    assert_eq!(tag(&seal, "cv"), "pass");
    assert_eq!(tag(&seal, "d"), SITE);
    assert!(
        String::from_utf8_lossy(&sealed)
            .matches("ARC-Seal:")
            .count()
            == 2,
        "{}",
        String::from_utf8_lossy(&sealed)
    );
    assert!(
        header_value(&sealed, "ARC-Authentication-Results")
            .unwrap()
            .starts_with("i=2; mx.example.invalid;"),
        "{}",
        String::from_utf8_lossy(&sealed)
    );
    // A receiver with both keys verifies the two-set chain.
    let cache = seeded(&record);
    cache.seed(
        "one._domainkey.earlier.example.invalid.",
        Txt::DomainKey(Arc::new(
            mail_auth::common::verify::DomainKey::parse(first_record.as_bytes()).unwrap(),
        )),
    );
    let receiver = Verifier::system("next.example.invalid")
        .unwrap()
        .with_txt_cache(cache)
        .verifying_arc(true);
    let verdict = receiver
        .verify(&sealed, Some(&format!("alice@{SENDER}")), Some(&client()))
        .await;
    assert_eq!(verdict.arc_chain, Some(Chain::Pass), "{:?}", verdict.header);
    // A chain the intake found broken is closed: sealed once with cv=fail,
    // and never extended after that.
    let closed = site.seal(&changed, RESULTS, Chain::Fail).unwrap();
    assert_eq!(tag(&seal_of(&closed), "cv"), "fail");
    assert_eq!(tag(&seal_of(&closed), "i"), "2");
    let after = site.seal(&closed, RESULTS, Chain::Fail).unwrap();
    assert_eq!(after, closed, "a failed chain is not sealed again");
}

#[test]
fn the_sealer_is_off_by_default_and_refuses_a_bad_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let (key, _) = fixture_key(dir.path(), "arc.pem");
    assert!(
        Sealer::load(&ArcConfig::default()).unwrap().is_none(),
        "disabled"
    );
    let mut missing = config(&key);
    missing.private_key_file = None;
    assert!(Sealer::load(&missing).is_err());
    let mut bad_domain = config(&key);
    bad_domain.domain = "Not a domain".into();
    assert!(Sealer::load(&bad_domain).is_err());
    let mut bad_selector = config(&key);
    bad_selector.selector = "arc key".into();
    assert!(Sealer::load(&bad_selector).is_err());
    let mut no_key = config(&key);
    no_key.private_key_file = Some(dir.path().join("missing.pem"));
    assert!(Sealer::load(&no_key).is_err());
    // The configuration itself: sealing needs the checks whose results it
    // carries.
    let mut mta = listmngr_core::MtaConfig {
        arc: config(&key),
        ..Default::default()
    };
    assert!(mta.validate().is_err(), "without authenticity_checks");
    mta.authenticity_checks = true;
    assert!(mta.validate().is_ok());
    mta.arc.selector.clear();
    assert!(mta.validate().is_err(), "without a selector");
}
