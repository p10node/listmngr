//! Bounded routing DNS diagnostics, not a deliverability or DNSSEC audit.
use hickory_resolver::{
    TokioResolver,
    config::{ConnectionConfig, NameServerConfig, ResolveHosts, ResolverConfig},
    net::{DnsError, NetError, runtime::TokioRuntimeProvider},
    proto::{
        op::ResponseCode,
        rr::{Name, RData},
    },
};
use serde_json::Value;
use std::{net::SocketAddr, time::Duration};

pub fn resolver(server: Option<SocketAddr>) -> Result<TokioResolver, ()> {
    let mut builder = if let Some(server) = server {
        let mut udp = ConnectionConfig::udp();
        udp.port = server.port();
        let mut tcp = ConnectionConfig::tcp();
        tcp.port = server.port();
        TokioResolver::builder_with_config(
            ResolverConfig::from_name_servers(vec![NameServerConfig::new(
                server.ip(),
                true,
                vec![udp, tcp],
            )]),
            TokioRuntimeProvider::default(),
        )
    } else {
        TokioResolver::builder_tokio().map_err(|_| ())?
    };
    let options = builder.options_mut();
    options.timeout = Duration::from_secs(2);
    options.attempts = 1;
    options.use_hosts_file = ResolveHosts::Never;
    builder.build().map_err(|_| ())
}

pub async fn checks(domains: &[String], server: Option<SocketAddr>) -> Vec<Value> {
    if domains.is_empty() {
        return vec![super::check("dns", "skip", "no_mail_domains")];
    }
    let Ok(resolver) = resolver(server) else {
        return vec![super::check("dns", "fail", "resolver_unavailable")];
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut checks = Vec::new();
    for domain in domains {
        let outcome = tokio::time::timeout_at(deadline, route(&resolver, domain)).await;
        let (status, detail) = match outcome {
            Ok(Ok(detail)) => ("ok", detail),
            Ok(Err(detail)) => ("fail", detail),
            Err(_) => ("fail", "dns_budget_exhausted"),
        };
        let mut check = super::check("dns", status, detail);
        check["domain"] = domain.clone().into();
        checks.push(check);
        if outcome.is_err() {
            // Explicit failure means an incomplete inventory never reports healthy.
            break;
        }
    }
    checks
}

async fn route(resolver: &TokioResolver, domain: &str) -> Result<&'static str, &'static str> {
    // Always absolute: never append resolver search domains to mail identities.
    let name = Name::from_ascii(format!("{}.", domain.trim_end_matches('.')))
        .map_err(|_| "invalid_mail_domain")?;
    match resolver.mx_lookup(name.clone()).await {
        Ok(records) => {
            let exchangers: Vec<_> = records
                .answers()
                .iter()
                .filter_map(|record| match &record.data {
                    RData::MX(mx) => Some(mx),
                    _ => None,
                })
                .collect();
            if exchangers.iter().any(|mx| mx.exchange.is_root()) {
                return Err("null_mx");
            }
            // A site with even one broken advertised exchanger needs attention.
            let mut count = 0;
            for mx in exchangers.iter().take(33) {
                count += 1;
                if count > 32 {
                    return Err("mx_limit_exceeded");
                }
                addresses(resolver, mx.exchange.clone()).await?;
            }
            if count == 0 {
                return Err("mx_unavailable");
            }
            Ok("mx_addresses_resolve")
        }
        Err(NetError::Dns(DnsError::NoRecordsFound(records)))
            if records.response_code == ResponseCode::NoError =>
        {
            addresses(resolver, name).await?;
            Ok("implicit_mx_addresses_resolve")
        }
        Err(_) => Err("mx_lookup_failed"),
    }
}

async fn addresses(resolver: &TokioResolver, name: Name) -> Result<(), &'static str> {
    let result = resolver
        .lookup_ip(name)
        .await
        .map_err(|_| "mail_host_addresses_unavailable")?;
    if result.iter().next().is_some() {
        Ok(())
    } else {
        Err("mail_host_addresses_unavailable")
    }
}
