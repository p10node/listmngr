//! Opt-in outbound DKIM. No incoming authentication or DNS policy evaluation.
use listmngr_core::{DkimSigningConfig, Error, Result};
use mail_auth::common::headers::HeaderWriter;
use mail_auth::{
    common::crypto::{RsaKey, Sha256},
    dkim::{Canonicalization, DkimSigner, Done},
};
use rustls_pki_types::{PrivateKeyDer, pem::PemObject};
use std::{collections::BTreeMap, io::Read, sync::Arc};

type Signer = DkimSigner<RsaKey<Sha256>, Done>;

#[derive(Clone, Default)]
pub struct SigningKeys(Arc<BTreeMap<String, Signer>>);

impl std::fmt::Debug for SigningKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SigningKeys")
            .field("keys", &"[REDACTED]")
            .finish()
    }
}

fn invalid() -> Error {
    Error::Validation("invalid outbound DKIM signing configuration or message".into())
}

fn dns_name(value: &str) -> bool {
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
fn read_key_file(path: &std::path::Path) -> Result<Vec<u8>> {
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

impl SigningKeys {
    /// Load operator key files once at runtime startup, never during config display.
    /// # Errors
    /// Returns a redacted error for an unreadable or unsupported RSA key.
    pub fn load(config: &[DkimSigningConfig]) -> Result<Self> {
        let mut keys = BTreeMap::new();
        let mut domains = std::collections::BTreeSet::new();
        for entry in config {
            if !dns_name(&entry.domain)
                || !entry.domain.contains('.')
                || !dns_name(&entry.selector)
                || entry.domain.len() + entry.selector.len() + "._domainkey.".len() > 253
                || !domains.insert(&entry.domain)
            {
                return Err(invalid());
            }
        }
        for entry in config {
            let bytes = read_key_file(&entry.private_key_file)?;
            let der = PrivateKeyDer::from_pem_slice(&bytes).map_err(|_| invalid())?;
            let key = RsaKey::<Sha256>::from_key_der(der).map_err(|_| invalid())?;
            keys.insert(
                entry.domain.clone(),
                DkimSigner::from_key(key)
                    .domain(&entry.domain)
                    .selector(&entry.selector)
                    // A repeated From oversigns its absence: prepending another From
                    // must invalidate the signature, not merely leave the old one valid.
                    .headers([
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
                    ])
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
    /// key: `(<selector>._domainkey.<domain>, "v=DKIM1; k=rsa; p=…")`. Reads
    /// the private key file with the same checks as [`Self::load`] and never
    /// returns private material.
    /// # Errors
    /// Returns a redacted error for an unreadable or unsupported RSA key.
    pub fn dns_record(entry: &DkimSigningConfig) -> Result<(String, String)> {
        use base64::Engine as _;
        use rsa::pkcs8::{DecodePrivateKey as _, EncodePublicKey as _};
        if !dns_name(&entry.domain) || !dns_name(&entry.selector) {
            return Err(invalid());
        }
        let bytes = read_key_file(&entry.private_key_file)?;
        let der = PrivateKeyDer::from_pem_slice(&bytes).map_err(|_| invalid())?;
        let private = match der {
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
            .map_err(|_| invalid())?;
        let encoded = base64::engine::general_purpose::STANDARD.encode(public.as_bytes());
        Ok((
            format!("{}._domainkey.{}", entry.selector, entry.domain),
            format!("v=DKIM1; k=rsa; p={encoded}"),
        ))
    }

    /// Sign final cooked bytes using an authoritative stored list mail host.
    /// # Errors
    /// Returns a redacted local error, never an SMTP/mailbox failure.
    pub fn sign(&self, domain: &str, bytes: Vec<u8>) -> Result<Vec<u8>> {
        let Some(signer) = self.0.get(domain) else {
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
        let signature = signer.sign(&bytes).map_err(|_| invalid())?;
        let mut output = signature.to_header().into_bytes();
        output.extend_from_slice(&bytes);
        Ok(output)
    }
}

// SMTP transport normalization, NOT DKIM canonicalization (owned by mail-auth).
// Only opt-in signed deliveries change LF to CRLF; unsigned baseline is intact.
fn smtp_bytes(bytes: &[u8]) -> Result<Vec<u8>> {
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
