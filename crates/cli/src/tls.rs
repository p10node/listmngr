//! TLS for the web: a second listener, beside the plain one, terminating
//! TLS with rustls and handing each connection to the same router.
//!
//! The plain listener stays for health probes, `listmngr status` and a
//! reverse proxy on the host; this one is for the network. Every
//! connection carries its peer as `ConnectInfo`, as the plain listener's
//! do, so rate limits and audit see the same address either way.
//!
//! The certificate is either the operator's (`cert_file`, `key_file`) or
//! ordered from an ACME directory (`acme_domains`) with TLS-ALPN-01 on
//! this same listener, cached in `acme_cache_dir` and renewed in the
//! background by `rustls-acme`.
use axum::extract::ConnectInfo;
use futures::StreamExt as _;
use hyper::body::Incoming;
use hyper::{Request, service::service_fn};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls_acme::caches::DirCache;
use rustls_acme::{AcmeConfig, AcmeState, is_tls_alpn_challenge};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_rustls::LazyConfigAcceptor;
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::{ClientConfig, RootCertStore, ServerConfig, crypto::ring};
use tower::ServiceExt as _;

/// How long connections in flight get to finish after shutdown.
const DRAIN: Duration = Duration::from_secs(10);

/// What every connection on the listener is offered.
const ALPN: [&[u8]; 2] = [b"h2", b"http/1.1"];

/// The listener's TLS side: the configuration every ordinary connection
/// gets, and the one a TLS-ALPN-01 validation connection gets when
/// certificates come from ACME.
#[derive(Clone)]
pub struct Tls {
    config: Arc<ServerConfig>,
    challenge: Option<Arc<ServerConfig>>,
}

/// The TLS listener, bound, with its configuration and — for ACME — the
/// task that orders and renews the certificate.
pub struct Prepared {
    pub listener: TcpListener,
    pub tls: Tls,
    pub acme: Option<tokio::task::JoinHandle<()>>,
}

fn builder() -> anyhow::Result<
    tokio_rustls::rustls::ConfigBuilder<ServerConfig, tokio_rustls::rustls::WantsVerifier>,
> {
    ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|error| anyhow::anyhow!("web.tls: {error}"))
}

/// The configuration for `cert_file` (the leaf and any chain, PEM) and
/// `key_file` (PKCS#8, PKCS#1 or SEC1, PEM), offering HTTP/2 and HTTP/1.1.
/// # Errors
/// A file that cannot be read or parsed, or a key that does not match.
pub fn own_certificate(cert_file: &Path, key_file: &Path) -> anyhow::Result<Tls> {
    let certs = CertificateDer::pem_file_iter(cert_file)
        .map_err(|error| anyhow::anyhow!("web.tls.cert_file: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| anyhow::anyhow!("web.tls.cert_file: {error}"))?;
    if certs.is_empty() {
        anyhow::bail!("web.tls.cert_file holds no certificate");
    }
    let key = PrivateKeyDer::from_pem_file(key_file)
        .map_err(|error| anyhow::anyhow!("web.tls.key_file: {error}"))?;
    let mut config = builder()?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|error| anyhow::anyhow!("web.tls: {error}"))?;
    config.alpn_protocols = ALPN.iter().map(|p| p.to_vec()).collect();
    Ok(Tls {
        config: Arc::new(config),
        challenge: None,
    })
}

/// The roots the ACME directory is verified against: the Web PKI and,
/// for a private directory, `acme_ca_file`.
fn directory_roots(ca_file: Option<&Path>) -> anyhow::Result<Arc<ClientConfig>> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(ca_file) = ca_file {
        for cert in CertificateDer::pem_file_iter(ca_file)
            .map_err(|error| anyhow::anyhow!("web.tls.acme_ca_file: {error}"))?
        {
            let cert = cert.map_err(|error| anyhow::anyhow!("web.tls.acme_ca_file: {error}"))?;
            roots
                .add(cert)
                .map_err(|error| anyhow::anyhow!("web.tls.acme_ca_file: {error}"))?;
        }
    }
    Ok(Arc::new(
        ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|error| anyhow::anyhow!("web.tls: {error}"))?
            .with_root_certificates(roots)
            .with_no_client_auth(),
    ))
}

/// The configuration for certificates ordered from `acme_directory_url`
/// for `acme_domains`, and the task that keeps them current: it deploys
/// the cached certificate at once when there is one, orders otherwise,
/// and renews at two thirds of the validity, logging every step.
/// # Errors
/// A cache directory that cannot be made owner-only, or a bad CA file.
pub fn acme(
    web: &listmngr_core::WebTlsConfig,
) -> anyhow::Result<(Tls, tokio::task::JoinHandle<()>)> {
    let cache_dir = web
        .acme_cache_dir
        .clone()
        .ok_or_else(|| anyhow::anyhow!("web.tls.acme_cache_dir is not set"))?;
    std::fs::create_dir_all(&cache_dir)
        .map_err(|error| anyhow::anyhow!("web.tls.acme_cache_dir: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&cache_dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| anyhow::anyhow!("web.tls.acme_cache_dir: {error}"))?;
    }
    owner_only(&cache_dir);
    let client = directory_roots(web.acme_ca_file.as_deref())?;
    let mut config = AcmeConfig::new_with_client_config(&web.acme_domains, client)
        .directory(&web.acme_directory_url)
        .cache(DirCache::new(cache_dir.clone()));
    if let Some(contact) = &web.acme_contact {
        config = config.contact_push(format!("mailto:{contact}"));
    }
    let state = AcmeState::new(config);
    let provider = Arc::new(ring::default_provider());
    let challenge = state.challenge_rustls_config_with_provider(provider);
    let mut served = builder()?
        .with_no_client_auth()
        .with_cert_resolver(state.resolver());
    served.alpn_protocols = ALPN.iter().map(|p| p.to_vec()).collect();
    let domains = web.acme_domains.join(",");
    let directory = web.acme_directory_url.clone();
    let task = tokio::spawn(drive(state, domains, directory, cache_dir));
    Ok((
        Tls {
            config: Arc::new(served),
            challenge: Some(challenge),
        },
        task,
    ))
}

/// Polls the ACME state until it ends (it does not, short of a panic),
/// logging what it did and making every file it cached owner-only.
async fn drive(
    mut state: AcmeState<std::io::Error, std::io::Error>,
    domains: String,
    directory: String,
    cache_dir: std::path::PathBuf,
) {
    use rustls_acme::EventOk;
    while let Some(event) = state.next().await {
        let message = match event {
            Ok(EventOk::DeployedCachedCert) => "ACME: cached certificate deployed",
            Ok(EventOk::DeployedNewCert) => "ACME: new certificate deployed",
            Ok(EventOk::CertCacheStore) => {
                owner_only(&cache_dir);
                "ACME: certificate cached"
            }
            Ok(EventOk::AccountCacheStore) => {
                owner_only(&cache_dir);
                "ACME: account cached"
            }
            Err(error) => {
                tracing::error!(%domains, %directory, %error, "ACME: failed");
                continue;
            }
        };
        tracing::info!(%domains, %directory, "{message}");
    }
}

/// Every regular file in the cache — the account key, the certificate
/// with its key — made owner-only. `DirCache` writes them with the
/// process umask (the systemd unit's is `0077` already); this closes
/// the gap elsewhere the moment they land, and at start for files from
/// an earlier run.
#[cfg(unix)]
fn owner_only(dir: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        if let Err(error) =
            std::fs::set_permissions(entry.path(), std::fs::Permissions::from_mode(0o600))
        {
            tracing::warn!(path = %entry.path().display(), %error, "ACME: a cache file stays as it is");
        }
    }
}

#[cfg(not(unix))]
fn owner_only(_dir: &Path) {}

/// What `[web] tls` asks for, bound and read before the HTTP server
/// starts: a taken port or a bad file stops the start.
/// # Errors
/// An unparsable address, a bind failure, or [`own_certificate`]'s and
/// [`acme`]'s errors.
pub async fn prepare(web: &listmngr_core::WebTlsConfig) -> anyhow::Result<Option<Prepared>> {
    if !web.enabled() {
        return Ok(None);
    }
    let address: SocketAddr = web
        .listen
        .as_deref()
        .unwrap_or_default()
        .parse()
        .map_err(|_| listmngr_core::Error::Validation("invalid web.tls.listen".into()))?;
    let (tls, acme) = if web.acme() {
        let (tls, task) = acme(web)?;
        (tls, Some(task))
    } else {
        let tls = own_certificate(
            web.cert_file.as_deref().unwrap_or_else(|| Path::new("")),
            web.key_file.as_deref().unwrap_or_else(|| Path::new("")),
        )?;
        (tls, None)
    };
    let listener = TcpListener::bind(address).await?;
    tracing::info!(%address, acme = acme.is_some(), "HTTPS server listening");
    Ok(Some(Prepared {
        listener,
        tls,
        acme,
    }))
}

/// Accept TLS connections until shutdown, then let the ones in flight
/// finish for [`DRAIN`].
/// # Errors
/// The listener's own errors.
pub async fn serve(
    listener: TcpListener,
    tls: Tls,
    app: axum::Router,
    mut shutdown: watch::Receiver<bool>,
) -> std::io::Result<()> {
    let mut connections = tokio::task::JoinSet::new();
    loop {
        let accepted = tokio::select! {
            accepted = listener.accept() => accepted,
            _ = shutdown.changed() => break,
            _ = connections.join_next(), if !connections.is_empty() => continue,
        };
        let (stream, peer) = accepted?;
        let tls = tls.clone();
        let app = app.clone();
        connections.spawn(async move {
            if let Err(error) = handshake(stream, peer, tls, app).await {
                tracing::debug!(%peer, %error, "TLS handshake failed");
            }
        });
    }
    if tokio::time::timeout(DRAIN, async {
        while connections.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        connections.abort_all();
        while connections.join_next().await.is_some() {}
    }
    Ok(())
}

/// Reads the client hello first: a TLS-ALPN-01 validation gets the
/// challenge certificate and nothing else; everything else gets the
/// site.
async fn handshake(
    stream: tokio::net::TcpStream,
    peer: SocketAddr,
    tls: Tls,
    app: axum::Router,
) -> std::io::Result<()> {
    let start =
        LazyConfigAcceptor::new(tokio_rustls::rustls::server::Acceptor::default(), stream).await?;
    if let Some(challenge) = tls
        .challenge
        .filter(|_| is_tls_alpn_challenge(&start.client_hello()))
    {
        tracing::info!(%peer, "ACME: answering a TLS-ALPN-01 validation");
        let _validated = start.into_stream(challenge).await?;
        return Ok(());
    }
    let stream = start.into_stream(tls.config).await?;
    connection(stream, peer, app).await;
    Ok(())
}

async fn connection<S>(stream: S, peer: SocketAddr, app: axum::Router)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let service = service_fn(move |mut request: Request<Incoming>| {
        request.extensions_mut().insert(ConnectInfo(peer));
        let app = app.clone();
        async move {
            let mut response = app.oneshot(request).await?;
            // Only this listener speaks TLS, so only its answers may ask the
            // browser to insist on it; the plain listener stays plain.
            response.headers_mut().insert(
                hyper::header::STRICT_TRANSPORT_SECURITY,
                hyper::header::HeaderValue::from_static("max-age=31536000; includeSubDomains"),
            );
            Ok::<_, std::convert::Infallible>(response)
        }
    });
    if let Err(error) = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
        .serve_connection_with_upgrades(TokioIo::new(stream), service)
        .await
    {
        tracing::debug!(%peer, %error, "TLS connection ended with an error");
    }
}
