//! Opt-in outbound DKIM with RSA and Ed25519 keys.
//!
//! RSA (`rsa-sha256`) and Ed25519 (`ed25519-sha256`, RFC 8463) keys,
//! several selectors of one domain signing side by side, the DNS records
//! that publish them, and key generation for `listmngr dkim gen`. No
//! incoming authentication or DNS policy evaluation.
use base64::Engine as _;
use listmngr_core::{DkimSigningConfig, Error, Result};
use mail_auth::common::headers::HeaderWriter;
use mail_auth::{
    common::crypto::{DkimKey, Ed25519Key, RsaKey, Sha256},
    dkim::{Canonicalization, DkimSigner, Done},
};
use rustls_pki_types::{PrivateKeyDer, pem::PemObject};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    sync::Arc,
};

type Signer = DkimSigner<DkimKey, Done>;

/// The kind of key a PEM file holds, which decides the signature algorithm
/// and the `k=` tag of the DNS record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    Rsa,
    Ed25519,
}

impl Algorithm {
    /// The DNS record's `k=` tag.
    #[must_use]
    pub const fn dns_tag(self) -> &'static str {
        match self {
            Self::Rsa => "rsa",
            Self::Ed25519 => "ed25519",
        }
    }

    /// The signature's `a=` tag.
    #[must_use]
    pub const fn signature_name(self) -> &'static str {
        match self {
            Self::Rsa => "rsa-sha256",
            Self::Ed25519 => "ed25519-sha256",
        }
    }
}

/// `id-Ed25519` (RFC 8410).
const ED25519_OID: rsa::pkcs8::ObjectIdentifier =
    rsa::pkcs8::ObjectIdentifier::new_unwrap("1.3.101.112");

/// The headers every signature covers. A repeated From oversigns its
/// absence: prepending another From must invalidate the signature, not
/// merely leave the old one valid.
const SIGNED_HEADERS: [&str; 14] = [
    "From",
    "From",
    "To",
    "Subject",
    "Date",
    "Message-ID",
    "Reply-To",
    "MIME-Version",
    "Content-Type",
    "Content-Transfer-Encoding",
    "List-Id",
    "List-Post",
    "List-Unsubscribe",
    "Auto-Submitted",
];

/// Every selector of every signing domain, in configuration order.
#[derive(Clone, Default)]
pub struct SigningKeys(Arc<BTreeMap<String, Vec<Signer>>>);

impl std::fmt::Debug for SigningKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SigningKeys")
            .field("keys", &"[REDACTED]")
            .finish()
    }
}

pub(crate) fn invalid() -> Error {
    Error::Validation("invalid outbound DKIM signing configuration or message".into())
}

pub(crate) fn dns_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

/// A private key file: a regular file readable by nobody else, 64 KiB at most.
pub(crate) fn read_key_file(path: &std::path::Path) -> Result<Vec<u8>> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Do not block opening a FIFO before the descriptor type check.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|_| invalid())?;
    let metadata = file.metadata().map_err(|_| invalid())?;
    if !metadata.is_file() {
        return Err(invalid());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(invalid());
        }
    }
    let mut bytes = Vec::new();
    file.take(65_537)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid())?;
    if bytes.len() > 65_536 {
        return Err(invalid());
    }
    Ok(bytes)
}

/// A key read from PEM, with the public half as the DNS record publishes
/// it: the `SubjectPublicKeyInfo` DER of an RSA key, the 32 raw bytes of an
/// Ed25519 key.
struct Loaded {
    key: DkimKey,
    algorithm: Algorithm,
    public: Vec<u8>,
}

fn rsa_loaded(der: PrivateKeyDer<'static>) -> Result<Loaded> {
    use rsa::pkcs8::{DecodePrivateKey as _, EncodePublicKey as _};
    let private = match &der {
        PrivateKeyDer::Pkcs8(der) => {
            rsa::RsaPrivateKey::from_pkcs8_der(der.secret_pkcs8_der()).map_err(|_| invalid())
        }
        PrivateKeyDer::Pkcs1(der) => {
            use rsa::pkcs1::DecodeRsaPrivateKey as _;
            rsa::RsaPrivateKey::from_pkcs1_der(der.secret_pkcs1_der()).map_err(|_| invalid())
        }
        _ => Err(invalid()),
    }?;
    let public = private
        .to_public_key()
        .to_public_key_der()
        .map_err(|_| invalid())?
        .as_bytes()
        .to_vec();
    let key = RsaKey::<Sha256>::from_key_der(der).map_err(|_| invalid())?;
    Ok(Loaded {
        key: key.into(),
        algorithm: Algorithm::Rsa,
        public,
    })
}

/// Read a PEM private key: PKCS#1 is RSA; PKCS#8 is whatever its algorithm
/// identifier says, RSA or Ed25519, and nothing else.
fn load_key(pem: &[u8]) -> Result<Loaded> {
    let der = PrivateKeyDer::from_pem_slice(pem).map_err(|_| invalid())?;
    match &der {
        PrivateKeyDer::Pkcs1(_) => rsa_loaded(der),
        PrivateKeyDer::Pkcs8(pkcs8) => {
            let info = rsa::pkcs8::PrivateKeyInfo::try_from(pkcs8.secret_pkcs8_der())
                .map_err(|_| invalid())?;
            if info.algorithm.oid == rsa::pkcs1::ALGORITHM_OID {
                rsa_loaded(der)
            } else if info.algorithm.oid == ED25519_OID {
                let key = Ed25519Key::from_pkcs8_maybe_unchecked_der(pkcs8.secret_pkcs8_der())
                    .map_err(|_| invalid())?;
                let public = key.public_key();
                Ok(Loaded {
                    key: key.into(),
                    algorithm: Algorithm::Ed25519,
                    public,
                })
            } else {
                Err(invalid())
            }
        }
        _ => Err(invalid()),
    }
}

fn validate_names(entry: &DkimSigningConfig) -> Result<()> {
    if !dns_name(&entry.domain)
        || !entry.domain.contains('.')
        || !dns_name(&entry.selector)
        || entry.domain.len() + entry.selector.len() + "._domainkey.".len() > 253
    {
        return Err(invalid());
    }
    Ok(())
}

impl SigningKeys {
    /// Load operator key files once at runtime startup, never during config
    /// display. Several entries may share a domain when their selectors
    /// differ: each signs, in configuration order.
    /// # Errors
    /// Returns a redacted error for a bad name, a repeated selector, or an
    /// unreadable or unsupported key.
    pub fn load(config: &[DkimSigningConfig]) -> Result<Self> {
        let mut seen = BTreeSet::new();
        for entry in config {
            validate_names(entry)?;
            if !seen.insert((&entry.domain, &entry.selector)) {
                return Err(invalid());
            }
        }
        let mut keys: BTreeMap<String, Vec<Signer>> = BTreeMap::new();
        for entry in config {
            let loaded = load_key(&read_key_file(&entry.private_key_file)?)?;
            keys.entry(entry.domain.clone()).or_default().push(
                DkimSigner::from_key(loaded.key)
                    .domain(&entry.domain)
                    .selector(&entry.selector)
                    .headers(SIGNED_HEADERS)
                    // mail-auth 0.12.1 relaxed body hashing mishandles trailing
                    // whitespace-only lines. RFC simple hashing preserves MIME
                    // bytes and interoperates without that broken transformation.
                    .header_canonicalization(Canonicalization::Relaxed)
                    .body_canonicalization(Canonicalization::Simple),
            );
        }
        Ok(Self(Arc::new(keys)))
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The DNS TXT record that publishes the public half of one configured
    /// key: `(<selector>._domainkey.<domain>, "v=DKIM1; k=…; p=…")`. Reads
    /// the private key file with the same checks as [`Self::load`] and never
    /// returns private material.
    /// # Errors
    /// Returns a redacted error for a bad name or an unreadable or
    /// unsupported key.
    pub fn dns_record(entry: &DkimSigningConfig) -> Result<(String, String)> {
        validate_names(entry)?;
        let bytes = read_key_file(&entry.private_key_file)?;
        Ok((
            format!("{}._domainkey.{}", entry.selector, entry.domain),
            record_value(&bytes)?,
        ))
    }

    /// Sign final cooked bytes for an authoritative stored list mail host,
    /// one `DKIM-Signature` per configured selector of the domain, each
    /// over the message as it was before any of them.
    /// # Errors
    /// Returns a redacted local error, never an SMTP/mailbox failure.
    pub fn sign(&self, domain: &str, bytes: Vec<u8>) -> Result<Vec<u8>> {
        let Some(signers) = self.0.get(domain) else {
            return Ok(bytes);
        };
        let bytes = smtp_bytes(&bytes)?;
        let boundary = bytes
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or_else(invalid)?;
        let from_count = bytes[..boundary]
            .split(|b| *b == b'\n')
            .filter(|line| {
                line.split(|b| *b == b':')
                    .next()
                    .is_some_and(|name| name.eq_ignore_ascii_case(b"From"))
            })
            .count();
        if from_count != 1
            || crate::header_value(&bytes, "From").is_none_or(|v| v.trim().is_empty())
        {
            return Err(invalid());
        }
        let mut output = Vec::with_capacity(bytes.len() + 512 * signers.len());
        for signer in signers {
            let signature = signer.sign(&bytes).map_err(|_| invalid())?;
            output.extend_from_slice(signature.to_header().as_bytes());
        }
        output.extend_from_slice(&bytes);
        Ok(output)
    }
}

/// The TXT value that publishes the public half of a PEM private key:
/// `v=DKIM1; k=rsa; p=<base64 SubjectPublicKeyInfo>` or
/// `v=DKIM1; k=ed25519; p=<base64 public key>`.
/// # Errors
/// Returns a redacted error for an unreadable or unsupported key.
pub fn record_value(pem: &[u8]) -> Result<String> {
    let loaded = load_key(pem)?;
    Ok(format!(
        "v=DKIM1; k={}; p={}",
        loaded.algorithm.dns_tag(),
        base64::engine::general_purpose::STANDARD.encode(&loaded.public)
    ))
}

/// The algorithm of a PEM private key.
/// # Errors
/// Returns a redacted error for an unreadable or unsupported key.
pub fn algorithm_of(pem: &[u8]) -> Result<Algorithm> {
    load_key(pem).map(|loaded| loaded.algorithm)
}

/// A new private key as PKCS#8 PEM, for `listmngr dkim gen`: Ed25519 from
/// the system's random source, or RSA of `rsa_bits` (2048 to 4096).
/// # Errors
/// Returns a redacted error for an RSA size outside the range or a failed
/// generation.
pub fn generate_key(algorithm: Algorithm, rsa_bits: u32) -> Result<String> {
    match algorithm {
        Algorithm::Ed25519 => {
            let der = Ed25519Key::generate_pkcs8().map_err(|_| invalid())?;
            Ok(pem("PRIVATE KEY", &der))
        }
        Algorithm::Rsa => {
            use rsa::pkcs8::EncodePrivateKey as _;
            if !(2048..=4096).contains(&rsa_bits) {
                return Err(invalid());
            }
            let bits = usize::try_from(rsa_bits).map_err(|_| invalid())?;
            let key =
                rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, bits).map_err(|_| invalid())?;
            let der = key.to_pkcs8_der().map_err(|_| invalid())?;
            Ok(pem("PRIVATE KEY", der.as_bytes()))
        }
    }
}

/// PEM-encode DER under `label`, 64 characters a line.
fn pem(label: &str, der: &[u8]) -> String {
    use std::fmt::Write as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(der);
    let mut out = format!("-----BEGIN {label}-----\n");
    for chunk in encoded.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(chunk).unwrap_or_default());
        out.push('\n');
    }
    let _ = writeln!(out, "-----END {label}-----");
    out
}

// SMTP transport normalization, NOT DKIM canonicalization (owned by mail-auth).
// Only opt-in signed deliveries change LF to CRLF; unsigned baseline is intact.
pub(crate) fn smtp_bytes(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut normalized = Vec::with_capacity(bytes.len());
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\r' && bytes.get(index + 1) != Some(&b'\n') {
            return Err(invalid());
        }
        if *byte == b'\n' && (index == 0 || bytes[index - 1] != b'\r') {
            normalized.push(b'\r');
        }
        normalized.push(*byte);
    }
    if !normalized.ends_with(b"\r\n") {
        normalized.extend_from_slice(b"\r\n");
    }
    Ok(normalized)
}
