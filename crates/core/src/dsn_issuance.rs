//! Outbound correlation only: possession does not authenticate a reporting MTA.
use crate::{Error, MtaConfig, Result};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::io::Read;

#[derive(Clone)]
pub struct Issuer {
    key: Hmac<Sha256>,
    key_id: String,
    ttl_ms: i64,
}
impl std::fmt::Debug for Issuer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DsnIssuer([REDACTED])")
    }
}
fn invalid() -> Error {
    Error::Validation("invalid DSN issuance configuration or authority".into())
}
impl Issuer {
    /// Read one exact 32-byte binary key; private regular files only on Unix.
    /// # Errors
    /// Rejects missing singleton isolation, invalid identifiers/TTL or unsafe keys.
    pub fn load(mta: &MtaConfig) -> Result<Option<Self>> {
        if !mta.dsn_issuance_enabled {
            return Ok(None);
        }
        if !mta.smtp_single_recipient
            || mta.dsn_key_id.is_empty()
            || mta.dsn_key_id.len() > 16
            || !mta
                .dsn_key_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || !(60..=2_592_000).contains(&mta.dsn_ttl_secs)
        {
            return Err(invalid());
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
        }
        let file = options
            .open(mta.dsn_key_file.as_ref().ok_or_else(invalid)?)
            .map_err(|_| invalid())?;
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
        file.take(33)
            .read_to_end(&mut bytes)
            .map_err(|_| invalid())?;
        if bytes.len() != 32 {
            return Err(invalid());
        }
        let key = Hmac::<Sha256>::new_from_slice(&bytes).map_err(|_| invalid())?;
        bytes.fill(0);
        Ok(Some(Self {
            key,
            key_id: mta.dsn_key_id.clone(),
            ttl_ms: i64::from(mta.dsn_ttl_secs) * 1000,
        }))
    }
    #[must_use]
    pub fn key_id(&self) -> &str {
        &self.key_id
    }
    /// # Errors
    /// Rejects timestamp overflow.
    pub fn expires_at(&self, issued_at: i64) -> Result<i64> {
        issued_at.checked_add(self.ttl_ms).ok_or_else(invalid)
    }
    /// Versioned, length-delimited domain separation over canonical stored claims.
    #[must_use]
    pub fn issue(&self, claims: &str, nonce: &str) -> String {
        let mut mac = self.key.clone();
        mac.update(b"listmngr/outbound-dsn/envid/v1\0");
        for value in [self.key_id.as_str(), nonce, claims] {
            mac.update(&(value.len() as u64).to_be_bytes());
            mac.update(value.as_bytes());
        }
        format!(
            "1.{}.{}.{}",
            self.key_id,
            nonce,
            URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
        )
    }
    /// Constant-time MAC verification against immutable local claims, not returned MIME.
    #[must_use]
    pub fn verify(&self, envid: &str, claims: &str, now: i64, issued: i64, expires: i64) -> bool {
        if now < issued || now >= expires || expires <= issued || envid.len() > 100 {
            return false;
        }
        let parts: Vec<_> = envid.split('.').collect();
        if parts.len() != 4
            || parts[0] != "1"
            || parts[1] != self.key_id
            || parts[2].len() != 32
            || !parts[2].bytes().all(|b| b.is_ascii_hexdigit())
        {
            return false;
        }
        let Ok(tag) = URL_SAFE_NO_PAD.decode(parts[3]) else {
            return false;
        };
        let mut mac = self.key.clone();
        mac.update(b"listmngr/outbound-dsn/envid/v1\0");
        for value in [self.key_id.as_str(), parts[2], claims] {
            mac.update(&(value.len() as u64).to_be_bytes());
            mac.update(value.as_bytes());
        }
        mac.verify_slice(&tag).is_ok()
    }
}
