use anyhow::Result;
use listmngr_core::Config;
use std::net::SocketAddr;
use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub enum Failure {
    Unreachable,
    Unhealthy,
    NotReady,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("service status check failed")
    }
}
impl std::error::Error for Failure {}

fn probe_address(listen: &str) -> Result<SocketAddr> {
    let mut address: SocketAddr = listen
        .parse()
        .map_err(|_| listmngr_core::Error::Validation("invalid web.listen".into()))?;
    if address.ip().is_unspecified() {
        address.set_ip(if address.is_ipv4() {
            std::net::Ipv4Addr::LOCALHOST.into()
        } else {
            std::net::Ipv6Addr::LOCALHOST.into()
        });
    }
    Ok(address)
}

pub async fn check(config: &Config) -> Result<()> {
    let address = probe_address(&config.web.listen)?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(2))
        .build()?;
    for (path, failure) in [
        ("/healthz", Failure::Unhealthy),
        ("/readyz", Failure::NotReady),
    ] {
        let response = client
            .get(format!("http://{address}{path}"))
            .send()
            .await
            .map_err(|_| Failure::Unreachable)?;
        if !response.status().is_success() {
            return Err(failure.into());
        }
    }
    println!("service: healthy and ready");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::probe_address;

    #[test]
    fn probe_maps_only_unspecified_addresses_to_family_loopback() {
        for (listen, expected) in [
            ("0.0.0.0:8000", "127.0.0.1:8000"),
            ("[::]:8001", "[::1]:8001"),
            ("127.0.0.2:8002", "127.0.0.2:8002"),
            ("[::1]:8003", "[::1]:8003"),
        ] {
            assert_eq!(probe_address(listen).unwrap().to_string(), expected);
        }
    }
}
