//! One LMTP or SMTP session fed arbitrary bytes over an in-memory pipe,
//! against a handler that takes `@example.invalid` recipients and stores
//! nothing: the state machine, the line and size limits, the `DATA`
//! dot-stuffing and the reply counts must never panic or hang.
#![no_main]
use libfuzzer_sys::fuzz_target;
use listmngr_mail::lmtp::{LmtpHandler, Protocol, RecipientOutcome, serve_session_as};
use std::sync::OnceLock;
use std::time::Duration;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

struct Handler;

impl LmtpHandler for Handler {
    fn local_hostname(&self) -> &str {
        "fuzz.example.invalid"
    }
    fn max_message_bytes(&self) -> usize {
        64 * 1024
    }
    fn max_recipients(&self) -> usize {
        8
    }
    fn command_timeout(&self) -> Duration {
        Duration::from_millis(500)
    }
    async fn accept_recipient(&mut self, address: &str) -> Result<(), String> {
        if address.ends_with("@example.invalid") {
            Ok(())
        } else {
            Err("not a list here".into())
        }
    }
    async fn deliver(
        &mut self,
        _mail_from: Option<&str>,
        recipients: &[String],
        _data: &[u8],
    ) -> Vec<RecipientOutcome> {
        recipients
            .iter()
            .map(|_| RecipientOutcome {
                code: 250,
                detail: "queued".into(),
            })
            .collect()
    }
}

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a runtime")
    })
}

fuzz_target!(|data: &[u8]| {
    let Some((&first, script)) = data.split_first() else {
        return;
    };
    let protocol = if first % 2 == 0 {
        Protocol::Lmtp
    } else {
        Protocol::Smtp
    };
    let script = script.to_vec();
    runtime().block_on(async move {
        let (server, mut client) = tokio::io::duplex(16 * 1024);
        let peer = tokio::spawn(async move {
            let (mut reader, mut writer) = tokio::io::split(&mut client);
            let drain = async {
                let mut sink = [0u8; 4096];
                while let Ok(n) = reader.read(&mut sink).await {
                    if n == 0 {
                        break;
                    }
                }
            };
            let speak = async {
                let _ = writer.write_all(&script).await;
                let _ = writer.shutdown().await;
            };
            tokio::join!(drain, speak);
        });
        let mut handler = Handler;
        let _ = tokio::time::timeout(
            Duration::from_secs(5),
            serve_session_as(protocol, server, &mut handler),
        )
        .await;
        // Awaited, so the peer and its half of the pipe are dropped
        // here and not left in the runtime: a task that outlives the
        // run keeps a drained pipe buffer whose only pointer is
        // one-past-the-end, which LeakSanitizer reports as a leak at
        // exit (CI's Linux job; macOS has no LeakSanitizer).
        peer.abort();
        let _ = peer.await;
    });
});
