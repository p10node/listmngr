//! A known DATA reply must not be hidden behind QUIT I/O.
use listmngr_mail::smtp::{RecipientStatus, SmtpClientConfig, send};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::test]
async fn final250_returns_without_waiting_for_quit() {
    let (client, server) = tokio::io::duplex(65536);
    let (accepted, received) = tokio::sync::oneshot::channel();
    let relay = tokio::spawn(async move {
        let (reader, mut writer) = tokio::io::split(server);
        let mut reader = BufReader::new(reader);
        writer.write_all(b"220 sink\r\n").await.unwrap();
        for reply in [
            b"250 ok\r\n".as_slice(),
            b"250 ok\r\n",
            b"250 ok\r\n",
            b"354 go\r\n",
        ] {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).await.unwrap() > 0);
            writer.write_all(reply).await.unwrap();
        }
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).await.unwrap() > 0);
            if line == ".\r\n" {
                break;
            }
        }
        writer.write_all(b"250 accepted\r\n").await.unwrap();
        accepted.send(()).unwrap();
        // Neither drain QUIT nor send its response. Keep both halves alive.
        std::future::pending::<()>().await;
    });
    let config = SmtpClientConfig {
        local_hostname: "local.invalid".into(),
        command_timeout: Duration::from_secs(30),
    };
    let delivery = tokio::spawn(async move {
        send(
            client,
            &config,
            None,
            &["a@example.invalid".into()],
            b"Subject: hi\r\n\r\nbody",
        )
        .await
        .unwrap()
    });
    received.await.unwrap();
    let result = tokio::time::timeout(Duration::from_millis(200), delivery).await;
    relay.abort();
    assert_eq!(
        result
            .expect("known DATA result was blocked on QUIT")
            .unwrap()
            .results,
        vec![RecipientStatus::Sent]
    );
}
