//! The experimental inbound SMTP listener on the real mail role: a list's
//! address is taken over plain SMTP into the `in` queue, any other
//! address is refused at `RCPT`, and `LHLO` is not the greeting.
use listmngr_core::Config;
use listmngr_db::{Database, NewList};
use listmngr_runners::MailRoleConfig;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

async fn line(reader: &mut BufReader<TcpStream>) -> String {
    let mut out = String::new();
    tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut out))
        .await
        .expect("reply in time")
        .unwrap();
    out
}

/// A reply, its continuation lines included.
async fn reply(reader: &mut BufReader<TcpStream>) -> String {
    let mut out = String::new();
    loop {
        let next = line(reader).await;
        let last = next.len() < 4 || next.as_bytes()[3] == b' ';
        out.push_str(&next);
        if last {
            return out;
        }
    }
}

async fn send(reader: &mut BufReader<TcpStream>, text: &str) {
    reader
        .get_mut()
        .write_all(format!("{text}\r\n").as_bytes())
        .await
        .unwrap();
}

#[tokio::test]
async fn the_inbound_smtp_listener_takes_list_mail_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}/smtp.db?mode=rwc", dir.path().display());
    let db = Database::connect(&url, 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    db.lists()
        .create(NewList {
            list_id: "dev.example.invalid".parse().unwrap(),
            display_name: "dev".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let mut config = Config::default();
    config.mta.inbound_smtp_listen = Some("127.0.0.1:0".into());
    let role = MailRoleConfig::from_core(&config).unwrap();
    assert!(role.inbound_smtp_listen.is_some());
    let lmtp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let smtp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = smtp.local_addr().unwrap().port();
    let (stop, rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(listmngr_runners::serve_mail_role(
        db.clone(),
        config,
        role,
        lmtp,
        Some(smtp),
        rx,
    ));
    let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let mut client = BufReader::new(stream);
    let greeting = reply(&mut client).await;
    assert!(
        greeting.starts_with("220 ") && greeting.contains("ESMTP"),
        "{greeting}"
    );
    send(&mut client, "LHLO mx.sender.invalid").await;
    assert!(reply(&mut client).await.starts_with("500 "));
    send(&mut client, "EHLO mx.sender.invalid").await;
    let capabilities = reply(&mut client).await;
    assert!(
        capabilities.contains("250-") && capabilities.contains("SIZE "),
        "{capabilities}"
    );
    send(&mut client, "MAIL FROM:<alice@elsewhere.invalid>").await;
    assert!(reply(&mut client).await.starts_with("250 "));
    // Only a list's address: nothing is relayed.
    send(&mut client, "RCPT TO:<nobody@example.invalid>").await;
    assert!(reply(&mut client).await.starts_with("550 "));
    send(&mut client, "RCPT TO:<someone@elsewhere.invalid>").await;
    assert!(reply(&mut client).await.starts_with("550 "));
    send(&mut client, "RCPT TO:<dev@example.invalid>").await;
    assert!(reply(&mut client).await.starts_with("250 "));
    send(&mut client, "DATA").await;
    assert!(reply(&mut client).await.starts_with("354 "));
    send(&mut client, "From: alice@elsewhere.invalid").await;
    send(&mut client, "To: dev@example.invalid").await;
    send(&mut client, "Subject: hello over smtp").await;
    send(&mut client, "Message-ID: <smtp-1@elsewhere.invalid>").await;
    send(&mut client, "").await;
    send(&mut client, "A post that came in over SMTP.").await;
    send(&mut client, ".").await;
    let accepted = reply(&mut client).await;
    assert!(accepted.starts_with("250 "), "{accepted}");
    // One reply, then the session goes on.
    send(&mut client, "NOOP").await;
    assert_eq!(reply(&mut client).await, "250 2.0.0 ok\r\n");
    send(&mut client, "QUIT").await;
    assert!(reply(&mut client).await.starts_with("221 "));
    // The post is in the `in` queue as any LMTP post would be.
    let queued: Vec<(String, String)> = sqlx::query_as(
        "SELECT q.queue, m.external_id FROM queue_jobs q JOIN messages m ON m.id=q.message_id",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(
        queued,
        [("in".to_owned(), "smtp-1@elsewhere.invalid".to_owned())]
    );
    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("the role stops")
        .unwrap()
        .unwrap();
}
