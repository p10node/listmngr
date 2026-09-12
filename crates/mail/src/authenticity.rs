//! Mailman's `validate-authenticity`: SPF, DKIM and DMARC checks.
//!
//! An inbound post's checks are summarized as an `Authentication-Results`
//! header (RFC 8601) and as the DMARC policy fact the `dmarc-mitigation`
//! rule needs.
//!
//! Checks run in the `in` runner (they need DNS); the pipeline handler only
//! writes the header they produced. The connecting client is read from the
//! topmost `Received:` header, which the operator's own MTA wrote, because
//! the LMTP intake only ever sees that MTA.
use mail_auth::{
    AuthenticatedMessage, AuthenticationResults, MessageAuthenticator, Parameters, ResolverCache,
    Txt, common::headers::HeaderWriter, dmarc::Policy, spf::verify::SpfParameters,
};
use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::Hash;
use std::net::IpAddr;
use std::sync::Mutex;

/// The connecting client as the front MTA recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    pub ip: IpAddr,
    /// The HELO/EHLO name, when the `Received:` header carried one.
    pub helo: Option<String>,
}

/// The outcome of the checks for one message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Verdict {
    /// `Authentication-Results` header value (authserv-id first), or `None`
    /// when the message could not be parsed.
    pub header: Option<String>,
    /// The RFC 5322 `From` domain publishes a DMARC policy of `reject` or
    /// `quarantine` — Mailman's trigger for mitigation.
    pub dmarc_policy_restrictive: bool,
    /// The From domain the policy applies to.
    pub dmarc_domain: Option<String>,
}

/// A pre-filled TXT record cache. Production runs without one; tests seed
/// it so no query leaves the process.
#[derive(Default)]
pub struct TxtCache(Mutex<HashMap<Box<str>, Txt>>);

impl std::fmt::Debug for TxtCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let entries = self.0.lock().map(|map| map.len()).unwrap_or(0);
        f.debug_struct("TxtCache")
            .field("entries", &entries)
            .finish()
    }
}

impl TxtCache {
    /// Seed `name` (a fully qualified name with its trailing dot) with a
    /// parsed record.
    /// # Panics
    /// Panics if another seeder panicked while holding the cache lock.
    pub fn seed(&self, name: &str, record: Txt) {
        self.0
            .lock()
            .expect("cache lock")
            .insert(name.into(), record);
    }
}

impl ResolverCache<Box<str>, Txt> for TxtCache {
    fn get<Q>(&self, name: &Q) -> Option<Txt>
    where
        Box<str>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.0.lock().expect("cache lock").get(name).cloned()
    }
    fn remove<Q>(&self, name: &Q) -> Option<Txt>
    where
        Box<str>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.0.lock().expect("cache lock").remove(name)
    }
    fn insert(&self, key: Box<str>, value: Txt, _valid_until: std::time::Instant) {
        self.0.lock().expect("cache lock").insert(key, value);
    }
}

/// Runs the checks with the system resolver.
pub struct Verifier {
    authenticator: MessageAuthenticator,
    authserv_id: String,
    cache: Option<TxtCache>,
}

impl std::fmt::Debug for Verifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Verifier")
            .field("authserv_id", &self.authserv_id)
            .field("cache", &self.cache)
            .finish_non_exhaustive()
    }
}

impl Verifier {
    /// A verifier over the system resolver configuration, reporting as
    /// `authserv_id` (the MTA's local hostname).
    /// # Errors
    /// Returns the resolver's error when the system configuration is unusable.
    pub fn system(authserv_id: &str) -> crate::Result<Self> {
        let authenticator = MessageAuthenticator::new_system_conf()
            .map_err(|error| crate::Error::Io(std::io::Error::other(error.to_string())))?;
        Ok(Self {
            authenticator,
            authserv_id: authserv_id.to_owned(),
            cache: None,
        })
    }

    /// Answer every TXT lookup from `cache` first. Names it lacks still reach
    /// DNS, so a seeded verifier must seed every name a check walks.
    #[must_use]
    pub fn with_txt_cache(mut self, cache: TxtCache) -> Self {
        self.cache = Some(cache);
        self
    }

    /// Check `raw`, sent by `envelope_sender` (RFC 5321 `MAIL FROM`, empty
    /// for a null reverse path) from `client`.
    pub async fn verify(
        &self,
        raw: &[u8],
        envelope_sender: Option<&str>,
        client: Option<&Client>,
    ) -> Verdict {
        let Some(message) = AuthenticatedMessage::parse(raw) else {
            return Verdict::default();
        };
        let header_from = message.from().to_owned();
        let cache = self.cache.as_ref();
        let dkim = match cache {
            Some(cache) => {
                self.authenticator
                    .verify_dkim(Parameters::new(&message).with_txt_cache(cache))
                    .await
            }
            None => self.authenticator.verify_dkim(&message).await,
        };
        let mail_from = envelope_sender.unwrap_or("");
        let mail_from_domain = mail_from.rsplit_once('@').map_or("", |(_, d)| d);
        let helo = client
            .and_then(|client| client.helo.as_deref())
            .unwrap_or(&self.authserv_id);
        let spf = match client {
            Some(client) => {
                let params =
                    SpfParameters::verify_mail_from(client.ip, helo, &self.authserv_id, mail_from);
                Some(match cache {
                    Some(cache) => {
                        self.authenticator
                            .verify_spf(Parameters::new(params).with_txt_cache(cache))
                            .await
                    }
                    None => self.authenticator.verify_spf(params).await,
                })
            }
            None => None,
        };
        let spf_for_dmarc = spf.clone().unwrap_or_default();
        let dmarc_params = mail_auth::dmarc::verify::DmarcParameters {
            message: &message,
            dkim_output: &dkim,
            dkim2_output: None,
            rfc5321_mail_from_domain: mail_from_domain,
            spf_output: &spf_for_dmarc,
        };
        let dmarc = match cache {
            Some(cache) => {
                self.authenticator
                    .verify_dmarc(Parameters::new(dmarc_params).with_txt_cache(cache))
                    .await
            }
            None => self.authenticator.verify_dmarc(dmarc_params).await,
        };
        let mut results =
            AuthenticationResults::new(&self.authserv_id).with_dkim_results(&dkim, &header_from);
        if let (Some(spf), Some(client)) = (&spf, client) {
            results = results.with_spf_mailfrom_result(spf, client.ip, mail_from, helo);
        }
        results = results.with_dmarc_result(&dmarc);
        let mut header = Vec::new();
        results.write_header(&mut header);
        let header = String::from_utf8_lossy(&header);
        // `write_header` emits the complete `Authentication-Results: …\r\n`
        // field; keep only the (folded) value for the pipeline handler.
        let value = header
            .strip_prefix("Authentication-Results:")
            .unwrap_or(&header)
            .trim()
            .replace("\r\n\t", " ")
            .replace("\r\n ", " ");
        Verdict {
            header: Some(value),
            dmarc_policy_restrictive: matches!(dmarc.policy(), Policy::Reject | Policy::Quarantine),
            dmarc_domain: (!dmarc.domain().is_empty()).then(|| dmarc.domain().to_owned()),
        }
    }
}

/// The client the topmost `Received:` header names: `from HELO (rdns [IP])`
/// as Postfix and Exim write it. Only that first hop is trusted — the front
/// MTA added it; anything deeper came with the message.
#[must_use]
pub fn received_client(raw: &[u8]) -> Option<Client> {
    let (_, value) = crate::facts::header_fields(raw)
        .into_iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("received"))?;
    let rest = value.strip_prefix("from ")?;
    let helo = rest
        .split_whitespace()
        .next()
        .filter(|helo| !helo.is_empty() && helo.chars().all(|c| c.is_ascii_graphic()))
        .map(str::to_owned);
    let start = rest.find('[')? + 1;
    let end = rest[start..].find(']')? + start;
    let literal = rest[start..end].trim_start_matches("IPv6:");
    let ip: IpAddr = literal.parse().ok()?;
    Some(Client { ip, helo })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_received_hop_names_the_client() {
        let raw = b"Received: from mail.sender.invalid (mail.sender.invalid [192.0.2.25])\r\n\tby mx.example.invalid (Postfix) with ESMTPS id X\r\n\tfor <dev@example.invalid>; Mon, 1 Sep 2026 10:00:00 +0000\r\nReceived: from forged (evil [198.51.100.1])\r\nFrom: a@sender.invalid\r\n\r\nbody";
        assert_eq!(
            received_client(raw),
            Some(Client {
                ip: "192.0.2.25".parse().unwrap(),
                helo: Some("mail.sender.invalid".into())
            })
        );
        let v6 = b"Received: from relay (relay.invalid [IPv6:2001:db8::25]) by mx\r\n\r\nbody";
        assert_eq!(
            received_client(v6).unwrap().ip,
            "2001:db8::25".parse::<IpAddr>().unwrap()
        );
        assert_eq!(received_client(b"From: a@b.invalid\r\n\r\nbody"), None);
        assert_eq!(
            received_client(b"Received: by local (Postfix)\r\n\r\nbody"),
            None
        );
    }
}
