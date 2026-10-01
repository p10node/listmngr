//! `listmngr serve` with `[web] tls`: the same site over TLS on a second
//! address, with HTTP/2 and HTTP/1.1 on offer, while the plain listener
//! keeps answering; a key that cannot be read stops the start.
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName};
use tokio_rustls::rustls::{ClientConfig, RootCertStore, crypto::ring};

/// A CA and a `localhost` certificate it signed, made with `openssl` like
/// the STARTTLS tests' — never checked in.
struct Identity {
    ca: PathBuf,
    cert: PathBuf,
    key: PathBuf,
}

fn identity(dir: &Path) -> Identity {
    std::fs::write(
        dir.join("extensions"),
        "subjectAltName=DNS:localhost\nbasicConstraints=critical,CA:FALSE\nextendedKeyUsage=serverAuth\n",
    )
    .unwrap();
    for args in [
        vec![
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            "ca.key",
            "-out",
            "ca.pem",
            "-days",
            "2",
            "-subj",
            "/CN=Owned web TLS CA",
            "-addext",
            "basicConstraints=critical,CA:TRUE",
        ],
        vec![
            "req",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            "server.key",
            "-out",
            "server.csr",
            "-subj",
            "/CN=localhost",
        ],
        vec![
            "x509",
            "-req",
            "-in",
            "server.csr",
            "-CA",
            "ca.pem",
            "-CAkey",
            "ca.key",
            "-CAcreateserial",
            "-out",
            "server.pem",
            "-days",
            "2",
            "-extfile",
            "extensions",
        ],
    ] {
        let output = std::process::Command::new("openssl")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(output.status.success(), "certificate generation failed");
    }
    let key = dir.join("server.key");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    Identity {
        ca: dir.join("ca.pem"),
        cert: dir.join("server.pem"),
        key,
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn serve(dir: &Path, plain: u16, tls: u16, identity: &Identity) -> std::process::Child {
    let url = format!("sqlite://{}/web.db?mode=rwc", dir.display());
    std::process::Command::new(assert_cmd::cargo::cargo_bin("listmngr"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("RUST_LOG", "info")
        .env("LISTMNGR__DATABASE__URL", &url)
        .env("LISTMNGR__WEB__LISTEN", format!("127.0.0.1:{plain}"))
        .env("LISTMNGR__WEB__TLS__LISTEN", format!("127.0.0.1:{tls}"))
        .env("LISTMNGR__WEB__TLS__CERT_FILE", &identity.cert)
        .env("LISTMNGR__WEB__TLS__KEY_FILE", &identity.key)
        .env(
            "LISTMNGR__SITE__BASE_URL",
            format!("https://localhost:{tls}"),
        )
        .current_dir(dir)
        .arg("serve")
        .stdout(std::process::Stdio::null())
        .stderr(std::fs::File::create(dir.join("serve.log")).unwrap())
        .spawn()
        .unwrap()
}

/// The plain listener's `/healthz`, once it answers.
fn wait_for_plain(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", port)) {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let _ = stream.write_all(
                b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            );
            let mut answer = String::new();
            let _ = stream.read_to_string(&mut answer);
            if answer.starts_with("HTTP/1.1 200") {
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "the plain listener never answered"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn client(ca: &Path, alpn: &[u8]) -> TlsConnector {
    let mut roots = RootCertStore::empty();
    for cert in CertificateDer::pem_file_iter(ca).unwrap() {
        roots.add(cert.unwrap()).unwrap();
    }
    let mut config = ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![alpn.to_vec()];
    TlsConnector::from(Arc::new(config))
}

#[tokio::test]
async fn the_site_is_served_over_tls_on_its_own_address() {
    let dir = tempfile::tempdir().unwrap();
    let identity = identity(dir.path());
    let (plain, tls) = (free_port(), free_port());
    let mut child = serve(dir.path(), plain, tls, &identity);
    tokio::task::spawn_blocking(move || wait_for_plain(plain))
        .await
        .unwrap();
    // HTTP/1.1 over TLS, the certificate checked against the CA.
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", tls))
        .await
        .unwrap();
    let mut stream = client(&identity.ca, b"http/1.1")
        .connect(ServerName::try_from("localhost").unwrap(), tcp)
        .await
        .unwrap();
    assert_eq!(stream.get_ref().1.alpn_protocol(), Some(&b"http/1.1"[..]));
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut answer = Vec::new();
    let _ = stream.read_to_end(&mut answer).await;
    let answer = String::from_utf8_lossy(&answer);
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    assert!(
        answer
            .to_ascii_lowercase()
            .contains("strict-transport-security: max-age=31536000; includesubdomains"),
        "HSTS over TLS: {answer}"
    );
    // HTTP/2 is on offer.
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", tls))
        .await
        .unwrap();
    let stream = client(&identity.ca, b"h2")
        .connect(ServerName::try_from("localhost").unwrap(), tcp)
        .await
        .unwrap();
    assert_eq!(stream.get_ref().1.alpn_protocol(), Some(&b"h2"[..]));
    drop(stream);
    // Plain HTTP on the TLS address is not HTTP.
    let mut tcp = tokio::net::TcpStream::connect(("127.0.0.1", tls))
        .await
        .unwrap();
    tcp.write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    let mut answer = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), tcp.read_to_end(&mut answer)).await;
    assert!(
        !String::from_utf8_lossy(&answer).starts_with("HTTP/"),
        "{}",
        String::from_utf8_lossy(&answer)
    );
    // The plain listener still answers, and never asks for TLS: it is the
    // probe and proxy listener.
    let plain_answer = tokio::task::spawn_blocking(move || {
        wait_for_plain(plain);
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", plain)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut answer = String::new();
        let _ = stream.read_to_string(&mut answer);
        answer
    })
    .await
    .unwrap();
    assert!(plain_answer.starts_with("HTTP/1.1 200"), "{plain_answer}");
    assert!(
        !plain_answer
            .to_ascii_lowercase()
            .contains("strict-transport-security"),
        "no HSTS on the plain listener: {plain_answer}"
    );
    child.kill().unwrap();
    child.wait().unwrap();
    let log = std::fs::read_to_string(dir.path().join("serve.log")).unwrap_or_default();
    assert!(log.contains("HTTPS server listening"), "{log}");
}

#[test]
fn a_key_that_cannot_be_read_stops_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let identity = identity(dir.path());
    std::fs::write(
        &identity.key,
        "-----BEGIN PRIVATE KEY-----\nnot a key\n-----END PRIVATE KEY-----\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&identity.key, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let mut child = serve(dir.path(), free_port(), free_port(), &identity);
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "serve kept running with a bad key"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(!status.success());
    let log = std::fs::read_to_string(dir.path().join("serve.log")).unwrap_or_default();
    assert!(!log.contains("HTTPS server listening"), "{log}");
}
