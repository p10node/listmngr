//! Mailman's `arc-sign`: seal a delivered post (RFC 8617) so a receiver can
//! trust the authentication results this site recorded before the list
//! changed the message.
//!
//! The chain is read at intake (`authenticity::Verifier`, with DNS) and its
//! verdict travels with the post; sealing happens on the final bytes of
//! each delivery, after the list's own DKIM signature, without DNS: the
//! existing sets are read back from the message and the recorded verdict
//! decides the new seal's `cv=`.
use crate::dkim::{dns_name, invalid, read_key_file, smtp_bytes};
use listmngr_core::{ArcConfig, Result};
use mail_auth::{
    ArcOutput, AuthenticatedMessage, AuthenticationResults, DkimResult,
    arc::{ArcError, ArcSealer, Set},
    common::{
        crypto::{RsaKey, Sha256},
        headers::{Header, HeaderWriter},
    },
    dkim::{Canonicalization, Done},
};
use rustls_pki_types::{PrivateKeyDer, pem::PemObject};

/// What the intake found of the chain a post arrived with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chain {
    /// No ARC sets.
    None,
    /// The sets validate.
    Pass,
    /// The sets do not validate (or could not be checked).
    Fail,
}

impl Chain {
    /// The word the message context stores.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Pass => "pass",
            Self::Fail => "fail",
        }
    }

    /// The reverse of [`Self::as_str`].
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "none" => Some(Self::None),
            "pass" => Some(Self::Pass),
            "fail" => Some(Self::Fail),
            _ => None,
        }
    }

    /// The chain verdict `verify_arc` gave.
    #[must_use]
    pub fn of(output: &ArcOutput<'_>) -> Self {
        match output.result() {
            DkimResult::Pass => Self::Pass,
            DkimResult::None if output.sets().is_empty() => Self::None,
            _ => Self::Fail,
        }
    }
}

/// The headers a seal signs: Mailman's `[ARC] sig_headers` default, the
/// list's own `DKIM-Signature`, and `From` once more so a second `From`
/// cannot be added without breaking the signature.
const SIGNED_HEADERS: [&str; 30] = [
    "From",
    "From",
    "Sender",
    "Reply-To",
    "Subject",
    "Date",
    "Message-ID",
    "To",
    "Cc",
    "MIME-Version",
    "Content-Type",
    "Content-Transfer-Encoding",
    "Content-ID",
    "Content-Description",
    "Resent-Date",
    "Resent-From",
    "Resent-Sender",
    "Resent-To",
    "Resent-Cc",
    "Resent-Message-ID",
    "In-Reply-To",
    "References",
    "List-Id",
    "List-Help",
    "List-Unsubscribe",
    "List-Subscribe",
    "List-Post",
    "List-Owner",
    "List-Archive",
    "DKIM-Signature",
];

/// The site's ARC sealer: one key, one domain, one selector.
pub struct Sealer(ArcSealer<RsaKey<Sha256>, Done>);

impl std::fmt::Debug for Sealer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sealer")
            .field("key", &"[REDACTED]")
            .finish()
    }
}

impl Sealer {
    /// Load the sealing key when `[mta.arc]` is enabled; `None` when it is
    /// not. Read at runtime startup, never during configuration display.
    /// # Errors
    /// Returns a redacted error for an invalid domain or selector or an
    /// unreadable or unsupported RSA key.
    pub fn load(config: &ArcConfig) -> Result<Option<Self>> {
        if !config.enabled {
            return Ok(None);
        }
        let Some(path) = &config.private_key_file else {
            return Err(invalid());
        };
        if !dns_name(&config.domain)
            || !config.domain.contains('.')
            || !dns_name(&config.selector)
            || config.domain.len() + config.selector.len() + "._domainkey.".len() > 253
        {
            return Err(invalid());
        }
        let bytes = read_key_file(path)?;
        let der = PrivateKeyDer::from_pem_slice(&bytes).map_err(|_| invalid())?;
        let key = RsaKey::<Sha256>::from_key_der(der).map_err(|_| invalid())?;
        Ok(Some(Self(
            ArcSealer::from_key(key)
                .domain(&config.domain)
                .selector(&config.selector)
                .headers(SIGNED_HEADERS)
                .header_canonicalization(Canonicalization::Relaxed)
                // As the DKIM signer: simple body hashing keeps the bytes as
                // they are and interoperates.
                .body_canonicalization(Canonicalization::Simple),
        )))
    }

    /// Seal `bytes` (a delivery, already signed) with a new ARC set whose
    /// `ARC-Authentication-Results` is `results` — the value of the
    /// `Authentication-Results` this site wrote at intake — and whose
    /// `cv=` follows `chain`, the intake's verdict on the sets the post
    /// arrived with. A chain that ended in `cv=fail` cannot be extended, and
    /// an unparsable message cannot be sealed: both come back as they were,
    /// because a delivery must not fail over its seal.
    /// # Errors
    /// Returns a redacted error when the bytes are not a mail message.
    pub fn seal(&self, bytes: &[u8], results: &str, chain: Chain) -> Result<Vec<u8>> {
        let bytes = smtp_bytes(bytes)?;
        let Some(message) = AuthenticatedMessage::parse(&bytes) else {
            return Ok(bytes);
        };
        let mut output = ArcOutput::default().with_result(match chain {
            Chain::Pass => DkimResult::Pass,
            Chain::None => DkimResult::None,
            Chain::Fail => DkimResult::Fail(mail_auth::Error::Arc(ArcError::BrokenChain)),
        });
        // The sets the post arrived with, as `verify_arc` groups them.
        if message.as_headers.len() == message.ams_headers.len()
            && message.as_headers.len() == message.aar_headers.len()
        {
            for ((seal, signature), results) in message
                .as_headers
                .iter()
                .zip(&message.ams_headers)
                .zip(&message.aar_headers)
            {
                output = output.with_set(Set {
                    signature: Header::new(signature.name, signature.value, &signature.header),
                    seal: Header::new(seal.name, seal.value, &seal.header),
                    results: Header::new(results.name, results.value, &results.header),
                });
            }
        } else if !message.as_headers.is_empty() {
            // A chain with sets missing: broken, whatever the intake saw.
            output = output.with_result(DkimResult::Fail(mail_auth::Error::Arc(
                ArcError::BrokenChain,
            )));
        }
        if !output.can_be_sealed() {
            return Ok(bytes);
        }
        // The results value already begins with the authserv-id, so it is
        // written whole as the "hostname" of an otherwise empty result.
        let results = AuthenticationResults::new(results);
        let Ok(set) = self.0.seal(&message, &results, &output) else {
            return Ok(bytes);
        };
        let mut sealed = set.to_header().into_bytes();
        sealed.extend_from_slice(&bytes);
        Ok(sealed)
    }
}
