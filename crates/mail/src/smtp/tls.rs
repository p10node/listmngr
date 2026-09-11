//! Verified REQUIRED STARTTLS transport; SMTP transaction semantics stay in the parent module.
use super::auth::AuthPlain;
use super::{
    AsyncRead, AsyncWrite, ErrorKind, IoError, IoResult, SendOutcome, SmtpClientConfig, command,
    is_safe_smtp_text, read_response, run_transaction, send_with_envid, validate_envid,
};
use rustls::pki_types::{CertificateDer, ServerName, pem::PemObject};
use std::sync::Arc;
use tokio_rustls::{TlsConnector, rustls};

/// Validated transport policy. TLS configuration cannot install a custom verifier.
#[derive(Debug, Clone)]
pub struct TransportSecurity(Policy);

#[derive(Debug, Clone)]
enum Policy {
    Disabled,
    Plaintext,
    Required {
        config: Arc<rustls::ClientConfig>,
        name: ServerName<'static>,
        auth: Option<AuthPlain>,
    },
}

/// Validated authentication mode; contains no credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthenticationPolicy {
    Disabled,
    Plain,
}

impl TransportSecurity {
    #[must_use]
    pub const fn authentication(&self) -> AuthenticationPolicy {
        match &self.0 {
            Policy::Required { auth: Some(_), .. } => AuthenticationPolicy::Plain,
            _ => AuthenticationPolicy::Disabled,
        }
    }

    /// Load public roots plus optional PEM CA certificates, and validate relay identity.
    /// # Errors
    /// Rejects unsupported modes, invalid identity or unreadable/invalid/empty CA files.
    pub fn from_mta(mta: &listmngr_core::MtaConfig) -> Result<Self, listmngr_core::Error> {
        let invalid = || {
            listmngr_core::Error::Validation("invalid SMTP TLS policy, identity or CA file".into())
        };
        let auth = AuthPlain::from_mta(mta)?;
        match mta.smtp_tls.as_str() {
            "plaintext_trusted_relay" => Ok(Self(Policy::Plaintext)),
            "opportunistic" if !mta.enabled => Ok(Self(Policy::Disabled)),
            "required" => {
                let relay: std::net::SocketAddr = mta.smtp_relay.parse().map_err(|_| invalid())?;
                let name = ServerName::try_from(
                    mta.smtp_tls_server_name
                        .clone()
                        .unwrap_or_else(|| relay.ip().to_string()),
                )
                .map_err(|_| invalid())?;
                let mut roots = rustls::RootCertStore::empty();
                roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
                if let Some(path) = &mta.smtp_tls_ca_file {
                    let bytes = std::fs::read(path).map_err(|_| invalid())?;
                    let certs = CertificateDer::pem_slice_iter(&bytes)
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|_| invalid())?;
                    if certs.is_empty() {
                        return Err(invalid());
                    }
                    for cert in certs {
                        roots.add(cert).map_err(|_| invalid())?;
                    }
                }
                let config = rustls::ClientConfig::builder_with_provider(Arc::new(
                    rustls::crypto::ring::default_provider(),
                ))
                .with_safe_default_protocol_versions()
                .map_err(|_| invalid())?
                .with_root_certificates(roots)
                .with_no_client_auth();
                Ok(Self(Policy::Required {
                    config: Arc::new(config),
                    name,
                    auth,
                }))
            }
            _ => Err(invalid()),
        }
    }
}

/// Send with a validated transport policy. REQUIRED never falls back to plaintext.
/// # Errors
/// TLS/policy/pre-upgrade SMTP failures are local transport errors, never mailbox failures.
pub async fn send_secure<S>(
    stream: S,
    config: &SmtpClientConfig,
    security: &TransportSecurity,
    mail_from: Option<&str>,
    recipients: &[String],
    data: &[u8],
) -> IoResult<SendOutcome>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    send_secure_with_envid(stream, config, security, mail_from, recipients, data, None).await
}

/// Send a singleton with a bounded ENVID only after current-session DSN negotiation.
/// # Errors
/// Invalid envelope, TLS or authentication failures. No plaintext fallback.
pub async fn send_secure_with_envid<S: AsyncRead + AsyncWrite + Unpin + Send>(
    stream: S,
    config: &SmtpClientConfig,
    security: &TransportSecurity,
    mail_from: Option<&str>,
    recipients: &[String],
    data: &[u8],
    envid: Option<&str>,
) -> IoResult<SendOutcome> {
    validate_envid(envid, mail_from, recipients)?;
    let Policy::Required {
        config: tls_config,
        name,
        auth,
    } = &security.0
    else {
        return match security.0 {
            Policy::Plaintext => {
                send_with_envid(stream, config, mail_from, recipients, data, envid).await
            }
            _ => Err(IoError::new(
                ErrorKind::InvalidInput,
                "SMTP transport disabled",
            )),
        };
    };
    if !is_safe_smtp_text(&config.local_hostname)
        || mail_from.is_some_and(|s| !is_safe_smtp_text(s))
    {
        return Err(IoError::new(
            ErrorKind::InvalidInput,
            "unsafe SMTP identity",
        ));
    }
    // One budget for greeting, initial EHLO, STARTTLS and TLS, including writes.
    let upgraded = tokio::time::timeout(config.command_timeout, async {
        let (read, mut writer) = tokio::io::split(stream);
        let mut reader = tokio::io::BufReader::new(read);
        let greeting = read_response(&mut reader, config.command_timeout).await?;
        if greeting.code != 220 {
            return Err(IoError::other("SMTP greeting rejected before TLS"));
        }
        let ehlo = command(
            &mut writer,
            &mut reader,
            config.command_timeout,
            &format!("EHLO {}", config.local_hostname),
        )
        .await?;
        if ehlo.code != 250
            || !ehlo.extensions.iter().any(|line| {
                line.split_ascii_whitespace()
                    .next()
                    .is_some_and(|token| token.eq_ignore_ascii_case("STARTTLS"))
            })
        {
            return Err(IoError::other("SMTP required STARTTLS unavailable"));
        }
        let reply = command(&mut writer, &mut reader, config.command_timeout, "STARTTLS").await?;
        if reply.code != 220 {
            return Err(IoError::other("SMTP STARTTLS rejected"));
        }
        // Do not discard unsolicited plaintext buffered across the security boundary.
        if !reader.buffer().is_empty() {
            return Err(IoError::other("unexpected bytes before TLS"));
        }
        let stream = reader.into_inner().unsplit(writer);
        TlsConnector::from(Arc::clone(tls_config))
            .connect(name.clone(), stream)
            .await
    })
    .await
    .map_err(|_| IoError::new(ErrorKind::TimedOut, "SMTP STARTTLS deadline exceeded"))??;
    let (read, mut writer) = tokio::io::split(upgraded);
    let mut reader = tokio::io::BufReader::new(read);
    // AUTH negotiates a fresh EHLO only after certificate verification.
    let supports_dsn = if let Some(auth) = auth {
        auth.authenticate(&mut writer, &mut reader, config).await?
    } else {
        false
    };
    // The shared envelope/DATA path is unchanged.
    let mut outcome = run_transaction(
        &mut writer,
        &mut reader,
        config.command_timeout,
        if auth.is_some() { None } else { Some(config) },
        mail_from,
        recipients,
        data,
        envid.map(|v| (v, supports_dsn)),
    )
    .await;
    if auth.is_some() {
        // A hostile authenticated relay can echo credentials in any later reply.
        // Keep typed stage/code and final-DATA ambiguity, never its free-form text.
        for status in &mut outcome.results {
            use super::RecipientStatus;
            match status {
                RecipientStatus::Sent => {}
                RecipientStatus::TransientFailure(text)
                | RecipientStatus::PermanentFailure(text)
                | RecipientStatus::RemotePermanentFailure { detail: text, .. } => {
                    *text = "SMTP reply redacted (authenticated session)".into();
                }
                RecipientStatus::Ambiguous(text) => {
                    *text = "remote outcome unknown after DATA (reply redacted)".into();
                }
            }
        }
    }
    Ok(outcome)
}
