use super::starttls_tests::Identity;
use super::*;
use base64::Engine as _;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
#[tokio::test]
async fn smtp_auth_runner_exact_delivery() {
    auth_delivery(false, None).await;
}

#[tokio::test]
async fn smtp_auth_runner_empty_continuation() {
    auth_delivery(true, None).await;
}

#[tokio::test]
async fn smtp_auth_runner_long_credentials_use_bounded_command() {
    for lengths in [(185, 185), (186, 185), (255, 255)] {
        auth_delivery(false, Some(lengths)).await;
    }
}

fn auth_config(
    identity: &Identity,
    sink: &tokio::net::TcpListener,
    file: bool,
) -> listmngr_core::Config {
    if file {
        let password_file = identity.dir.path().join("smtp-password");
        std::fs::write(&password_file, b"owned-auth-password\r\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&password_file, std::fs::Permissions::from_mode(0o600))
                .unwrap();
        }
        let path = identity.dir.path().join("auth.toml");
        std::fs::write(
            &path,
            format!(
                r#"[mta]
enabled = true
smtp_tls = "required"
smtp_relay = "{}"
smtp_tls_server_name = "localhost"
smtp_tls_ca_file = {:?}
smtp_auth_username = "owned-auth-user"
smtp_auth_password_file = {:?}
"#,
                sink.local_addr().unwrap(),
                identity.dir.path().join("ca.pem"),
                password_file
            ),
        )
        .unwrap();
        listmngr_core::Config::load(Some(&path)).unwrap()
    } else {
        serde_json::from_value(serde_json::json!({"mta": {
        "enabled": true, "smtp_tls": "required", "smtp_relay": sink.local_addr().unwrap().to_string(),
        "smtp_auth_username": "owned-auth-user", "smtp_auth_password": "owned-auth-password",
        "smtp_tls_server_name": "localhost", "smtp_tls_ca_file": identity.dir.path().join("ca.pem")
    }})).unwrap()
    }
}

fn credential_response(
    config: &mut listmngr_core::Config,
    lengths: Option<(usize, usize)>,
) -> String {
    let (user, password) = lengths.map_or_else(
        || {
            (
                "owned-auth-user".to_owned(),
                "owned-auth-password".to_owned(),
            )
        },
        |(user, password)| ("u".repeat(user), "p".repeat(password)),
    );
    if lengths.is_some() {
        config.mta.smtp_auth_username = Some(user.clone().into());
        config.mta.smtp_auth_password = Some(password.clone().into());
        config.mta.smtp_auth_password_file = None;
    }
    format!(
        "{}\r\n",
        base64::engine::general_purpose::STANDARD.encode(format!("\0{user}\0{password}"))
    )
}

async fn auth_delivery(continuation: bool, lengths: Option<(usize, usize)>) {
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
    let mut config = auth_config(&identity, &sink, continuation);
    let response = credential_response(&mut config, lengths);
    config.mta.command_timeout_secs = 2;
    let role = MailRoleConfig::from_core(&config).unwrap();
    assert_eq!(
        role.smtp_tls.authentication(),
        listmngr_mail::smtp::AuthenticationPolicy::Plain
    );
    let expected = prepare_delivery(&db, &lease, raw, context).await.unwrap().0;
    let inline = format!("AUTH PLAIN {response}");
    let bare = inline.len() > 512;
    let auth_line = if bare {
        "AUTH PLAIN\r\n".to_owned()
    } else {
        inline
    };
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
            (
                "EHLO listmngr.invalid\r\n",
                "250-owned TLS relay\r\n250 AUTH PLAIN\r\n",
            ),
            (auth_line.as_str(), "235 authenticated\r\n"),
            ("MAIL FROM:<test-bounces@example.invalid>\r\n", "250 ok\r\n"),
            ("RCPT TO:<member@example.invalid>\r\n", "250 ok\r\n"),
            ("DATA\r\n", "354 go\r\n"),
        ] {
            line.clear();
            tls.read_line(&mut line).await.unwrap();
            assert!(line == expected, "unexpected SMTP command (redacted)");
            if (continuation || bare) && expected == auth_line {
                tls.get_mut().write_all(b"334 \r\n").await.unwrap();
                line.clear();
                tls.read_line(&mut line).await.unwrap();
                assert!(line == response, "missing continuation (redacted)");
            }
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
