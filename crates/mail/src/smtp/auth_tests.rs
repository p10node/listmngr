use super::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::test]
async fn bare_auth_requires_a_credential_response_before_success() {
    for challenge in [false, true] {
        let auth =
            AuthPlain(STANDARD.encode(format!("\0{}\0{}", "u".repeat(255), "p".repeat(255))));
        let (client, server) = tokio::io::duplex(4096);
        let (read, mut writer) = tokio::io::split(client);
        let mut reader = BufReader::new(read);
        let config = SmtpClientConfig {
            local_hostname: "fixture.invalid".into(),
            command_timeout: std::time::Duration::from_secs(1),
        };
        let peer = async {
            let mut server = BufReader::new(server);
            let mut line = String::new();
            server.read_line(&mut line).await.unwrap();
            assert_eq!(line, "EHLO fixture.invalid\r\n");
            server
                .get_mut()
                .write_all(b"250-fixture\r\n250 AUTH PLAIN\r\n")
                .await
                .unwrap();
            line.clear();
            server.read_line(&mut line).await.unwrap();
            assert!(
                line == "AUTH PLAIN\r\n",
                "expected bounded AUTH without credential bytes"
            );
            if challenge {
                server.get_mut().write_all(b"334 \r\n").await.unwrap();
                line.clear();
                server.read_line(&mut line).await.unwrap();
                assert!(
                    line == format!("{}\r\n", auth.0),
                    "incorrect credential response (redacted)"
                );
            }
            server
                .get_mut()
                .write_all(b"235 authenticated\r\n")
                .await
                .unwrap();
        };
        let ((), result) = tokio::join!(peer, auth.authenticate(&mut writer, &mut reader, &config));
        assert_eq!(result.is_ok(), challenge);
    }
}
