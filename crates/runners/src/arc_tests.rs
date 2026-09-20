//! `[mta.arc]`: the out runner seals every list delivery after the list's
//! own DKIM signature, carrying what the intake recorded, and a receiver
//! verifies the chain.
use super::*;
use listmngr_mail::arc::{Chain, Sealer};
use listmngr_mail::authenticity::{TxtCache, Verifier};
use listmngr_mail::dkim::SigningKeys;
use mail_auth::common::parse::TxtRecordParser;
use mail_auth::hickory_resolver::proto::op::ResponseCode;
use mail_auth::{DnsError, Txt};
use std::sync::Arc;

const RESULTS: &str = "mx.example.invalid; dkim=pass header.d=foreign.invalid header.s=fixture; dmarc=pass header.from=foreign.invalid policy.dmarc=none";

fn sealing_config(dkim_key: &std::path::Path, arc_key: &std::path::Path) -> listmngr_core::Config {
    serde_json::from_value(serde_json::json!({"mta":{
        "smtp_tls":"plaintext_trusted_relay",
        "authenticity_checks": true,
        "dkim_signing":[{"domain":"example.invalid","selector":"fixture","private_key_file":dkim_key}],
        "arc":{"enabled":true,"domain":"example.invalid","selector":"arc","private_key_file":arc_key}
    }}))
    .unwrap()
}

/// The public record of a key file, as DNS would publish it.
fn record(key: &std::path::Path, selector: &str) -> String {
    SigningKeys::dns_record(&listmngr_core::DkimSigningConfig {
        domain: "example.invalid".into(),
        selector: selector.into(),
        private_key_file: key.to_path_buf(),
    })
    .unwrap()
    .1
}

const fn not_found() -> Txt {
    Txt::Error(mail_auth::Error::Dns(DnsError::RecordNotFound(
        ResponseCode::NXDomain,
    )))
}

/// A receiver's verifier answering every lookup from the cache.
fn receiver(keys: &[(&str, &str)]) -> Verifier {
    let cache = TxtCache::default();
    for (name, record) in keys {
        cache.seed(
            name,
            Txt::DomainKey(Arc::new(
                mail_auth::common::verify::DomainKey::parse(record.as_bytes()).unwrap(),
            )),
        );
    }
    cache.seed("_dmarc.foreign.invalid.", not_found());
    cache.seed("_dmarc.invalid.", not_found());
    Verifier::system("receiver.invalid")
        .unwrap()
        .with_txt_cache(cache)
        .verifying_arc(true)
}

fn tag(header: &str, name: &str) -> String {
    header
        .split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix(&format!("{name}=")))
        .unwrap_or_else(|| panic!("{name}= in {header}"))
        .to_owned()
}

async fn delivered(role: &mut MailRoleConfig, raw: &[u8], context: &str) -> Vec<u8> {
    let (db, lease, original, sink) = tests::fixture_at(
        "sqlite::memory:",
        context,
        raw,
        vec!["member@example.invalid".into()],
    )
    .await;
    role.smtp_relay = original.smtp_relay;
    role.command_timeout = Duration::from_secs(2);
    let (sent, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(
            dkim_tests::capture(
                &sink,
                "test-bounces@example.invalid",
                "member@example.invalid"
            ),
            deliver_one(&db, role, lease.clone())
        )
    })
    .await
    .unwrap();
    assert_eq!(
        db.mail_queue()
            .message(lease.job.message_id)
            .await
            .unwrap()
            .raw,
        raw,
        "the stored post is untouched"
    );
    sent
}

#[tokio::test]
async fn a_sealing_site_seals_each_delivery_after_its_dkim_signature() {
    let dir = tempfile::tempdir().unwrap();
    let dkim_key = dkim_tests::key(dir.path());
    let arc_key = dir.path().join("arc.pem");
    std::fs::copy(&dkim_key, &arc_key).unwrap();
    let mut role = MailRoleConfig::from_core(&sealing_config(&dkim_key, &arc_key)).unwrap();
    assert!(role.arc.is_some());
    let raw = b"From: Author <author@foreign.invalid>\r\nTo: test@example.invalid\r\nSubject: ARC tracer\r\nMessage-ID: <arc@foreign.invalid>\r\nDate: Mon, 1 Sep 2026 10:00:00 +0000\r\n\r\ncontrolled body\r\n";
    let context = serde_json::json!({
        "list_id": "test.example.invalid",
        "authentication_results": RESULTS,
        "arc_chain": "none"
    })
    .to_string();
    let sent = delivered(&mut role, raw, &context).await;
    // The set is the outermost header block: i=1, cv=none, the intake's
    // results as the AAR, and the signature covering the list's DKIM one.
    let text = String::from_utf8_lossy(&sent);
    assert!(text.starts_with("ARC-Seal:"), "{text}");
    let seal = listmngr_mail::header_value(&sent, "ARC-Seal").unwrap();
    assert_eq!(tag(&seal, "i"), "1");
    assert_eq!(tag(&seal, "cv"), "none");
    assert_eq!(tag(&seal, "d"), "example.invalid");
    assert_eq!(tag(&seal, "s"), "arc");
    assert_eq!(
        listmngr_mail::header_value(&sent, "ARC-Authentication-Results").as_deref(),
        Some(format!("i=1; {RESULTS}").as_str())
    );
    let signature = listmngr_mail::header_value(&sent, "ARC-Message-Signature").unwrap();
    assert!(
        tag(&signature, "h").contains("DKIM-Signature"),
        "{signature}"
    );
    assert!(listmngr_mail::header_value(&sent, "DKIM-Signature").is_some());
    // A receiver verifies the list's signature and the chain.
    let verdict = receiver(&[
        (
            "fixture._domainkey.example.invalid.",
            &record(&dkim_key, "fixture"),
        ),
        ("arc._domainkey.example.invalid.", &record(&arc_key, "arc")),
    ])
    .verify(&sent, Some("test-bounces@example.invalid"), None)
    .await;
    assert_eq!(verdict.arc_chain, Some(Chain::Pass), "{:?}", verdict.header);
    let header = verdict.header.unwrap();
    assert!(
        header.contains("dkim=pass header.d=example.invalid header.s=fixture"),
        "{header}"
    );
}

#[tokio::test]
async fn a_chain_that_arrived_valid_is_kept_through_the_pipeline_and_extended() {
    let dir = tempfile::tempdir().unwrap();
    let dkim_key = dkim_tests::key(dir.path());
    let arc_key = dir.path().join("arc.pem");
    std::fs::copy(&dkim_key, &arc_key).unwrap();
    // An earlier sealer's set on the post as it arrived.
    let earlier_key = dir.path().join("earlier.pem");
    std::fs::copy(&dkim_key, &earlier_key).unwrap();
    let earlier = Sealer::load(&listmngr_core::ArcConfig {
        enabled: true,
        domain: "earlier.invalid".into(),
        selector: "one".into(),
        private_key_file: Some(earlier_key.clone()),
    })
    .unwrap()
    .unwrap();
    let post = b"From: Author <author@foreign.invalid>\r\nTo: test@example.invalid\r\nSubject: ARC tracer\r\nMessage-ID: <chain@foreign.invalid>\r\nDate: Mon, 1 Sep 2026 10:00:00 +0000\r\n\r\ncontrolled body\r\n";
    let arrived = earlier
        .seal(
            post,
            "earlier.invalid; dkim=pass header.d=foreign.invalid",
            Chain::None,
        )
        .unwrap();
    let mut role = MailRoleConfig::from_core(&sealing_config(&dkim_key, &arc_key)).unwrap();
    let context = serde_json::json!({
        "list_id": "test.example.invalid",
        "authentication_results": format!("{RESULTS}; arc=pass smtp.remote-ip=192.0.2.25"),
        "arc_chain": "pass"
    })
    .to_string();
    let sent = delivered(&mut role, &arrived, &context).await;
    let text = String::from_utf8_lossy(&sent);
    assert_eq!(text.matches("ARC-Seal:").count(), 2, "{text}");
    let seal = listmngr_mail::header_value(&sent, "ARC-Seal").unwrap();
    assert_eq!(tag(&seal, "i"), "2");
    assert_eq!(tag(&seal, "cv"), "pass");
    assert_eq!(tag(&seal, "d"), "example.invalid");
    assert!(
        text.contains("d=earlier.invalid"),
        "the earlier set travels: {text}"
    );
    let verdict = receiver(&[
        (
            "fixture._domainkey.example.invalid.",
            &record(&dkim_key, "fixture"),
        ),
        ("arc._domainkey.example.invalid.", &record(&arc_key, "arc")),
        (
            "one._domainkey.earlier.invalid.",
            &record(&earlier_key, "one"),
        ),
    ])
    .verify(&sent, Some("test-bounces@example.invalid"), None)
    .await;
    assert_eq!(verdict.arc_chain, Some(Chain::Pass), "{:?}", verdict.header);
    // Without the intake's record — a post that did not pass the checks,
    // say one injected — the delivery is not sealed, and the chain it
    // carried is dropped as any stale signature.
    let mut role = MailRoleConfig::from_core(&sealing_config(&dkim_key, &arc_key)).unwrap();
    let sent = delivered(
        &mut role,
        &arrived,
        "{\"list_id\":\"test.example.invalid\"}",
    )
    .await;
    let text = String::from_utf8_lossy(&sent);
    assert!(!text.contains("ARC-Seal:"), "{text}");
    assert!(listmngr_mail::header_value(&sent, "DKIM-Signature").is_some());
}
