//! The site's master key and what it seals.
//!
//! `[security] master_key` (or `master_key_file`) is 32 random bytes the
//! operator keeps beside the database's other secrets. From it one key per
//! purpose is derived (HKDF-SHA256 with a purpose label), and a secret the
//! database must be able to read back — today the TOTP secrets — is stored
//! as `v1:<base64(nonce || ciphertext)>` under ChaCha20-Poly1305 with the
//! row's owner as associated data, so a row cannot be moved to another
//! account. A site without a master key stores such secrets in the clear,
//! as every release before 1.1 did; `listmngr secrets encrypt` seals the
//! rows a site already has once a key is configured, and `secrets rewrap`
//! moves them to a new key.
use base64::Engine as _;
use listmngr_core::{Error, Result};
use rand::TryRngCore as _;
use ring::{aead, hkdf};
use zeroize::Zeroizing;

/// The prefix of a sealed value in the database.
pub const SEALED_PREFIX: &str = "v1:";
const NONCE_LEN: usize = 12;

/// Whether a stored value is sealed under the master key.
#[must_use]
pub fn is_sealed(value: &str) -> bool {
    value.starts_with(SEALED_PREFIX)
}

/// The master key, zeroed when dropped; never printed.
pub struct MasterKey(Zeroizing<[u8; 32]>);

impl std::fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MasterKey([REDACTED])")
    }
}

impl MasterKey {
    /// The key as the configuration spells it: 64 hexadecimal digits.
    /// # Errors
    /// Validation for anything but 64 hexadecimal digits.
    pub fn from_hex(hex: &str) -> Result<Self> {
        let hex = hex.trim();
        let digits = hex.as_bytes();
        if digits.len() != 64 || !digits.iter().all(u8::is_ascii_hexdigit) {
            return Err(Error::Validation(
                "security.master_key must be 64 hexadecimal digits (32 bytes)".into(),
            ));
        }
        let mut bytes = Zeroizing::new([0_u8; 32]);
        for (index, pair) in digits.chunks_exact(2).enumerate() {
            let text = std::str::from_utf8(pair).map_err(|_| Error::Validation("hex".into()))?;
            bytes[index] =
                u8::from_str_radix(text, 16).map_err(|_| Error::Validation("hex".into()))?;
        }
        Ok(Self(bytes))
    }

    /// A fresh random key as 64 hexadecimal digits, for `listmngr secrets new-key`.
    /// # Errors
    /// The system's random source failing.
    pub fn generate_hex() -> Result<Zeroizing<String>> {
        let mut bytes = Zeroizing::new([0_u8; 32]);
        rand::rngs::OsRng
            .try_fill_bytes(bytes.as_mut())
            .map_err(|_| Error::Database("random source unavailable".into()))?;
        let mut hex = String::with_capacity(64);
        for byte in bytes.iter() {
            use std::fmt::Write as _;
            let _ = write!(hex, "{byte:02x}");
        }
        Ok(Zeroizing::new(hex))
    }

    /// The AEAD key for `purpose`, derived from the master key.
    fn derived(&self, purpose: &[u8]) -> aead::LessSafeKey {
        let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, b"listmngr master key v1");
        let prk = salt.extract(self.0.as_ref());
        let info = [purpose];
        let okm = prk
            .expand(&info, hkdf::HKDF_SHA256)
            .expect("HKDF-SHA256 output length is valid");
        let mut key = Zeroizing::new([0_u8; 32]);
        okm.fill(key.as_mut()).expect("32 bytes of output");
        aead::LessSafeKey::new(
            aead::UnboundKey::new(&aead::CHACHA20_POLY1305, key.as_ref())
                .expect("a 32-byte ChaCha20-Poly1305 key"),
        )
    }

    /// Seal `plain` for `purpose`, bound to `aad`.
    /// # Errors
    /// The random source failing.
    pub fn seal(&self, purpose: &[u8], aad: &[u8], plain: &[u8]) -> Result<String> {
        let mut nonce = [0_u8; NONCE_LEN];
        rand::rngs::OsRng
            .try_fill_bytes(&mut nonce)
            .map_err(|_| Error::Database("random source unavailable".into()))?;
        let mut buffer = plain.to_vec();
        self.derived(purpose)
            .seal_in_place_append_tag(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(aad),
                &mut buffer,
            )
            .map_err(|_| Error::Database("sealing failed".into()))?;
        let mut out = nonce.to_vec();
        out.extend_from_slice(&buffer);
        Ok(format!(
            "{SEALED_PREFIX}{}",
            base64::engine::general_purpose::STANDARD.encode(out)
        ))
    }

    /// Open a value sealed by [`Self::seal`] with the same purpose and
    /// associated data; the plaintext is zeroed when dropped.
    /// # Errors
    /// Validation for a value that is not sealed; `Database` when the key,
    /// the associated data or the bytes do not match what sealed it.
    pub fn open(&self, purpose: &[u8], aad: &[u8], sealed: &str) -> Result<Zeroizing<Vec<u8>>> {
        let body = sealed
            .strip_prefix(SEALED_PREFIX)
            .ok_or_else(|| Error::Validation("not a sealed value".into()))?;
        let mut bytes = base64::engine::general_purpose::STANDARD
            .decode(body)
            .map_err(|_| Error::Database("sealed value is not base64".into()))?;
        if bytes.len() < NONCE_LEN + aead::CHACHA20_POLY1305.tag_len() {
            return Err(Error::Database("sealed value is too short".into()));
        }
        let mut nonce = [0_u8; NONCE_LEN];
        nonce.copy_from_slice(&bytes[..NONCE_LEN]);
        let plain = self
            .derived(purpose)
            .open_in_place(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(aad),
                &mut bytes[NONCE_LEN..],
            )
            .map_err(|_| Error::Database("sealed value does not open under this key".into()))?
            .to_vec();
        Ok(Zeroizing::new(plain))
    }
}

/// The purpose label of sealed TOTP secrets.
pub const TOTP_PURPOSE: &[u8] = b"listmngr/totp/v1";
