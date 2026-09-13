//! Export only synthetic SMTP captures and public keys for independent verification.
use base64::Engine;
use std::path::Path;

pub fn export_capture(name: &str, message: &[u8], key: &Path, domain: &str) {
    assert!(!name.is_empty());
    assert!(name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'));
    let public = std::process::Command::new("openssl")
        .args(["pkey", "-in"])
        .arg(key)
        .args(["-pubout", "-outform", "DER"])
        .output()
        .unwrap();
    assert!(
        public.status.success(),
        "owned public key extraction failed"
    );
    let record = format!(
        "v=DKIM1; k=rsa; p={}\n",
        base64::engine::general_purpose::STANDARD.encode(public.stdout)
    );
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/dkim-evidence/captures");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join(format!("{name}.eml")), message).unwrap();
    std::fs::write(root.join(format!("{name}.dns.txt")), record).unwrap();
    std::fs::write(
        root.join(format!("{name}.json")),
        serde_json::to_vec(&serde_json::json!({
            "domain": domain, "selector": "fixture", "source": "executed TCP SMTP fixture"
        }))
        .unwrap(),
    )
    .unwrap();
}
