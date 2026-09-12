use listmngr_mail::smtp::{RecipientStatus, SmtpClientConfig, send};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

fn config() -> SmtpClientConfig {
    SmtpClientConfig {
        local_hostname: "listmngr.example.invalid".into(),
        command_timeout: Duration::from_secs(2),
    }
}

/// Drives a scripted fake relay: for each inbound command line, replies with
/// the corresponding scripted response(s), then returns every line it saw.
async fn fake_relay(
    server: tokio::io::DuplexStream,
    greeting: &str,
    script: Vec<(&'static str, &'static str)>,
) -> Vec<String> {
    let (read_half, mut write_half) = tokio::io::split(server);
    let mut reader = BufReader::new(read_half);
    write_half.write_all(greeting.as_bytes()).await.unwrap();
    let mut seen = Vec::new();
    for (expected_prefix, response) in script {
        if expected_prefix == "." {
            // Consume the (possibly multi-line) DATA payload up to its dot terminator.
            loop {
                let mut line = String::new();
                let n = reader.read_line(&mut line).await.unwrap();
                assert!(n > 0, "connection closed while awaiting DATA terminator");
                if line == ".\r\n" {
                    seen.push(line);
                    break;
                }
            }
            write_half.write_all(response.as_bytes()).await.unwrap();
            continue;
        }
        let mut line = String::new();
        let n = reader.read_line(&mut line).await.unwrap();
        if n == 0 {
            break;
        }
        assert!(
            line.starts_with(expected_prefix),
            "expected {expected_prefix:?}, got {line:?}"
        );
        seen.push(line);
        write_half.write_all(response.as_bytes()).await.unwrap();
    }
    // Drain anything else the client sends (e.g. DATA payload + QUIT) without asserting.
    let mut rest = String::new();
    let _ = tokio::time::timeout(Duration::from_millis(50), reader.read_to_string(&mut rest)).await;
    seen
}

#[tokio::test]
async fn delivers_to_all_accepted_recipients_on_success() {
    let (client, server) = tokio::io::duplex(65536);
    let relay = tokio::spawn(fake_relay(
        server,
        "220 relay.invalid ESMTP\r\n",
        vec![
            ("EHLO ", "250 relay.invalid\r\n"),
            ("MAIL FROM:", "250 ok\r\n"),
            ("RCPT TO:<a@", "250 ok\r\n"),
            ("RCPT TO:<b@", "250 ok\r\n"),
            ("DATA", "354 go\r\n"),
            (".", "250 accepted\r\n"),
        ],
    ));
    let outcome = send(
        client,
        &config(),
        Some("alice@example.invalid"),
        &["a@example.invalid".into(), "b@example.invalid".into()],
        b"Subject: hi\r\n\r\nbody\r\n",
    )
    .await
    .unwrap();
    assert_eq!(
        outcome.results,
        vec![RecipientStatus::Sent, RecipientStatus::Sent]
    );
    relay.await.unwrap();
}

#[tokio::test]
async fn permanent_rcpt_failure_does_not_block_other_recipients() {
    let (client, server) = tokio::io::duplex(65536);
    let relay = tokio::spawn(fake_relay(
        server,
        "220 relay.invalid\r\n",
        vec![
            ("EHLO ", "250 relay.invalid\r\n"),
            ("MAIL FROM:", "250 ok\r\n"),
            ("RCPT TO:<bad@", "550 no such user\r\n"),
            ("RCPT TO:<good@", "250 ok\r\n"),
            ("DATA", "354 go\r\n"),
            (".", "250 accepted\r\n"),
        ],
    ));
    let outcome = send(
        client,
        &config(),
        Some("alice@example.invalid"),
        &["bad@example.invalid".into(), "good@example.invalid".into()],
        b"body",
    )
    .await
    .unwrap();
    assert!(matches!(
        outcome.results[0],
        RecipientStatus::RemotePermanentFailure { .. }
    ));
    assert_eq!(outcome.results[1], RecipientStatus::Sent);
    relay.await.unwrap();
}

#[tokio::test]
async fn all_recipients_rejected_skips_data_entirely() {
    let (client, server) = tokio::io::duplex(65536);
    let relay = tokio::spawn(fake_relay(
        server,
        "220 relay.invalid\r\n",
        vec![
            ("EHLO ", "250 relay.invalid\r\n"),
            ("MAIL FROM:", "250 ok\r\n"),
            ("RCPT TO:", "550 no such user\r\n"),
            ("RSET", "250 ok\r\n"),
        ],
    ));
    let outcome = send(
        client,
        &config(),
        Some("alice@example.invalid"),
        &["nobody@example.invalid".into()],
        b"body",
    )
    .await
    .unwrap();
    assert!(matches!(
        outcome.results[0],
        RecipientStatus::RemotePermanentFailure { .. }
    ));
    relay.await.unwrap();
}

#[tokio::test]
async fn transient_data_failure_is_reported_as_retryable_for_accepted_recipients() {
    let (client, server) = tokio::io::duplex(65536);
    let relay = tokio::spawn(fake_relay(
        server,
        "220 relay.invalid\r\n",
        vec![
            ("EHLO ", "250 relay.invalid\r\n"),
            ("MAIL FROM:", "250 ok\r\n"),
            ("RCPT TO:", "250 ok\r\n"),
            ("DATA", "354 go\r\n"),
            (".", "451 temporary local problem\r\n"),
        ],
    ));
    let outcome = send(
        client,
        &config(),
        Some("alice@example.invalid"),
        &["a@example.invalid".into()],
        b"body",
    )
    .await
    .unwrap();
    assert!(matches!(
        outcome.results[0],
        RecipientStatus::TransientFailure(_)
    ));
    relay.await.unwrap();
}

#[tokio::test]
async fn dot_stuffs_a_leading_dot_in_the_body() {
    let (client, server) = tokio::io::duplex(65536);
    let relay = tokio::spawn(async move {
        let (read_half, mut write_half) = tokio::io::split(server);
        let mut reader = BufReader::new(read_half);
        write_half
            .write_all(b"220 relay.invalid\r\n")
            .await
            .unwrap();
        for response in [
            "250 relay.invalid\r\n",
            "250 ok\r\n",
            "250 ok\r\n",
            "354 go\r\n",
        ] {
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            write_half.write_all(response.as_bytes()).await.unwrap();
        }
        let mut body = Vec::new();
        loop {
            let mut line = Vec::new();
            reader.read_until(b'\n', &mut line).await.unwrap();
            if line == b".\r\n" {
                break;
            }
            body.extend_from_slice(&line);
        }
        write_half.write_all(b"250 accepted\r\n").await.unwrap();
        body
    });
    send(
        client,
        &config(),
        Some("alice@example.invalid"),
        &["a@example.invalid".into()],
        b".leading dot\r\nnormal\r\n",
    )
    .await
    .unwrap();
    let body = relay.await.unwrap();
    assert_eq!(body, b"..leading dot\r\nnormal\r\n");
}

#[tokio::test]
async fn non_ascii_greeting_bytes_never_panic_and_are_treated_as_malformed() {
    // A char boundary landing mid-codepoint at a fixed byte offset panics if the
    // response is sliced as `&str`; this must instead be a graceful failure.
    for greeting in ["\u{1F600}\r\n", "250\u{E9}\r\n"] {
        let (client, server) = tokio::io::duplex(65536);
        let mut server = server;
        let bytes = greeting.as_bytes().to_vec();
        let relay = tokio::spawn(async move {
            server.write_all(&bytes).await.unwrap();
            let mut rest = Vec::new();
            let _ = tokio::time::timeout(Duration::from_millis(100), server.read_to_end(&mut rest))
                .await;
        });
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            send(
                client,
                &config(),
                Some("alice@example.invalid"),
                &["a@example.invalid".into()],
                b"body",
            ),
        )
        .await
        .expect("send must not hang on a malformed greeting");
        assert!(
            result.is_err(),
            "malformed greeting {greeting:?} must be a graceful error, not Sent"
        );
        relay.await.unwrap();
    }
}

#[tokio::test]
async fn unbounded_continuation_lines_do_not_hang_or_grow_forever() {
    let (client, server) = tokio::io::duplex(1 << 20);
    let mut config = config();
    config.command_timeout = Duration::from_millis(150);
    let relay = tokio::spawn(async move {
        let mut server = server;
        server.write_all(b"220 relay.invalid\r\n").await.unwrap();
        // Keep sending well-formed continuation lines and never send a final line.
        loop {
            if server.write_all(b"250-still-going\r\n").await.is_err() {
                break;
            }
            if tokio::time::timeout(
                Duration::from_millis(1),
                tokio::time::sleep(Duration::from_millis(1)),
            )
            .await
            .is_ok()
            {}
        }
    });
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        send(
            client,
            &config,
            Some("alice@example.invalid"),
            &["a@example.invalid".into()],
            b"body",
        ),
    )
    .await
    .expect("an endless continuation response must not hang send() indefinitely");
    assert!(result.is_ok());
    relay.abort();
}

#[tokio::test]
async fn mismatched_continuation_code_is_never_reported_as_sent() {
    let (client, server) = tokio::io::duplex(65536);
    let relay = tokio::spawn(fake_relay(
        server,
        "220 relay.invalid\r\n",
        vec![
            ("EHLO ", "250 relay.invalid\r\n"),
            ("MAIL FROM:", "250 ok\r\n"),
            ("RCPT TO:", "250 ok\r\n"),
            ("DATA", "354 go\r\n"),
            // The final DATA reply desyncs: first line claims 250 (continuing),
            // but the actual final line is 550. This must never resolve Sent.
            (".", "250-pending\r\n550 rejected\r\n"),
        ],
    ));
    let outcome = send(
        client,
        &config(),
        Some("alice@example.invalid"),
        &["a@example.invalid".into()],
        b"body",
    )
    .await
    .unwrap();
    assert_ne!(outcome.results[0], RecipientStatus::Sent);
    relay.await.unwrap();
}

#[tokio::test]
async fn only_exact_250_completes_data_as_sent() {
    let (client, server) = tokio::io::duplex(65536);
    let relay = tokio::spawn(fake_relay(
        server,
        "220 relay.invalid\r\n",
        vec![
            ("EHLO ", "250 relay.invalid\r\n"),
            ("MAIL FROM:", "250 ok\r\n"),
            ("RCPT TO:", "250 ok\r\n"),
            ("DATA", "354 go\r\n"),
            // 221 is QUIT's code, not a valid DATA-completion reply; a desynced
            // transcript must never be interpreted as delivery success.
            (".", "221 bye\r\n"),
        ],
    ));
    let outcome = send(
        client,
        &config(),
        Some("alice@example.invalid"),
        &["a@example.invalid".into()],
        b"body",
    )
    .await
    .unwrap();
    assert_ne!(outcome.results[0], RecipientStatus::Sent);
    relay.await.unwrap();
}

#[tokio::test]
async fn hostname_and_envelope_sender_crlf_injection_is_rejected_before_any_io() {
    let (client, server) = tokio::io::duplex(65536);
    let relay = tokio::spawn(async move {
        let mut server = server;
        // A real greeting is sent so a validation gap would let the client
        // proceed to actually write its (malicious) EHLO onto the wire. The
        // client may reject and drop the stream before this write lands;
        // that race itself is evidence validation ran before any I/O.
        let _ = server.write_all(b"220 relay.invalid\r\n").await;
        let mut buf = Vec::new();
        let _ =
            tokio::time::timeout(Duration::from_millis(150), server.read_to_end(&mut buf)).await;
        buf
    });
    let mut malicious_config = config();
    malicious_config.local_hostname = "evil\r\nMAIL FROM:<bounce@attacker.invalid".into();
    let result = send(
        client,
        &malicious_config,
        Some("alice@example.invalid"),
        &["a@example.invalid".into()],
        b"body",
    )
    .await;
    assert!(result.is_err(), "a CRLF-bearing hostname must be rejected");
    let seen = relay.await.unwrap();
    assert!(
        seen.is_empty(),
        "no bytes must reach the wire for a rejected hostname, saw {:?}",
        String::from_utf8_lossy(&seen)
    );
}

#[tokio::test]
async fn envelope_sender_crlf_injection_is_rejected_before_any_command_is_sent() {
    let (client, server) = tokio::io::duplex(65536);
    let relay = tokio::spawn(async move {
        let mut server = server;
        // See the hostname-injection test above for why this write may race
        // the client's early, pre-I/O rejection and fail with BrokenPipe.
        let _ = server.write_all(b"220 relay.invalid\r\n").await;
        let mut buf = Vec::new();
        let _ =
            tokio::time::timeout(Duration::from_millis(150), server.read_to_end(&mut buf)).await;
        buf
    });
    let result = send(
        client,
        &config(),
        Some("alice@example.invalid>\r\nRCPT TO:<evil@attacker.invalid"),
        &["a@example.invalid".into()],
        b"body",
    )
    .await;
    assert!(
        result.is_err(),
        "a CRLF-bearing envelope sender must be rejected"
    );
    let seen = relay.await.unwrap();
    assert!(
        seen.is_empty(),
        "no bytes must reach the wire for a rejected envelope sender, saw {:?}",
        String::from_utf8_lossy(&seen)
    );
}

#[tokio::test]
async fn recipient_crlf_injection_is_isolated_to_that_recipient() {
    let (client, server) = tokio::io::duplex(65536);
    let relay = tokio::spawn(fake_relay(
        server,
        "220 relay.invalid\r\n",
        vec![
            ("EHLO ", "250 relay.invalid\r\n"),
            ("MAIL FROM:", "250 ok\r\n"),
            ("RCPT TO:<good@", "250 ok\r\n"),
            ("DATA", "354 go\r\n"),
            (".", "250 accepted\r\n"),
        ],
    ));
    let malicious = "good2@example.invalid>\r\nRCPT TO:<extra@attacker.invalid".to_owned();
    let outcome = send(
        client,
        &config(),
        Some("alice@example.invalid"),
        &["good@example.invalid".into(), malicious],
        b"body",
    )
    .await
    .unwrap();
    assert_eq!(outcome.results[0], RecipientStatus::Sent);
    assert!(matches!(
        outcome.results[1],
        RecipientStatus::PermanentFailure(_)
    ));
    let seen = relay.await.unwrap();
    // The relay must only ever have seen exactly one legitimate RCPT command.
    assert_eq!(
        seen.iter().filter(|line| line.starts_with("RCPT")).count(),
        1
    );
}

#[tokio::test]
async fn a_permanent_rcpt_failure_survives_a_later_recipients_connection_loss() {
    let (client, server) = tokio::io::duplex(65536);
    let relay = tokio::spawn(async move {
        let (read_half, mut write_half) = tokio::io::split(server);
        let mut reader = BufReader::new(read_half);
        write_half
            .write_all(b"220 relay.invalid\r\n")
            .await
            .unwrap();
        for response in ["250 relay.invalid\r\n", "250 ok\r\n"] {
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            write_half.write_all(response.as_bytes()).await.unwrap();
        }
        // RCPT A: permanent rejection.
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert!(line.starts_with("RCPT TO:<a@"));
        write_half.write_all(b"550 no such user\r\n").await.unwrap();
        // RCPT B: read the command, then drop the connection without replying.
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert!(line.starts_with("RCPT TO:<b@"));
        drop(write_half);
    });
    let outcome = send(
        client,
        &config(),
        Some("alice@example.invalid"),
        &["a@example.invalid".into(), "b@example.invalid".into()],
        b"body",
    )
    .await
    .unwrap();
    assert!(
        matches!(
            outcome.results[0],
            RecipientStatus::RemotePermanentFailure { .. }
        ),
        "recipient A's known 550 must survive B's later connection loss, got {:?}",
        outcome.results[0]
    );
    assert_ne!(
        outcome.results[1],
        RecipientStatus::Sent,
        "recipient B's outcome is unknown, but must never be Sent"
    );
    relay.await.unwrap();
}

#[tokio::test]
async fn data_start_554_is_a_permanent_failure_not_transient() {
    let (client, server) = tokio::io::duplex(65536);
    let relay = tokio::spawn(fake_relay(
        server,
        "220 relay.invalid\r\n",
        vec![
            ("EHLO ", "250 relay.invalid\r\n"),
            ("MAIL FROM:", "250 ok\r\n"),
            ("RCPT TO:", "250 ok\r\n"),
            ("DATA", "554 no valid recipients\r\n"),
        ],
    ));
    let outcome = send(
        client,
        &config(),
        Some("alice@example.invalid"),
        &["a@example.invalid".into()],
        b"body",
    )
    .await
    .unwrap();
    assert!(
        matches!(
            outcome.results[0],
            RecipientStatus::RemotePermanentFailure { .. }
        ),
        "got {:?}",
        outcome.results[0]
    );
    relay.await.unwrap();
}

#[tokio::test]
async fn connection_loss_after_full_data_write_is_ambiguous_not_sent_or_plain_transient() {
    let (client, server) = tokio::io::duplex(65536);
    let relay = tokio::spawn(async move {
        let (read_half, mut write_half) = tokio::io::split(server);
        let mut reader = BufReader::new(read_half);
        write_half
            .write_all(b"220 relay.invalid\r\n")
            .await
            .unwrap();
        for response in [
            "250 relay.invalid\r\n",
            "250 ok\r\n",
            "250 ok\r\n",
            "354 go\r\n",
        ] {
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            write_half.write_all(response.as_bytes()).await.unwrap();
        }
        // Read the full DATA payload up to the dot terminator, then vanish
        // without ever sending the final reply: the relay may or may not
        // have durably accepted the message.
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            if line == ".\r\n" {
                break;
            }
        }
        drop(write_half);
    });
    let outcome = send(
        client,
        &config(),
        Some("alice@example.invalid"),
        &["a@example.invalid".into()],
        b"body",
    )
    .await
    .unwrap();
    assert!(
        matches!(outcome.results[0], RecipientStatus::Ambiguous(_)),
        "got {:?}",
        outcome.results[0]
    );
    relay.await.unwrap();
}

#[tokio::test]
async fn a_relay_that_stops_reading_mid_data_does_not_hang_the_write_forever() {
    // A tiny duplex buffer plus a relay that never drains past DATA forces
    // the client's write_all(payload) to actually block on backpressure,
    // exercising the write-side deadline (not just the read-side one).
    let (client, server) = tokio::io::duplex(64);
    let mut config = config();
    config.command_timeout = Duration::from_millis(200);
    let relay = tokio::spawn(async move {
        let (read_half, mut write_half) = tokio::io::split(server);
        let mut reader = BufReader::new(read_half);
        write_half
            .write_all(b"220 relay.invalid\r\n")
            .await
            .unwrap();
        for response in [
            "250 relay.invalid\r\n",
            "250 ok\r\n",
            "250 ok\r\n",
            "354 go\r\n",
        ] {
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            write_half.write_all(response.as_bytes()).await.unwrap();
        }
        // Now go silent: never read the DATA payload, never reply again.
        tokio::time::sleep(Duration::from_secs(10)).await;
    });
    let large_body = vec![b'x'; 8192];
    let outcome = tokio::time::timeout(
        Duration::from_secs(3),
        send(
            client,
            &config,
            Some("alice@example.invalid"),
            &["a@example.invalid".into()],
            &large_body,
        ),
    )
    .await
    .expect("a stalled relay must not hang send() indefinitely")
    .unwrap();
    assert_ne!(outcome.results[0], RecipientStatus::Sent);
    relay.abort();
}

#[tokio::test]
async fn rcpt_221_is_never_treated_as_accepted_even_though_it_is_technically_2xx() {
    // 221 is QUIT's code, not RCPT's; a desynced transcript replying 221 to
    // RCPT must never be accepted just because it happens to be class-2xx.
    let (client, server) = tokio::io::duplex(65536);
    let relay = tokio::spawn(fake_relay(
        server,
        "220 relay.invalid\r\n",
        vec![
            ("EHLO ", "250 relay.invalid\r\n"),
            ("MAIL FROM:", "250 ok\r\n"),
            ("RCPT TO:", "221 bye\r\n"),
            ("RSET", "250 ok\r\n"),
        ],
    ));
    let outcome = tokio::time::timeout(
        Duration::from_secs(3),
        send(
            client,
            &config(),
            Some("alice@example.invalid"),
            &["a@example.invalid".into()],
            b"body",
        ),
    )
    .await
    .expect("must not hang")
    .unwrap();
    assert_ne!(
        outcome.results[0],
        RecipientStatus::Sent,
        "a malformed 221 reply to RCPT must never be treated as accepted"
    );
    relay.await.unwrap();
}

#[tokio::test]
async fn ehlo_251_is_not_a_valid_ehlo_success_code() {
    // 251 ("user not local; will forward") is only meaningful for RCPT; an
    // EHLO stage replying 251 is not a legitimate greeting-stage success and
    // must not be treated as one just because it is class-2xx.
    let (client, server) = tokio::io::duplex(65536);
    let mut config = config();
    config.command_timeout = Duration::from_millis(200);
    let relay = tokio::spawn(async move {
        let (read_half, mut write_half) = tokio::io::split(server);
        let mut reader = BufReader::new(read_half);
        write_half
            .write_all(b"220 relay.invalid\r\n")
            .await
            .unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert!(line.starts_with("EHLO "));
        write_half.write_all(b"251 forwarding\r\n").await.unwrap();
        // The client must not proceed to MAIL FROM after a non-250 EHLO reply.
        let mut rest = String::new();
        let _ = tokio::time::timeout(Duration::from_millis(150), reader.read_line(&mut rest)).await;
        rest
    });
    let outcome = tokio::time::timeout(
        Duration::from_secs(2),
        send(
            client,
            &config,
            Some("alice@example.invalid"),
            &["a@example.invalid".into()],
            b"body",
        ),
    )
    .await
    .expect("must not hang")
    .unwrap();
    assert_ne!(outcome.results[0], RecipientStatus::Sent);
    let rest = relay.await.unwrap();
    assert!(
        rest.is_empty(),
        "must not proceed past a non-250 EHLO reply, saw {rest:?}"
    );
}

#[tokio::test]
async fn a_write_failure_after_the_payload_but_before_the_terminator_lands_is_ambiguous_not_plain_transient()
 {
    let (client, server) = tokio::io::duplex(200);
    let mut config = config();
    config.command_timeout = Duration::from_millis(200);
    let relay = tokio::spawn(async move {
        let (read_half, mut write_half) = tokio::io::split(server);
        let mut reader = BufReader::new(read_half);
        write_half
            .write_all(b"220 relay.invalid\r\n")
            .await
            .unwrap();
        for response in [
            "250 relay.invalid\r\n",
            "250 ok\r\n",
            "250 ok\r\n",
            "354 go\r\n",
        ] {
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            write_half.write_all(response.as_bytes()).await.unwrap();
        }
        // Go silent: the 199-byte payload fits in the 200-byte duplex buffer
        // and lands fully, but the 2-byte ".\r\n" terminator has only 1 byte
        // of room left and stalls until the client's write deadline fires.
        tokio::time::sleep(Duration::from_secs(10)).await;
    });
    let mut body = vec![b'x'; 197];
    body.extend_from_slice(b"\r\n");
    let outcome = tokio::time::timeout(
        Duration::from_secs(3),
        send(
            client,
            &config,
            Some("alice@example.invalid"),
            &["a@example.invalid".into()],
            &body,
        ),
    )
    .await
    .expect("a stalled terminator write must not hang send() indefinitely")
    .unwrap();
    assert!(
        matches!(outcome.results[0], RecipientStatus::Ambiguous(_)),
        "a write failure once the payload may already be on the wire must be ambiguous, got {:?}",
        outcome.results[0]
    );
    relay.abort();
}

#[tokio::test]
async fn unreachable_greeting_never_panics_and_reports_transient() {
    let (client, server) = tokio::io::duplex(65536);
    drop(server);
    let result = send(
        client,
        &config(),
        Some("alice@example.invalid"),
        &["a@example.invalid".into()],
        b"body",
    )
    .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn an_eight_bit_body_declares_body_8bitmime_when_the_relay_offers_it() {
    let (client, server) = tokio::io::duplex(65536);
    let relay = tokio::spawn(fake_relay(
        server,
        "220 relay.invalid ESMTP\r\n",
        vec![
            (
                "EHLO ",
                "250-relay.invalid\r\n250-SIZE 10240000\r\n250 8BITMIME\r\n",
            ),
            (
                "MAIL FROM:<alice@example.invalid> BODY=8BITMIME",
                "250 ok\r\n",
            ),
            ("RCPT TO:<a@", "250 ok\r\n"),
            ("DATA", "354 go\r\n"),
            (".", "250 accepted\r\n"),
        ],
    ));
    let outcome = send(
        client,
        &config(),
        Some("alice@example.invalid"),
        &["a@example.invalid".into()],
        "Subject: tên\r\n\r\nchào bạn\r\n".as_bytes(),
    )
    .await
    .unwrap();
    assert_eq!(outcome.results, vec![RecipientStatus::Sent]);
    let seen = relay.await.unwrap();
    assert!(
        seen.iter()
            .any(|line| line.starts_with("MAIL FROM:<alice@example.invalid> BODY=8BITMIME")),
        "{seen:?}"
    );
}

#[tokio::test]
async fn a_seven_bit_body_never_declares_a_body_type() {
    let (client, server) = tokio::io::duplex(65536);
    let relay = tokio::spawn(fake_relay(
        server,
        "220 relay.invalid ESMTP\r\n",
        vec![
            ("EHLO ", "250-relay.invalid\r\n250 8BITMIME\r\n"),
            ("MAIL FROM:<alice@example.invalid>\r\n", "250 ok\r\n"),
            ("RCPT TO:<a@", "250 ok\r\n"),
            ("DATA", "354 go\r\n"),
            (".", "250 accepted\r\n"),
        ],
    ));
    let outcome = send(
        client,
        &config(),
        Some("alice@example.invalid"),
        &["a@example.invalid".into()],
        b"Subject: plain\r\n\r\nascii only\r\n",
    )
    .await
    .unwrap();
    assert_eq!(outcome.results, vec![RecipientStatus::Sent]);
    relay.await.unwrap();
}

#[tokio::test]
async fn a_relay_without_8bitmime_never_receives_an_eight_bit_message() {
    let (client, server) = tokio::io::duplex(65536);
    let relay = tokio::spawn(fake_relay(
        server,
        "220 relay.invalid ESMTP\r\n",
        vec![("EHLO ", "250-relay.invalid\r\n250 PIPELINING\r\n")],
    ));
    let outcome = send(
        client,
        &config(),
        Some("alice@example.invalid"),
        &["a@example.invalid".into()],
        "Subject: tên\r\n\r\nchào bạn\r\n".as_bytes(),
    )
    .await
    .unwrap();
    assert_eq!(
        outcome.results,
        vec![RecipientStatus::TransientFailure(
            "relay does not announce 8BITMIME for an 8-bit message".into()
        )]
    );
    let seen = relay.await.unwrap();
    assert!(
        !seen.iter().any(|line| line.starts_with("MAIL FROM:")),
        "the transaction never starts: {seen:?}"
    );
}
