//! RFC 8058 one-click unsubscribe tokens.
//!
//! A keyed MAC over the membership and an expiry, carried in the
//! `List-Unsubscribe` HTTPS URI of a personalized delivery and redeemed by
//! an unauthenticated `POST`. The token names the membership, never the
//! address, so a logged URL reveals nothing about the recipient.
use crate::{ListId, MemberId};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use sha2::Sha256;

/// How long a delivered link stays redeemable: mail providers act on
/// one-click links long after delivery.
pub const TTL_SECS: i64 = 90 * 24 * 60 * 60;

const DOMAIN: &[u8] = b"listmngr/one-click-unsubscribe/v1\0";

/// A MAC key for one-click tokens; the key material is never printed.
#[derive(Clone)]
pub struct Signer {
    key: Hmac<Sha256>,
}

impl std::fmt::Debug for Signer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OneClickSigner([REDACTED])")
    }
}

impl Signer {
    /// # Errors
    /// Returns an error for an empty key.
    pub fn new(key: &[u8]) -> crate::Result<Self> {
        if key.len() < 16 {
            return Err(crate::Error::Validation("one-click key too short".into()));
        }
        Ok(Self {
            key: Hmac::<Sha256>::new_from_slice(key)
                .map_err(|_| crate::Error::Validation("invalid one-click key".into()))?,
        })
    }

    fn tag(&self, list: &ListId, member: &str, expires: i64) -> Vec<u8> {
        let mut mac = self.key.clone();
        mac.update(DOMAIN);
        for value in [list.as_str(), member, &expires.to_string()] {
            mac.update(&(value.len() as u64).to_be_bytes());
            mac.update(value.as_bytes());
        }
        mac.finalize().into_bytes().to_vec()
    }

    /// A token for `member` of `list`, valid until `now_secs + TTL_SECS`.
    #[must_use]
    pub fn issue(&self, list: &ListId, member: MemberId, now_secs: i64) -> String {
        let member = member.to_string();
        let expires = now_secs.saturating_add(TTL_SECS);
        format!(
            "{member}.{expires}.{}",
            URL_SAFE_NO_PAD.encode(self.tag(list, &member, expires))
        )
    }

    /// The membership a valid, unexpired token for `list` names.
    #[must_use]
    pub fn verify(&self, list: &ListId, token: &str, now_secs: i64) -> Option<MemberId> {
        if token.len() > 128 {
            return None;
        }
        let mut parts = token.split('.');
        let (member, expires, tag) = (parts.next()?, parts.next()?, parts.next()?);
        if parts.next().is_some() {
            return None;
        }
        let expires: i64 = expires.parse().ok()?;
        if now_secs >= expires {
            return None;
        }
        let member_id: MemberId = member.parse().ok()?;
        let tag = URL_SAFE_NO_PAD.decode(tag).ok()?;
        let mut mac = self.key.clone();
        mac.update(DOMAIN);
        for value in [list.as_str(), member, &expires.to_string()] {
            mac.update(&(value.len() as u64).to_be_bytes());
            mac.update(value.as_bytes());
        }
        mac.verify_slice(&tag).ok()?;
        Some(member_id)
    }
}

/// The HTTPS URI the `List-Unsubscribe` header carries for one recipient.
#[must_use]
pub fn url(base_url: &str, list: &ListId, token: &str) -> String {
    format!(
        "{}/unsubscribe/{}?token={token}",
        base_url.trim_end_matches('/'),
        list
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signer() -> Signer {
        Signer::new(&[7_u8; 32]).unwrap()
    }

    #[test]
    fn tokens_round_trip_and_name_only_the_membership() {
        let list: ListId = "dev.example.invalid".parse().unwrap();
        let member = MemberId::new();
        let token = signer().issue(&list, member, 1_000);
        assert!(!token.contains('@'));
        assert_eq!(signer().verify(&list, &token, 1_000), Some(member));
        assert_eq!(
            signer().verify(&list, &token, 1_000 + TTL_SECS - 1),
            Some(member)
        );
        assert_eq!(
            signer().verify(&list, &token, 1_000 + TTL_SECS),
            None,
            "expired"
        );
        let other: ListId = "other.example.invalid".parse().unwrap();
        assert_eq!(
            signer().verify(&other, &token, 1_000),
            None,
            "bound to the list"
        );
        assert_eq!(
            Signer::new(&[9_u8; 32])
                .unwrap()
                .verify(&list, &token, 1_000),
            None,
            "other key"
        );
        let mut tampered = token.clone();
        tampered.replace_range(0..1, if token.starts_with('0') { "1" } else { "0" });
        assert_eq!(signer().verify(&list, &tampered, 1_000), None);
        assert_eq!(signer().verify(&list, "", 1_000), None);
        assert_eq!(signer().verify(&list, &format!("{token}.x"), 1_000), None);
        assert!(Signer::new(b"short").is_err());
    }

    #[test]
    fn the_url_lives_under_the_site_base() {
        let list: ListId = "dev.example.invalid".parse().unwrap();
        assert_eq!(
            url("https://lists.example.invalid/", &list, "T"),
            "https://lists.example.invalid/unsubscribe/dev.example.invalid?token=T"
        );
    }
}
