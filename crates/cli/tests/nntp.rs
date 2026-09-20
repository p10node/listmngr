//! `listmngr nntp gate` on the real binary: the newsgroups of gatewayed
//! lists polled once against a news server, the report printed as JSON.
use assert_cmd::Command;
use listmngr_db::{Database, NewList};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// A news server with one group of two articles.
async fn news_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut read = BufReader::new(read);
                write.write_all(b"200 news ready\r\n").await.unwrap();
                loop {
                    let mut line = String::new();
                    if read.read_line(&mut line).await.unwrap() == 0 {
                        return;
                    }
                    let command = line.trim_end();
                    let reply: String = if command.eq_ignore_ascii_case("QUIT") {
                        let _ = write.write_all(b"205 bye\r\n").await;
                        return;
                    } else if command == "GROUP comp.lang.rust.lists" {
                        "211 2 10 11 comp.lang.rust.lists\r\n".into()
                    } else if let Some(number) = command.strip_prefix("ARTICLE ") {
                        format!(
                            "220 {number} <{number}@news.invalid>\r\nFrom: carol@elsewhere.invalid\r\nSubject: article {number}\r\nMessage-ID: <{number}@news.invalid>\r\n\r\nbody\r\n.\r\n"
                        )
                    } else {
                        "200 ok\r\n".into()
                    };
                    write.write_all(reply.as_bytes()).await.unwrap();
                }
            });
        }
    });
    port
}

#[test]
fn nntp_gate_polls_once_and_reports_each_list() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("news.db").display());
    let rt = tokio::runtime::Runtime::new().unwrap();
    let port = rt.block_on(async {
        let db = Database::connect(&url, 1).await.unwrap();
        db.migrate().await.unwrap();
        db.domains()
            .create("example.invalid", "", None)
            .await
            .unwrap();
        let list = db
            .lists()
            .create(NewList {
                list_id: "dev.example.invalid".parse().unwrap(),
                display_name: "Dev".into(),
                style: "legacy-default".into(),
            })
            .await
            .unwrap();
        db.lists()
            .update(
                &list.id,
                &serde_json::json!({"gateway_to_mail": true, "linked_newsgroup": "comp.lang.rust.lists"}),
            )
            .await
            .unwrap();
        db.pool().close().await;
        news_server().await
    });
    // Without a news server the command refuses to guess.
    Command::cargo_bin("listmngr")
        .unwrap()
        .env_clear()
        .current_dir(dir.path())
        .env("LISTMNGR__DATABASE__URL", &url)
        .args(["nntp", "gate"])
        .assert()
        .failure();
    // The first poll catches up: watermark 11, nothing gated.
    let output = Command::cargo_bin("listmngr")
        .unwrap()
        .env_clear()
        .current_dir(dir.path())
        .env("LISTMNGR__DATABASE__URL", &url)
        .env("LISTMNGR__NNTP__HOST", "127.0.0.1")
        .env("LISTMNGR__NNTP__PORT", port.to_string())
        .args(["nntp", "gate"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(output.stdout.trim_ascii()).unwrap();
    assert_eq!(report["list_id"], "dev.example.invalid");
    assert_eq!(report["newsgroup"], "comp.lang.rust.lists");
    assert_eq!(report["watermark"], 11);
    assert_eq!(report["gated"], 0);
    assert_eq!(report["error"], serde_json::Value::Null);
    // The server's group grew to 11 only; a second poll gates nothing more.
    let output = Command::cargo_bin("listmngr")
        .unwrap()
        .env_clear()
        .current_dir(dir.path())
        .env("LISTMNGR__DATABASE__URL", &url)
        .env("LISTMNGR__NNTP__HOST", "127.0.0.1")
        .env("LISTMNGR__NNTP__PORT", port.to_string())
        .args(["nntp", "gate"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(output.stdout.trim_ascii()).unwrap();
    assert_eq!(report["gated"], 0);
    assert_eq!(report["watermark"], 11);
    // The runtime must outlive the server the binary talked to.
    drop(rt);
}
