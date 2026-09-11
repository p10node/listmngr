use super::*;
use tokio::io::AsyncReadExt;

#[test]
fn required_starttls_policy_validation_controls() {
    let identity = Identity::new();
    let mut config = listmngr_core::Config::default();
    config.mta.enabled = true;
    for mode in ["opportunistic", "none", "REQUIRED", "", "unknown"] {
        config.mta.smtp_tls = mode.into();
        assert!(MailRoleConfig::from_core(&config).is_err(), "{mode}");
    }
    config.mta.smtp_tls = "required".into();
    // Public roots and the relay IP as identity require no private CA file.
    assert!(MailRoleConfig::from_core(&config).is_ok());
    for name in [
        "",
        "bad name",
        "relay.invalid:25",
        "https://relay.invalid",
        "relay\r\nMAIL FROM:<x>",
    ] {
        config.mta.smtp_tls_server_name = Some(name.into());
        assert!(MailRoleConfig::from_core(&config).is_err());
    }
    config.mta.smtp_tls_server_name = Some("localhost".into());
    let ca = identity.dir.path().join("bad-ca.pem");
    config.mta.smtp_tls_ca_file = Some(ca.to_str().unwrap().into());
    assert!(MailRoleConfig::from_core(&config).is_err());
    for bytes in [
        b"".as_slice(),
        b"not a certificate",
        b"-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n",
    ] {
        std::fs::write(&ca, bytes).unwrap();
        let error = MailRoleConfig::from_core(&config).unwrap_err().to_string();
        assert!(
            !error.contains(ca.to_str().unwrap()),
            "local paths are not disclosed"
        );
    }
}

#[derive(Debug, Clone, Copy)]
enum Fault {
    Missing,
    Substring,
    FirstLineOnly,
    Rejected,
    WrongName,
    Untrusted,
    HandshakeTimeout,
    HandshakeClose,
    GreetingTimeout,
}

#[tokio::test]
async fn required_starttls_negative_controls_never_mail_or_bounce() {
    let identity = Identity::new();
    for fault in [
        Fault::Missing,
        Fault::Substring,
        Fault::FirstLineOnly,
        Fault::Rejected,
        Fault::WrongName,
        Fault::Untrusted,
        Fault::HandshakeTimeout,
        Fault::HandshakeClose,
        Fault::GreetingTimeout,
    ] {
        let (db, lease, _, sink) = tests::fixture_at(
            "sqlite::memory:",
            "{\"list_id\":\"test.example.invalid\"}",
            b"Subject: secret\r\n\r\nsecret body\r\n",
            vec!["member@example.invalid".into()],
        )
        .await;
        let mut config = listmngr_core::Config::default();
        config.mta.enabled = true;
        config.mta.smtp_tls = "required".into();
        config.mta.smtp_relay = sink.local_addr().unwrap().to_string();
        config.mta.smtp_tls_server_name = Some(
            if matches!(fault, Fault::WrongName) {
                "wrong.invalid"
            } else {
                "localhost"
            }
            .into(),
        );
        if !matches!(fault, Fault::Untrusted) {
            config.mta.smtp_tls_ca_file =
                Some(identity.dir.path().join("ca.pem").to_str().unwrap().into());
        }
        let mut role = MailRoleConfig::from_core(&config).unwrap();
        role.command_timeout = Duration::from_millis(200);
        let peer = negative_peer(&sink, &identity, fault);
        tokio::time::timeout(Duration::from_secs(4), async {
            tokio::join!(peer, deliver_one(&db, &role, lease.clone()));
        })
        .await
        .unwrap();
        let status: String =
            sqlx::query_scalar("SELECT status FROM delivery_recipients WHERE job_id=$1")
                .bind(lease.job.id.0.to_string())
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(status, "pending", "{fault:?}");
        let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM bounce_events")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(events, 0, "{fault:?}");
    }
}

async fn negative_peer(sink: &tokio::net::TcpListener, identity: &Identity, fault: Fault) {
    let (stream, _) = sink.accept().await.unwrap();
    let mut plain = BufReader::new(stream);
    if matches!(fault, Fault::GreetingTimeout) {
        let mut bytes = Vec::new();
        plain.read_to_end(&mut bytes).await.unwrap();
        assert!(bytes.is_empty());
        return;
    }
    plain.get_mut().write_all(b"220 owned\r\n").await.unwrap();
    let mut line = String::new();
    plain.read_line(&mut line).await.unwrap();
    assert_eq!(line, "EHLO listmngr.invalid\r\n");
    let extension: &[u8] = match fault {
        Fault::Missing => b"250-owned\r\n250 SIZE 10000\r\n",
        Fault::Substring => b"250-owned\r\n250 XSTARTTLS\r\n",
        Fault::FirstLineOnly => b"250-STARTTLS\r\n250 SIZE 10000\r\n",
        _ => b"250-owned\r\n250 STARTTLS\r\n",
    };
    plain.get_mut().write_all(extension).await.unwrap();
    if matches!(
        fault,
        Fault::Missing | Fault::Substring | Fault::FirstLineOnly
    ) {
        let mut bytes = Vec::new();
        plain.read_to_end(&mut bytes).await.unwrap();
        assert!(bytes.is_empty(), "{fault:?}: unexpected plaintext bytes");
        return;
    }
    line.clear();
    plain.read_line(&mut line).await.unwrap();
    assert_eq!(line, "STARTTLS\r\n");
    assert!(plain.buffer().is_empty());
    if matches!(fault, Fault::Rejected) {
        plain
            .get_mut()
            .write_all(b"550 TLS refused\r\n")
            .await
            .unwrap();
        let mut bytes = Vec::new();
        plain.read_to_end(&mut bytes).await.unwrap();
        assert!(bytes.is_empty());
        return;
    }
    plain.get_mut().write_all(b"220 upgrade\r\n").await.unwrap();
    failed_handshake(plain, identity, fault).await;
}

async fn failed_handshake(
    mut plain: BufReader<tokio::net::TcpStream>,
    identity: &Identity,
    fault: Fault,
) {
    if matches!(fault, Fault::HandshakeClose) {
        return;
    }
    if matches!(fault, Fault::HandshakeTimeout) {
        let mut bytes = Vec::new();
        plain.read_to_end(&mut bytes).await.unwrap();
        assert!(!bytes.is_empty(), "must actually begin TLS");
        for secret in [b"MAIL FROM".as_slice(), b"RCPT TO", b"DATA", b"secret body"] {
            assert!(!bytes.windows(secret.len()).any(|part| part == secret));
        }
        return;
    }
    let result = identity.acceptor.accept(plain.into_inner()).await;
    assert!(
        result.is_err(),
        "{fault:?}: unverified TLS must be rejected"
    );
}

use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_rustls::{TlsAcceptor, rustls};

pub(super) struct Identity {
    pub(super) dir: tempfile::TempDir,
    pub(super) acceptor: TlsAcceptor,
}

impl Identity {
    pub(super) fn new() -> Self {
        let dir =
            tempfile::tempdir_in(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target")).unwrap();
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
                "/CN=Owned STARTTLS CA",
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
            std::fs::write(dir.path().join("extensions"), "subjectAltName=DNS:localhost\nbasicConstraints=critical,CA:FALSE\nextendedKeyUsage=serverAuth\n").unwrap();
            let output = std::process::Command::new("openssl")
                .args(args)
                .current_dir(dir.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "owned certificate generation failed"
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for file in ["ca.key", "server.key"] {
                std::fs::set_permissions(
                    dir.path().join(file),
                    std::fs::Permissions::from_mode(0o600),
                )
                .unwrap();
            }
        }
        let cert = CertificateDer::from_pem_file(dir.path().join("server.pem")).unwrap();
        let key = PrivateKeyDer::from_pem_file(dir.path().join("server.key")).unwrap();
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .unwrap();
        Self {
            dir,
            acceptor: TlsAcceptor::from(Arc::new(config)),
        }
    }
}

#[tokio::test]
async fn required_starttls_runner_exact_delivery() {
    let identity = Identity::new();
    let raw = b"From: author@example.invalid\r\nSubject: TLS tracer\r\n\r\n.secret body\r\n";
    let context = "{\"list_id\":\"test.example.invalid\"}";
    let (db, lease, _, sink) = tests::fixture_at(
        "sqlite::memory:",
        context,
        raw,
        vec!["member@example.invalid".into()],
    )
    .await;
    // Deserialize through the real config contract (unknown fields on RED are ignored).
    let mut config: listmngr_core::Config = serde_json::from_value(serde_json::json!({"mta": {
        "enabled": true, "smtp_tls": "required", "smtp_relay": sink.local_addr().unwrap().to_string(),
        "smtp_tls_server_name": "localhost", "smtp_tls_ca_file": identity.dir.path().join("ca.pem")
    }})).unwrap();
    config.mta.command_timeout_secs = 2;
    let role = MailRoleConfig::from_core(&config).unwrap();
    let expected = prepare_delivery(&db, &lease, raw, context).await.unwrap().0;
    let peer = async {
        let (stream, _) = sink.accept().await.unwrap();
        let mut plain = BufReader::new(stream);
        plain
            .get_mut()
            .write_all(b"220 owned relay\r\n")
            .await
            .unwrap();
        let mut line = String::new();
        plain.read_line(&mut line).await.unwrap();
        assert_eq!(line, "EHLO listmngr.invalid\r\n");
        plain
            .get_mut()
            .write_all(b"250-owned relay\r\n250 STARTTLS\r\n")
            .await
            .unwrap();
        line.clear();
        plain.read_line(&mut line).await.unwrap();
        assert_eq!(line, "STARTTLS\r\n", "no plaintext envelope is allowed");
        assert!(plain.buffer().is_empty());
        plain.get_mut().write_all(b"220 upgrade\r\n").await.unwrap();
        let tls = identity.acceptor.accept(plain.into_inner()).await.unwrap();
        let mut tls = BufReader::new(tls);
        for (expected, reply) in [
            ("EHLO listmngr.invalid\r\n", "250 owned TLS relay\r\n"),
            ("MAIL FROM:<test-bounces@example.invalid>\r\n", "250 ok\r\n"),
            ("RCPT TO:<member@example.invalid>\r\n", "250 ok\r\n"),
            ("DATA\r\n", "354 go\r\n"),
        ] {
            line.clear();
            tls.read_line(&mut line).await.unwrap();
            assert_eq!(line, expected);
            tls.get_mut().write_all(reply.as_bytes()).await.unwrap();
        }
        let mut captured = Vec::new();
        loop {
            let mut line = Vec::new();
            assert!(tls.read_until(b'\n', &mut line).await.unwrap() > 0);
            if line == b".\r\n" {
                break;
            }
            captured.extend_from_slice(if line.starts_with(b"..") {
                &line[1..]
            } else {
                &line
            });
        }
        assert_eq!(captured, expected);
        tls.get_mut().write_all(b"250 accepted\r\n").await.unwrap();
    };
    tokio::time::timeout(Duration::from_secs(6), async {
        tokio::join!(peer, deliver_one(&db, &role, lease.clone()));
    })
    .await
    .unwrap();
    let status: String =
        sqlx::query_scalar("SELECT status FROM delivery_recipients WHERE job_id=$1")
            .bind(lease.job.id.0.to_string())
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(status, "sent");
}

#[tokio::test]
async fn dsn_envid_uses_only_post_starttls_capability() {
    let identity = Identity::new();
    for (pre, post) in [(true, false), (false, true)] {
        let sink = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config:listmngr_core::Config=serde_json::from_value(serde_json::json!({"mta":{"smtp_tls":"required","smtp_relay":sink.local_addr().unwrap().to_string(),"smtp_tls_server_name":"localhost","smtp_tls_ca_file":identity.dir.path().join("ca.pem")}})).unwrap();
        let role = MailRoleConfig::from_core(&config).unwrap();
        let smtp = SmtpClientConfig {
            local_hostname: "listmngr.invalid".into(),
            command_timeout: Duration::from_secs(2),
        };
        let stream = tokio::net::TcpStream::connect(role.smtp_relay)
            .await
            .unwrap();
        let peer = async {
            let (stream, _) = sink.accept().await.unwrap();
            let mut plain = BufReader::new(stream);
            plain.get_mut().write_all(b"220 fixture\r\n").await.unwrap();
            let mut line = String::new();
            plain.read_line(&mut line).await.unwrap();
            assert!(line.starts_with("EHLO "));
            plain
                .get_mut()
                .write_all(if pre {
                    b"250-fixture\r\n250-DSN\r\n250 STARTTLS\r\n"
                } else {
                    b"250-fixture\r\n250 STARTTLS\r\n"
                })
                .await
                .unwrap();
            line.clear();
            plain.read_line(&mut line).await.unwrap();
            assert_eq!(line, "STARTTLS\r\n");
            plain.get_mut().write_all(b"220 upgrade\r\n").await.unwrap();
            let mut tls =
                BufReader::new(identity.acceptor.accept(plain.into_inner()).await.unwrap());
            line.clear();
            tls.read_line(&mut line).await.unwrap();
            assert!(line.starts_with("EHLO "));
            tls.get_mut()
                .write_all(if post {
                    b"250-fixture\r\n250 dSn\r\n"
                } else {
                    b"250-fixture\r\n250 SIZE 1000\r\n"
                })
                .await
                .unwrap();
            line.clear();
            let read = tls.read_line(&mut line).await;
            if post {
                assert!(read.unwrap() > 0);
                assert_eq!(
                    line,
                    "MAIL FROM:<sender@example.invalid> ENVID=1.test.fixture.tag\r\n"
                );
                tls.get_mut()
                    .write_all(b"450 known retry\r\n")
                    .await
                    .unwrap();
            } else {
                assert!(matches!(read, Ok(0) | Err(_)));
                assert!(line.is_empty(), "no MAIL after DSN disappears across TLS");
            }
        };
        let recipients = vec!["member@example.invalid".into()];
        let ((), outcome) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(
                peer,
                send_secure_with_envid(
                    stream,
                    &smtp,
                    &role.smtp_tls,
                    Some("sender@example.invalid"),
                    &recipients,
                    b"Subject: never sent\r\n\r\nbody",
                    Some("1.test.fixture.tag")
                )
            )
        })
        .await
        .unwrap();
        assert!(matches!(
            outcome.unwrap().results[0],
            RecipientStatus::TransientFailure(_)
        ));
    }
}
