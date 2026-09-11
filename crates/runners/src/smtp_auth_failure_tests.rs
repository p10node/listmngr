use super::starttls_tests::Identity;
use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

const USER: &str = "owned-auth-user";
const PASSWORD: &str = "owned-auth-password";
fn encoded() -> String {
    STANDARD.encode(format!("\0{USER}\0{PASSWORD}"))
}

async fn upgraded(
    sink: &tokio::net::TcpListener,
    identity: &Identity,
) -> BufReader<tokio_rustls::server::TlsStream<tokio::net::TcpStream>> {
    let (stream, _) = sink.accept().await.unwrap();
    let mut plain = BufReader::new(stream);
    plain.get_mut().write_all(b"220 owned\r\n").await.unwrap();
    let mut line = String::new();
    plain.read_line(&mut line).await.unwrap();
    assert_eq!(line, "EHLO listmngr.invalid\r\n");
    // AUTH before TLS must never authorize post-upgrade authentication.
    plain
        .get_mut()
        .write_all(b"250-owned\r\n250-AUTH PLAIN\r\n250 STARTTLS\r\n")
        .await
        .unwrap();
    line.clear();
    plain.read_line(&mut line).await.unwrap();
    assert_eq!(line, "STARTTLS\r\n");
    assert!(plain.buffer().is_empty());
    plain.get_mut().write_all(b"220 upgrade\r\n").await.unwrap();
    let tls = identity.acceptor.accept(plain.into_inner()).await.unwrap();
    let mut tls = BufReader::new(tls);
    line.clear();
    tls.read_line(&mut line).await.unwrap();
    assert_eq!(line, "EHLO listmngr.invalid\r\n");
    tls
}

fn core_config(identity: &Identity, sink: &tokio::net::TcpListener) -> listmngr_core::Config {
    serde_json::from_value(serde_json::json!({"mta": {
        "enabled": true, "smtp_tls": "required", "smtp_relay": sink.local_addr().unwrap().to_string(),
        "smtp_tls_server_name": "localhost", "smtp_tls_ca_file": identity.dir.path().join("ca.pem"),
        "smtp_auth_username": USER, "smtp_auth_password": PASSWORD
    }})).unwrap()
}

#[tokio::test]
async fn smtp_auth_failures_retry_without_envelope_or_bounce() {
    let identity = Identity::new();
    for (extensions, reply, continuation) in [
        ("250-owned\r\n250 AUTH PLAIN\r\n", "535 ", false),
        ("250-owned\r\n250 AUTH PLAIN\r\n", "454 ", false),
        ("250-owned\r\n250 AUTH PLAIN\r\n", "250 ", false),
        ("250-owned\r\n250 AUTH PLAIN\r\n", "334 ", false), // nonempty challenge
        ("250-owned\r\n250 AUTH PLAIN\r\n", "334 ", true),  // second challenge
        ("250-owned\r\n250 AUTH PLAIN\r\n", "", false),     // timeout
        ("250-owned\r\n250 AUTH LOGIN\r\n", "", false),
        ("250-owned\r\n250 XAUTH PLAIN\r\n", "", false),
        ("250-owned\r\n250 AUTH XPLAIN\r\n", "", false),
        ("250-AUTH PLAIN\r\n250 SIZE 10000\r\n", "", false),
        ("250-owned\r\n250 SIZE 10000\r\n", "", false),
    ] {
        let (db, lease, _, sink) = tests::fixture_at(
            "sqlite::memory:",
            "{\"list_id\":\"test.example.invalid\"}",
            b"Subject: auth failure\r\n\r\nbody\r\n",
            vec!["member@example.invalid".into()],
        )
        .await;
        let config = core_config(&identity, &sink);
        let mut role = MailRoleConfig::from_core(&config).unwrap();
        role.command_timeout = Duration::from_millis(200);
        let debug = format!("{config:?} {role:?} {}", config.redacted_json());
        assert_redacted(&debug);
        let peer = async {
            let mut tls = upgraded(&sink, &identity).await;
            tls.get_mut()
                .write_all(extensions.as_bytes())
                .await
                .unwrap();
            if extensions == "250-owned\r\n250 AUTH PLAIN\r\n" {
                let mut line = String::new();
                tls.read_line(&mut line).await.unwrap();
                assert!(
                    line == format!("AUTH PLAIN {}\r\n", encoded()),
                    "incorrect AUTH (redacted)"
                );
                if continuation {
                    tls.get_mut().write_all(b"334 \r\n").await.unwrap();
                    line.clear();
                    tls.read_line(&mut line).await.unwrap();
                    assert!(
                        line == format!("{}\r\n", encoded()),
                        "incorrect continuation (redacted)"
                    );
                }
                if !reply.is_empty() {
                    // Hostile relay echoes both raw and base64 credentials.
                    tls.get_mut()
                        .write_all(format!("{reply}{USER} {PASSWORD} {}\r\n", encoded()).as_bytes())
                        .await
                        .unwrap();
                }
            }
            let mut remaining = Vec::new();
            let _ = tls.read_to_end(&mut remaining).await; // rustls EOF without close_notify is expected
            assert!(
                remaining.is_empty(),
                "AUTH failure leaked envelope or credentials"
            );
        };
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
        assert_eq!(status, "pending");
        let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM bounce_events")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(events, 0);
        let job = db.mail_queue().job(lease.job.id).await.unwrap();
        assert_redacted(&format!("{job:?}"));
    }
}

fn assert_redacted(value: &str) {
    for secret in [USER.to_owned(), PASSWORD.to_owned(), encoded()] {
        assert!(!value.contains(&secret), "credential disclosed (redacted)");
    }
}

#[tokio::test]
async fn smtp_auth_redacts_post_auth_hostile_reply() {
    hostile_reply(true).await;
}

#[tokio::test]
async fn smtp_auth_redacts_auth_error() {
    hostile_reply(false).await;
}

async fn hostile_reply(post_auth: bool) {
    let identity = Identity::new();
    let sink = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = core_config(&identity, &sink);
    let security = listmngr_mail::smtp::TransportSecurity::from_mta(&config.mta).unwrap();
    let client_config = SmtpClientConfig {
        local_hostname: "listmngr.invalid".into(),
        command_timeout: Duration::from_secs(2),
    };
    let peer = async {
        let mut tls = upgraded(&sink, &identity).await;
        tls.get_mut()
            .write_all(b"250-owned\r\n250 AUTH PLAIN\r\n")
            .await
            .unwrap();
        let mut line = String::new();
        tls.read_line(&mut line).await.unwrap();
        assert!(line == format!("AUTH PLAIN {}\r\n", encoded()));
        if !post_auth {
            tls.get_mut()
                .write_all(format!("535 {USER} {PASSWORD} {}\r\n", encoded()).as_bytes())
                .await
                .unwrap();
            let mut remaining = Vec::new();
            let _ = tls.read_to_end(&mut remaining).await;
            assert!(remaining.is_empty());
            return;
        }
        tls.get_mut().write_all(b"235 accepted\r\n").await.unwrap();
        line.clear();
        tls.read_line(&mut line).await.unwrap();
        assert_eq!(line, "MAIL FROM:<sender@example.invalid>\r\n");
        tls.get_mut()
            .write_all(format!("550 {USER} {PASSWORD} {}\r\n", encoded()).as_bytes())
            .await
            .unwrap();
    };
    let client = async {
        let stream = tokio::net::TcpStream::connect(sink.local_addr().unwrap())
            .await
            .unwrap();
        let outcome = send_secure(
            stream,
            &client_config,
            &security,
            Some("sender@example.invalid"),
            &["member@example.invalid".into()],
            b"body\r\n",
        )
        .await;
        if post_auth {
            assert!(matches!(
                outcome.as_ref().unwrap().results[0],
                RecipientStatus::RemotePermanentFailure { .. }
            ));
        } else {
            let error = outcome.as_ref().unwrap_err();
            assert_redacted(&error.to_string());
        }
        assert_redacted(&format!("{outcome:?}"));
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(peer, client);
    })
    .await
    .unwrap();
}
