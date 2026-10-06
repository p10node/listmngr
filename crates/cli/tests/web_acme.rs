//! `listmngr serve` with `[web] tls.acme_domains`: the certificate is
//! ordered from an ACME directory — Pebble, Let's Encrypt's test server,
//! on loopback, resolving the domain through its own `pebble-challtestsrv`
//! DNS — through TLS-ALPN-01 on the TLS listener itself, cached in
//! `acme_cache_dir`, and the cached one deployed at the next start.
//!
//! Needs `TEST_PEBBLE_BIN` and `TEST_PEBBLE_CHALLTESTSRV_BIN`, the two
//! binaries of one Pebble release
//! (<https://github.com/letsencrypt/pebble/releases>); binds loopback
//! ports of its own choosing.
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName};
use tokio_rustls::rustls::{ClientConfig, RootCertStore, crypto::ring};

/// The name the certificate is ordered for; the challenge DNS answers
/// `127.0.0.1` for every name and nothing for `AAAA`.
const DOMAIN: &str = "lists.example.test";

/// Pebble's own TLS identity for its directory: a CA and a `localhost`
/// certificate it signed, made with `openssl` — never checked in. The
/// CA is what `acme_ca_file` points at.
struct Identity {
    ca: PathBuf,
    cert: PathBuf,
    key: PathBuf,
}

fn identity(dir: &Path) -> Identity {
    std::fs::write(
        dir.join("extensions"),
        "subjectAltName=DNS:localhost,IP:127.0.0.1\nbasicConstraints=critical,CA:FALSE\nextendedKeyUsage=serverAuth\n",
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
            "/CN=Owned ACME test CA",
            "-addext",
            "basicConstraints=critical,CA:TRUE",
        ],
        vec![
            "req",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            "pebble.key",
            "-out",
            "pebble.csr",
            "-subj",
            "/CN=localhost",
        ],
        vec![
            "x509",
            "-req",
            "-in",
            "pebble.csr",
            "-CA",
            "ca.pem",
            "-CAkey",
            "ca.key",
            "-CAcreateserial",
            "-out",
            "pebble.pem",
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
    Identity {
        ca: dir.join("ca.pem"),
        cert: dir.join("pebble.pem"),
        key: dir.join("pebble.key"),
    }
}

/// A child process killed when the test ends, panicking or not.
struct Guard(std::process::Child);

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// The binary `variable` names; a Pebble release archive carries it as
/// `0644`, so a start refused as `PermissionDenied` wants a `chmod +x`.
fn binary(variable: &str) -> std::process::Command {
    let path =
        std::env::var_os(variable).unwrap_or_else(|| panic!("{variable} names a Pebble binary"));
    assert!(
        Path::new(&path).is_file(),
        "{variable} = {} is not a file",
        path.to_string_lossy()
    );
    std::process::Command::new(path)
}

/// `command` started, or the panic names what could not start and why.
fn start(command: &mut std::process::Command, what: &str) -> Guard {
    Guard(command.spawn().unwrap_or_else(|error| {
        panic!(
            "{what} ({}) did not start: {error}",
            command.get_program().display()
        )
    }))
}

fn wait_for_port(port: u16, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "{what} never listened");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The loopback ports: Pebble's directory and management interface, the
/// port it validates TLS-ALPN-01 on (the TLS listener's), the challenge
/// DNS server Pebble resolves the domain through, and its management.
struct Ports {
    directory: u16,
    management: u16,
    tls: u16,
    dns: u16,
    dns_management: u16,
}

/// `pebble-challtestsrv` as a DNS server only: `A` is `127.0.0.1` for
/// every name, `AAAA` nothing, every challenge responder off.
fn challtestsrv(dir: &Path, ports: &Ports) -> Guard {
    let child = start(
        binary("TEST_PEBBLE_CHALLTESTSRV_BIN")
            .args([
                "-dnsserver",
                &format!("127.0.0.1:{}", ports.dns),
                "-defaultIPv4",
                "127.0.0.1",
                "-defaultIPv6",
                "",
                "-http01",
                "",
                "-https01",
                "",
                "-tlsalpn01",
                "",
                "-doh",
                "",
                "-management",
                &format!("127.0.0.1:{}", ports.dns_management),
            ])
            .current_dir(dir)
            .stdout(std::fs::File::create(dir.join("challtestsrv.log")).unwrap())
            .stderr(std::fs::File::create(dir.join("challtestsrv.err")).unwrap()),
        "pebble-challtestsrv",
    );
    wait_for_port(ports.dns_management, "pebble-challtestsrv");
    child
}

/// Pebble on loopback, validating at once (`PEBBLE_VA_NOSLEEP`) and
/// rejecting no nonce on purpose (`PEBBLE_WFE_NONCEREJECT`, five percent
/// by default, which the client would retry but the log would show).
fn pebble(dir: &Path, identity: &Identity, ports: &Ports) -> Guard {
    let config = dir.join("pebble.json");
    std::fs::write(
        &config,
        format!(
            r#"{{"pebble": {{
  "listenAddress": "127.0.0.1:{}",
  "managementListenAddress": "127.0.0.1:{}",
  "certificate": {:?},
  "privateKey": {:?},
  "httpPort": {},
  "tlsPort": {},
  "ocspResponderURL": "",
  "externalAccountBindingRequired": false,
  "retryAfter": {{"authz": 1, "order": 1}},
  "keyAlgorithm": "ecdsa",
  "profiles": {{"default": {{"description": "test", "validityPeriod": 7776000}}}}
}}}}"#,
            ports.directory,
            ports.management,
            identity.cert.display().to_string(),
            identity.key.display().to_string(),
            free_port(),
            ports.tls
        ),
    )
    .unwrap();
    let child = start(
        binary("TEST_PEBBLE_BIN")
            .env("PEBBLE_VA_NOSLEEP", "1")
            .env("PEBBLE_WFE_NONCEREJECT", "0")
            .arg("-config")
            .arg(&config)
            .arg("-dnsserver")
            .arg(format!("127.0.0.1:{}", ports.dns))
            .current_dir(dir)
            .stdout(std::fs::File::create(dir.join("pebble.log")).unwrap())
            .stderr(std::fs::File::create(dir.join("pebble.err")).unwrap()),
        "Pebble",
    );
    wait_for_port(ports.directory, "Pebble");
    child
}

fn serve(dir: &Path, plain: u16, identity: &Identity, ports: &Ports) -> Guard {
    let config = dir.join("listmngr.toml");
    std::fs::write(
        &config,
        format!(
            "[database]\nurl = \"sqlite://{}/web.db?mode=rwc\"\n[site]\nbase_url = \"https://{DOMAIN}:{}\"\n[web]\nlisten = \"127.0.0.1:{plain}\"\n[web.tls]\nlisten = \"127.0.0.1:{}\"\nacme_domains = [{DOMAIN:?}]\nacme_contact = \"postmaster@example.invalid\"\nacme_directory_url = \"https://127.0.0.1:{}/dir\"\nacme_cache_dir = {:?}\nacme_ca_file = {:?}\n",
            dir.display(),
            ports.tls,
            ports.tls,
            ports.directory,
            dir.join("acme").display().to_string(),
            identity.ca.display().to_string(),
        ),
    )
    .unwrap();
    std::process::Command::new(assert_cmd::cargo::cargo_bin("listmngr"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("RUST_LOG", "info")
        .env("LISTMNGR_CONFIG", &config)
        .current_dir(dir)
        .arg("serve")
        .stdout(std::process::Stdio::null())
        .stderr(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("serve.log"))
                .unwrap(),
        )
        .spawn()
        .map(Guard)
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

fn client(roots: RootCertStore) -> TlsConnector {
    let mut config = ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    TlsConnector::from(Arc::new(config))
}

fn roots_from(pem: &[u8]) -> RootCertStore {
    let mut roots = RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(pem) {
        roots.add(cert.unwrap()).unwrap();
    }
    assert!(
        !roots.is_empty(),
        "no certificate in {}",
        String::from_utf8_lossy(pem)
    );
    roots
}

/// One `GET` over TLS to `127.0.0.1:port` as `name`: the whole answer and
/// the leaf certificate the server presented.
async fn get(
    port: u16,
    connector: &TlsConnector,
    name: &'static str,
    path: &str,
) -> std::io::Result<(String, Vec<u8>)> {
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
    let mut stream = connector
        .connect(ServerName::try_from(name).unwrap(), tcp)
        .await?;
    let leaf = stream
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|chain| chain.first())
        .map(|cert| cert.to_vec())
        .unwrap_or_default();
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: {name}\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await?;
    let mut answer = Vec::new();
    let _ = stream.read_to_end(&mut answer).await;
    Ok((String::from_utf8_lossy(&answer).into_owned(), leaf))
}

/// `/healthz` over TLS as the domain, with a certificate Pebble's root
/// signed, within the deadline: the leaf served.
async fn wait_for_issued(tls: u16, connector: &TlsConnector) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        if let Ok((answer, leaf)) = get(tls, connector, DOMAIN, "/healthz").await
            && answer.starts_with("HTTP/1.1 200")
        {
            return leaf;
        }
        assert!(
            Instant::now() < deadline,
            "no certificate from Pebble within the deadline"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[tokio::test]
#[ignore = "requires TEST_PEBBLE_BIN and TEST_PEBBLE_CHALLTESTSRV_BIN (a Pebble release); binds loopback ports"]
async fn the_certificate_is_ordered_from_pebble_and_the_cached_one_reused() {
    let dir = tempfile::tempdir().unwrap();
    let identity = identity(dir.path());
    let ports = Ports {
        directory: free_port(),
        management: free_port(),
        tls: free_port(),
        dns: free_port(),
        dns_management: free_port(),
    };
    let plain = free_port();
    let dns = challtestsrv(dir.path(), &ports);
    let pebble = pebble(dir.path(), &identity, &ports);
    // Pebble's root, from its management interface (served with the
    // identity above).
    let own_ca = std::fs::read(&identity.ca).unwrap();
    let management = client(roots_from(&own_ca));
    let (roots, _) = get(ports.management, &management, "localhost", "/roots/0")
        .await
        .unwrap();
    let body = roots
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_owned())
        .unwrap();
    let issued = client(roots_from(body.as_bytes()));

    let child = serve(dir.path(), plain, &identity, &ports);
    tokio::task::spawn_blocking(move || wait_for_plain(plain))
        .await
        .unwrap();
    let first = wait_for_issued(ports.tls, &issued).await;
    assert!(!first.is_empty());
    drop(child);
    let log = std::fs::read_to_string(dir.path().join("serve.log")).unwrap_or_default();
    assert!(log.contains("HTTPS server listening"), "{log}");
    assert!(log.contains("answering a TLS-ALPN-01 validation"), "{log}");
    assert!(log.contains("ACME: new certificate deployed"), "{log}");
    assert!(log.contains("ACME: certificate cached"), "{log}");
    assert!(!log.contains("ACME: failed"), "{log}");
    // The cache is owner-only, files included, and holds the certificate
    // and the account.
    let cache = dir.path().join("acme");
    let names: Vec<PathBuf> = std::fs::read_dir(&cache)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    let has_prefix = |prefix: &str| {
        names.iter().any(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(prefix)
        })
    };
    assert!(has_prefix("cached_cert_"), "{names:?}");
    assert!(has_prefix("cached_account_"), "{names:?}");
    #[cfg(unix)]
    {
        assert_eq!(mode(&cache), 0o700);
        for path in &names {
            assert_eq!(mode(path), 0o600, "{}", path.display());
        }
    }

    // A second start deploys the cached certificate — the same leaf —
    // without an order.
    std::fs::write(dir.path().join("serve.log"), "").unwrap();
    let child = serve(dir.path(), plain, &identity, &ports);
    tokio::task::spawn_blocking(move || wait_for_plain(plain))
        .await
        .unwrap();
    let second = wait_for_issued(ports.tls, &issued).await;
    drop(child);
    drop(pebble);
    drop(dns);
    assert_eq!(first, second, "the cached certificate is the one served");
    let log = std::fs::read_to_string(dir.path().join("serve.log")).unwrap_or_default();
    assert!(log.contains("ACME: cached certificate deployed"), "{log}");
    assert!(!log.contains("ACME: new certificate deployed"), "{log}");
    assert!(!log.contains("TLS-ALPN-01"), "{log}");
    assert!(!log.contains("ACME: failed"), "{log}");
}
