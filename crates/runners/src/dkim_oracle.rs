//! Same-library cryptographic oracle; the public capture enables an independent parent oracle.
#[path = "../tests/support/dkim_capture.rs"]
mod capture_export;
use base64::Engine;
pub use capture_export::export_capture;
use mail_auth::common::{parse::TxtRecordParser, verify::DomainKey};
use mail_auth::{
    AuthenticatedMessage, DkimResult, MessageAuthenticator, Parameters, ResolverCache, Txt,
};
use std::{borrow::Borrow, collections::HashMap, hash::Hash, path::Path};

struct ControlledDns(HashMap<Box<str>, Txt>);
impl ResolverCache<Box<str>, Txt> for ControlledDns {
    fn get<Q>(&self, name: &Q) -> Option<Txt>
    where
        Box<str>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        // A miss panics before the library can fall back to any real DNS.
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

pub fn public_txt(path: &Path) -> String {
    let output = std::process::Command::new("openssl")
        .args(["pkey", "-in"])
        .arg(path)
        .args(["-pubout", "-outform", "DER"])
        .output()
        .unwrap();
    assert!(output.status.success());
    format!(
        "v=DKIM1; k=rsa; p={}",
        base64::engine::general_purpose::STANDARD.encode(output.stdout)
    )
}

pub async fn passes(bytes: &[u8], txt: &str) -> bool {
    let key: Txt = DomainKey::parse(txt.as_bytes()).unwrap().into();
    let dns = ControlledDns(HashMap::from([
        (
            Box::from("fixture._domainkey.example.invalid."),
            key.clone(),
        ),
        (Box::from("fixture._domainkey.example.com."), key.clone()),
        (Box::from("fixture._domainkey.foreign.invalid."), key),
    ]));
    let resolver = MessageAuthenticator::new(
        mail_auth::hickory_resolver::config::ResolverConfig::default(),
        mail_auth::hickory_resolver::config::ResolverOpts::default(),
    )
    .unwrap();
    let message = AuthenticatedMessage::parse(bytes).unwrap();
    let outputs = resolver
        .verify_dkim(Parameters::new(&message).with_txt_cache(&dns))
        .await;
    outputs.len() == 1 && matches!(outputs[0].result(), DkimResult::Pass)
}

pub(super) async fn verify_capture(sent: &[u8], path: &Path) {
    let record = public_txt(path);
    assert!(
        passes(sent, &record).await,
        "captured SMTP signature must verify"
    );
    let text = String::from_utf8(sent.to_vec()).unwrap();
    for altered in [
        text.replace("controlled body", "tampered body"),
        text.replace(
            "Author <author@foreign.invalid>",
            "Attacker <attacker@foreign.invalid>",
        ),
        text.replace("d=example.invalid", "d=foreign.invalid"),
        format!("From: attacker@foreign.invalid\r\n{text}"),
    ] {
        assert!(
            !passes(altered.as_bytes(), &record).await,
            "tampered signed content verified"
        );
    }
    let other = tempfile::tempdir().unwrap();
    let wrong = super::key(other.path());
    assert!(!passes(sent, &public_txt(&wrong)).await);
    export_capture("ordinary", sent, path, "example.invalid");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/dkim-evidence");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("ordinary.eml"), sent).unwrap();
    std::fs::write(root.join("public-key.txt"), format!("{record}\n")).unwrap();
    std::fs::write(
        root.join("identity.txt"),
        "domain=example.invalid\nselector=fixture\nquery=fixture._domainkey.example.invalid\n",
    )
    .unwrap();
}
