//! The webhook runner: posts what the store owes each webhook.
//!
//! A delivery is claimed under a lease, posted once as JSON with an HMAC
//! signature the receiver can check, and recorded: delivered on a 2xx,
//! retried with backoff on anything else, given up after
//! `[webhooks] max_attempts` or at once for a target the site may never
//! reach. The target is resolved first and the connection pinned to the
//! addresses found, so a name that changes between the check and the
//! connection cannot steer a delivery somewhere private.
use crate::delivery_policy::Backoff;
use hmac::{Hmac, Mac};
use listmngr_core::WebhooksConfig;
use listmngr_db::Database;
use listmngr_db::webhooks::{Delivery, Outcome, Webhook};
use sha2::Sha256;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;
use tokio::sync::watch;

/// How long one attempt may hold a delivery before another runner may
/// take it over.
const LEASE_MS: i64 = 60_000;
/// Ten seconds, doubling, an hour at most, jittered: Mailman's outgoing
/// retry shape.
const BACKOFF: Backoff = Backoff {
    initial_ms: 10_000,
    max_ms: 3_600_000,
};

/// Post due deliveries until shutdown.
///
/// The caller owns and supervises this future; it runs wherever
/// `listmngr serve` runs, with or without the mail role, since events
/// come from the API and the web as well.
pub async fn run(db: Database, config: WebhooksConfig, mut shutdown: watch::Receiver<bool>) {
    while !*shutdown.borrow() {
        match deliver_due(&db, &config).await {
            Ok(Some(_)) => {}
            Ok(None) => pause(&mut shutdown).await,
            Err(error) => {
                tracing::error!(%error, "webhook delivery failed");
                pause(&mut shutdown).await;
            }
        }
    }
}

async fn pause(shutdown: &mut watch::Receiver<bool>) {
    tokio::select! { () = tokio::time::sleep(Duration::from_millis(500)) => {}, _ = shutdown.changed() => {} }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Claim the next due delivery, post it and record the outcome. `None`
/// when nothing is due.
/// # Errors
/// The store's errors: a claim or a record that could not be written,
/// or a webhook whose secret cannot be derived.
pub async fn deliver_due(
    db: &Database,
    config: &WebhooksConfig,
) -> Result<Option<(Delivery, Outcome)>, listmngr_core::Error> {
    let now = now_ms();
    let Some((delivery, webhook)) = db.webhooks().claim_due(now, LEASE_MS).await? else {
        return Ok(None);
    };
    let secret = db.webhooks().secret(webhook.id).await?;
    let outcome = match post(config, &delivery, &webhook, &secret).await {
        Attempt::Accepted(status) => Outcome::Delivered { status },
        Attempt::Refused {
            status,
            error,
            permanent,
        } => {
            if permanent || delivery.attempts >= i64::from(config.max_attempts) {
                Outcome::Failed { status, error }
            } else {
                Outcome::Retry {
                    status,
                    error,
                    next_attempt_at: now.saturating_add(BACKOFF.delay_ms(delivery.attempts)),
                }
            }
        }
    };
    let label = match &outcome {
        Outcome::Delivered { .. } => "delivered",
        Outcome::Retry { .. } => "retried",
        Outcome::Failed { .. } => "failed",
    };
    listmngr_core::metrics::global()
        .webhook_deliveries
        .inc(label);
    tracing::info!(
        webhook = %webhook.id,
        delivery = %delivery.id,
        event = %delivery.event,
        attempt = delivery.attempts,
        outcome = label,
        "webhook delivery"
    );
    let delivery = db
        .webhooks()
        .record(&delivery.id, &outcome, now_ms())
        .await?;
    Ok(Some((delivery, outcome)))
}

/// `sha256=` HMAC of `timestamp.body` under the webhook's secret: what a
/// receiver recomputes to know the site sent the delivery, and when.
///
/// # Panics
/// Never: HMAC accepts a key of any length.
#[must_use]
pub fn signature(secret: &str, timestamp: i64, body: &str) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC takes a key of any length");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body.as_bytes());
    let digest = mac.finalize().into_bytes();
    let mut out = String::with_capacity(7 + digest.len() * 2);
    out.push_str("sha256=");
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// An address the site must not post to unless told it may: loopback,
/// link-local, private, shared, unspecified, multicast, or an IPv4 of
/// those mapped into IPv6.
#[must_use]
pub fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_private_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_private_v4(v4);
            }
            let first = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80
        }
    }
}

fn is_private_v4(v4: Ipv4Addr) -> bool {
    let [a, b, _, _] = v4.octets();
    v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || v4.is_unspecified()
        || v4.is_broadcast()
        || v4.is_multicast()
        || v4.is_documentation()
        || a == 0
        || (a == 100 && (64..=127).contains(&b))
        || (a == 192 && b == 0)
        || (a == 198 && (b == 18 || b == 19))
}

enum Attempt {
    Accepted(i64),
    Refused {
        status: Option<i64>,
        error: String,
        permanent: bool,
    },
}

fn permanent(error: &str) -> Attempt {
    Attempt::Refused {
        status: None,
        error: error.to_owned(),
        permanent: true,
    }
}

const fn transient(error: String) -> Attempt {
    Attempt::Refused {
        status: None,
        error,
        permanent: false,
    }
}

/// Where the target resolves to, checked against the site's policy.
async fn addresses(
    config: &WebhooksConfig,
    url: &reqwest::Url,
) -> Result<Vec<SocketAddr>, Attempt> {
    let port = url.port_or_known_default().unwrap_or(443);
    let found: Vec<SocketAddr> = match url.host() {
        Some(url::Host::Ipv4(ip)) => vec![SocketAddr::new(IpAddr::V4(ip), port)],
        Some(url::Host::Ipv6(ip)) => vec![SocketAddr::new(IpAddr::V6(ip), port)],
        Some(url::Host::Domain(name)) => tokio::net::lookup_host((name, port))
            .await
            .map_err(|error| transient(format!("resolve {name}: {error}")))?
            .collect(),
        None => return Err(permanent("url has no host")),
    };
    if found.is_empty() {
        return Err(transient("resolve: no address".into()));
    }
    if !config.allow_private_targets && found.iter().any(|address| is_private(address.ip())) {
        return Err(permanent(
            "target resolves to a private, loopback or link-local address",
        ));
    }
    Ok(found)
}

async fn post(
    config: &WebhooksConfig,
    delivery: &Delivery,
    webhook: &Webhook,
    secret: &str,
) -> Attempt {
    let Ok(url) = reqwest::Url::parse(&webhook.url) else {
        return permanent("url cannot be parsed");
    };
    match url.scheme() {
        "https" => {}
        "http" if config.allow_http => {}
        scheme => return permanent(&format!("scheme {scheme} is not allowed")),
    }
    let found = match addresses(config, &url).await {
        Ok(found) => found,
        Err(refused) => return refused,
    };
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(u64::from(config.timeout_secs)))
        .user_agent("listmngr");
    if let Some(url::Host::Domain(name)) = url.host() {
        // Connect to what was checked, not to what a second lookup says.
        builder = builder.resolve_to_addrs(name, &found);
    }
    let client = match builder.build() {
        Ok(client) => client,
        Err(error) => return transient(format!("client: {error}")),
    };
    let body = delivery.payload.to_string();
    let timestamp = chrono::Utc::now().timestamp();
    let response = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header("X-Listmngr-Event", &delivery.event)
        .header("X-Listmngr-Delivery", &delivery.id)
        .header("X-Listmngr-Webhook", webhook.id.to_string())
        .header("X-Listmngr-Timestamp", timestamp.to_string())
        .header("X-Listmngr-Signature", signature(secret, timestamp, &body))
        .body(body)
        .send()
        .await;
    match response {
        Ok(response) => {
            let status = i64::from(response.status().as_u16());
            if response.status().is_success() {
                Attempt::Accepted(status)
            } else {
                Attempt::Refused {
                    status: Some(status),
                    error: format!("HTTP {status}"),
                    permanent: false,
                }
            }
        }
        // reqwest's text names the URL, never a header or the body.
        Err(error) => transient(format!("request: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_addresses_are_the_ones_a_site_must_not_post_to() {
        for private in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.1.1",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fe80::1",
            "fd00::1",
            "::ffff:10.0.0.1",
        ] {
            assert!(is_private(private.parse().unwrap()), "{private}");
        }
        for public in [
            "93.184.216.34",
            "2606:2800:220:1:248:1893:25c8:1946",
            "8.8.8.8",
        ] {
            assert!(!is_private(public.parse().unwrap()), "{public}");
        }
    }

    #[test]
    fn the_signature_covers_the_timestamp_and_the_body() {
        let a = signature("secret", 1_700_000_000, "{}");
        assert!(a.starts_with("sha256=") && a.len() == 7 + 64);
        assert_eq!(a, signature("secret", 1_700_000_000, "{}"));
        assert_ne!(a, signature("secret", 1_700_000_001, "{}"));
        assert_ne!(a, signature("secret", 1_700_000_000, "{ }"));
        assert_ne!(a, signature("other", 1_700_000_000, "{}"));
    }
}
