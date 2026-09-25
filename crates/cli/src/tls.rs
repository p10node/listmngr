//! TLS for the web: a second listener, beside the plain one, terminating
//! TLS with rustls and handing each connection to the same router.
//!
//! The plain listener stays for health probes, `listmngr status` and a
//! reverse proxy on the host; this one is for the network. Every
//! connection carries its peer as `ConnectInfo`, as the plain listener's
//! do, so rate limits and audit see the same address either way.
use axum::extract::ConnectInfo;
use hyper::body::Incoming;
use hyper::{Request, service::service_fn};
use hyper_util::rt::{TokioExecutor, TokioIo};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::{ServerConfig, crypto::ring};
use tower::ServiceExt as _;

/// How long connections in flight get to finish after shutdown.
const DRAIN: Duration = Duration::from_secs(10);

/// The acceptor for `cert_file` (the leaf and any chain, PEM) and
/// `key_file` (PKCS#8, PKCS#1 or SEC1, PEM), offering HTTP/2 and HTTP/1.1.
/// # Errors
/// A file that cannot be read or parsed, or a key that does not match.
pub fn acceptor(cert_file: &Path, key_file: &Path) -> anyhow::Result<TlsAcceptor> {
    let certs = CertificateDer::pem_file_iter(cert_file)
        .map_err(|error| anyhow::anyhow!("web.tls.cert_file: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| anyhow::anyhow!("web.tls.cert_file: {error}"))?;
    if certs.is_empty() {
        anyhow::bail!("web.tls.cert_file holds no certificate");
    }
    let key = PrivateKeyDer::from_pem_file(key_file)
        .map_err(|error| anyhow::anyhow!("web.tls.key_file: {error}"))?;
    let mut config = ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|error| anyhow::anyhow!("web.tls: {error}"))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|error| anyhow::anyhow!("web.tls: {error}"))?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// What `[web] tls` asks for, bound and read before the HTTP server
/// starts: a taken port or a bad file stops the start.
/// # Errors
/// An unparsable address, a bind failure, or [`acceptor`]'s errors.
pub async fn prepare(
    web: &listmngr_core::WebTlsConfig,
) -> anyhow::Result<Option<(TcpListener, TlsAcceptor)>> {
    if !web.enabled() {
        return Ok(None);
    }
    let address: SocketAddr = web
        .listen
        .as_deref()
        .unwrap_or_default()
        .parse()
        .map_err(|_| listmngr_core::Error::Validation("invalid web.tls.listen".into()))?;
    let acceptor = acceptor(
        web.cert_file.as_deref().unwrap_or_else(|| Path::new("")),
        web.key_file.as_deref().unwrap_or_else(|| Path::new("")),
    )?;
    let listener = TcpListener::bind(address).await?;
    tracing::info!(%address, "HTTPS server listening");
    Ok(Some((listener, acceptor)))
}

/// Accept TLS connections until shutdown, then let the ones in flight
/// finish for [`DRAIN`].
/// # Errors
/// The listener's own errors.
pub async fn serve(
    listener: TcpListener,
    acceptor: TlsAcceptor,
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
        let acceptor = acceptor.clone();
        let app = app.clone();
        connections.spawn(async move {
            match acceptor.accept(stream).await {
                Ok(tls) => connection(tls, peer, app).await,
                Err(error) => tracing::debug!(%peer, %error, "TLS handshake failed"),
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

async fn connection<S>(stream: S, peer: SocketAddr, app: axum::Router)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let service = service_fn(move |mut request: Request<Incoming>| {
        request.extensions_mut().insert(ConnectInfo(peer));
        app.clone().oneshot(request)
    });
    if let Err(error) = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
        .serve_connection_with_upgrades(TokioIo::new(stream), service)
        .await
    {
        tracing::debug!(%peer, %error, "TLS connection ended with an error");
    }
}
