use anyhow::Result;
use listmngr_core::Config;
use serde_json::{Value, json};
use std::time::Duration;

mod dns;

#[derive(Debug, clap::Args)]
pub struct Options {
    /// Query this DNS server (IP:port) instead of the system resolver.
    #[arg(long)]
    dns_server: Option<std::net::SocketAddr>,
}

#[derive(Debug)]
pub struct Failure;
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("required doctor check failed")
    }
}
impl std::error::Error for Failure {}

fn check(id: &str, status: &str, detail: &str) -> Value {
    json!({"id": id, "status": status, "detail": detail})
}

pub async fn run(config: &Config, options: Options) -> Result<()> {
    let database = tokio::time::timeout(
        Duration::from_secs(3),
        listmngr_db::doctor::inspect(&config.database.url),
    )
    .await
    .unwrap_or(Err("database_timeout"));
    let mut checks = vec![match &database {
        Ok(_) => check("database", "ok", "migration_ledger_current"),
        Err(reason) => check("database", "fail", reason),
    }];
    checks.push(if config.mta.enabled {
        match tokio::time::timeout(
            Duration::from_secs(3),
            smtp_greeting(&config.mta.smtp_relay),
        )
        .await
        {
            Ok(Ok(())) => check("mta", "ok", "smtp_greeting_only"),
            Ok(Err(())) => check("mta", "fail", "smtp_unavailable_or_invalid_greeting"),
            Err(_) => check("mta", "fail", "smtp_timeout"),
        }
    } else {
        check("mta", "skip", "mail_role_disabled")
    });
    checks.push(check(
        "mta_delivery",
        "skip",
        "tls_auth_and_delivery_not_probed",
    ));
    match &database {
        Ok(domains) => checks.extend(dns::checks(domains, options.dns_server).await),
        Err(_) => checks.push(check("dns", "skip", "database_unavailable")),
    }
    checks.push(check(
        "dns_authentication",
        "skip",
        "spf_dkim_dmarc_arc_and_ptr_not_probed",
    ));
    let ok = !checks.iter().any(|check| check["status"] == "fail");
    println!("{}", json!({"version": 1, "ok": ok, "checks": checks}));
    if !ok {
        return Err(Failure.into());
    }
    Ok(())
}

async fn smtp_greeting(address: &str) -> Result<(), ()> {
    use tokio::io::AsyncReadExt;
    let mut stream = tokio::net::TcpStream::connect(address)
        .await
        .map_err(|_| ())?;
    // Read byte-bounded RFC 5321 greeting lines, with one outer deadline.
    // No EHLO, AUTH, MAIL, RCPT, DATA or even QUIT is sent.
    for _ in 0..32 {
        let mut line = Vec::new();
        while line.len() < 512 {
            line.push(stream.read_u8().await.map_err(|_| ())?);
            if line.ends_with(b"\r\n") {
                break;
            }
        }
        if !line.ends_with(b"\r\n") || !line.starts_with(b"220") {
            return Err(());
        }
        match line.get(3) {
            Some(b' ') => return Ok(()),
            Some(b'-') => {}
            _ => return Err(()),
        }
    }
    Err(())
}
