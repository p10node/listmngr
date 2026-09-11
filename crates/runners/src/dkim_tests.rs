use super::*;
use std::path::Path;
#[path = "dkim_oracle.rs"]
pub(super) mod oracle;

#[cfg(unix)]
fn private_fixture_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

pub(super) fn key(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("owned-test.pem");
    let output = std::process::Command::new("openssl")
        .args([
            "genpkey",
            "-algorithm",
            "RSA",
            "-pkeyopt",
            "rsa_keygen_bits:2048",
            "-out",
        ])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "owned RSA fixture generation failed"
    );
    #[cfg(unix)]
    private_fixture_permissions(&path);
    path
}

pub(super) fn signing_config(path: &Path, domain: &str) -> listmngr_core::Config {
    serde_json::from_value(
        serde_json::json!({"mta":{"smtp_tls":"plaintext_trusted_relay","dkim_signing":[{
            "domain":domain,"selector":"fixture","private_key_file":path
        }]}}),
    )
    .unwrap()
}

pub(super) async fn capture(
    sink: &tokio::net::TcpListener,
    sender: &str,
    recipient: &str,
) -> Vec<u8> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (stream, _) = sink.accept().await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    writer.write_all(b"220 fixture\r\n").await.unwrap();
    let from = format!("MAIL FROM:<{sender}>");
    let rcpt = format!("RCPT TO:<{recipient}>");
    for (prefix, reply) in [
        ("EHLO", "250 fixture\r\n"),
        (from.as_str(), "250 ok\r\n"),
        (rcpt.as_str(), "250 ok\r\n"),
        ("DATA", "354 go\r\n"),
    ] {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).await.unwrap() > 0);
        assert!(line.starts_with(prefix), "{line}");
        writer.write_all(reply.as_bytes()).await.unwrap();
    }
    let mut data = Vec::new();
    loop {
        let mut line = Vec::new();
        assert!(reader.read_until(b'\n', &mut line).await.unwrap() > 0);
        if line == b".\r\n" {
            break;
        }
        data.extend_from_slice(if line.starts_with(b"..") {
            &line[1..]
        } else {
            &line
        });
    }
    writer.write_all(b"250 accepted\r\n").await.unwrap();
    data
}

#[tokio::test]
async fn dkim_production_outgoing_signs_authoritative_domain() {
    let dir = tempfile::tempdir().unwrap();
    let path = key(dir.path());
    let raw = b"From: Author <author@foreign.invalid>\r\nSubject: DKIM tracer\r\nMessage-ID: <dkim@foreign.invalid>\r\n\r\ncontrolled body\r\n.dot line\r\n \t\r\n";
    let (db, lease, original, sink) = tests::fixture_at(
        "sqlite::memory:",
        "{\"list_id\":\"test.example.invalid\"}",
        raw,
        vec!["member@example.invalid".into()],
    )
    .await;
    let mut role = MailRoleConfig::from_core(&signing_config(&path, "example.invalid")).unwrap();
    role.smtp_relay = original.smtp_relay;
    role.command_timeout = Duration::from_secs(2);
    let (sent, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(
            capture(
                &sink,
                "test-bounces@example.invalid",
                "member@example.invalid"
            ),
            deliver_one(&db, &role, lease.clone())
        )
    })
    .await
    .unwrap();
    assert!(
        listmngr_mail::header_value(&sent, "DKIM-Signature").is_some(),
        "configured production delivery lacks DKIM signature"
    );
    oracle::verify_capture(&sent, &path).await;
    assert_eq!(
        db.mail_queue()
            .message(lease.job.message_id)
            .await
            .unwrap()
            .raw,
        raw
    );
}

#[test]
fn dkim_invalid_config_rejected_before_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let path = key(dir.path());
    for (domain, selector) in [
        ("bad\r\ndomain", "fixture"),
        ("example.invalid", "bad; s=evil"),
        ("", "fixture"),
        ("-bad.invalid", "fixture"),
    ] {
        let mut config = signing_config(&path, domain);
        config.mta.dkim_signing[0].selector = selector.into();
        assert!(
            MailRoleConfig::from_core(&config).is_err(),
            "invalid DKIM identity accepted"
        );
    }
    let mut config = signing_config(&path, "example.invalid");
    config
        .mta
        .dkim_signing
        .push(config.mta.dkim_signing[0].clone());
    assert!(
        MailRoleConfig::from_core(&config).is_err(),
        "duplicate signing domain accepted"
    );
}

#[tokio::test]
async fn dkim_missing_from_shunts_locally_without_mailbox_event() {
    let dir = tempfile::tempdir().unwrap();
    let path = key(dir.path());
    let (db, lease, original, sink) = tests::fixture_at(
        "sqlite::memory:",
        "{\"list_id\":\"test.example.invalid\"}",
        b"Subject: missing From\r\n\r\nbody",
        vec!["member@example.invalid".into()],
    )
    .await;
    let mut role = MailRoleConfig::from_core(&signing_config(&path, "example.invalid")).unwrap();
    role.smtp_relay = original.smtp_relay;
    role.command_timeout = Duration::from_millis(30);
    deliver_one(&db, &role, lease.clone()).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(50), sink.accept())
            .await
            .is_err(),
        "local signing failure opened SMTP"
    );
    assert_eq!(
        db.mail_queue().job(lease.job.id).await.unwrap().state,
        listmngr_db::mail_queue::JobState::Shunted
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM bounce_events")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn dkim_unconfigured_domain_preserves_unsigned_delivery() {
    let dir = tempfile::tempdir().unwrap();
    let path = key(dir.path());
    for config in [
        tests::plaintext_config(),
        signing_config(&path, "foreign.invalid"),
    ] {
        let raw = b"From: author@foreign.invalid\r\nSubject: unsigned\r\n\r\nbody\r\n";
        let (db, lease, original, sink) = tests::fixture_at(
            "sqlite::memory:",
            "{\"list_id\":\"test.example.invalid\",\"dkim_domain\":\"foreign.invalid\"}",
            raw,
            vec!["member@example.invalid".into()],
        )
        .await;
        let mut role = MailRoleConfig::from_core(&config).unwrap();
        role.smtp_relay = original.smtp_relay;
        let (sent, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(
                capture(
                    &sink,
                    "test-bounces@example.invalid",
                    "member@example.invalid"
                ),
                deliver_one(&db, &role, lease)
            )
        })
        .await
        .unwrap();
        assert!(listmngr_mail::header_value(&sent, "DKIM-Signature").is_none());
        assert!(sent.ends_with(b"body\r\n"));
    }
}

#[test]
fn dkim_bad_keys_are_redacted_and_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fixture.pem");
    let config = signing_config(&path, "example.invalid");
    assert!(MailRoleConfig::from_core(&config).is_err());
    std::fs::write(&path, "NOT A KEY: sentinel-private-material").unwrap();
    #[cfg(unix)]
    private_fixture_permissions(&path);
    let error = MailRoleConfig::from_core(&config).unwrap_err();
    assert!(!format!("{error:?} {error}").contains("sentinel-private-material"));
    for bits in ["rsa_keygen_bits:1024", "rsa_keygen_bits:2048"] {
        std::fs::remove_file(&path).unwrap();
        let result = std::process::Command::new("openssl")
            .args(["genpkey", "-algorithm", "RSA", "-pkeyopt", bits, "-out"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(result.status.success());
        #[cfg(unix)]
        private_fixture_permissions(&path);
        assert_eq!(
            MailRoleConfig::from_core(&config).is_ok(),
            bits.ends_with("2048")
        );
    }
    let role = MailRoleConfig::from_core(&config).unwrap();
    assert!(format!("{role:?}").contains("[REDACTED]"));
    assert!(
        role.dkim
            .sign(
                "example.invalid",
                b"From: a@example.invalid\r\nFrom: b@example.invalid\r\n\r\nbody".to_vec()
            )
            .is_err()
    );
    assert!(
        role.dkim
            .sign(
                "example.invalid",
                b"From: a@example.invalid\r\n\r\nbare\rCR".to_vec()
            )
            .is_err()
    );
}

#[test]
fn dkim_wire_normalization_and_private_file_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let path = key(dir.path());
    let config = signing_config(&path, "example.invalid");
    let role = MailRoleConfig::from_core(&config).unwrap();
    let signed = role
        .dkim
        .sign(
            "example.invalid",
            b"From: author@elsewhere.invalid\nSubject: LF\n\nbody\n.dot".to_vec(),
        )
        .unwrap();
    assert!(
        signed
            .iter()
            .enumerate()
            .all(|(i, b)| *b != b'\n' || (i > 0 && signed[i - 1] == b'\r')),
        "bare LF signed without transport normalization"
    );
    assert!(signed.ends_with(b"body\r\n.dot\r\n"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            MailRoleConfig::from_core(&config).is_err(),
            "world readable DKIM key accepted"
        );
    }
}
