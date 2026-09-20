//! Mailman's `nntp` runner, the mail side: a post prepared for the news
//! server (`prepare_message`) and the NNTP client posting it.
use listmngr_core::{MailingList, NewsgroupModeration, NntpConfig};
use listmngr_mail::header_value;
use listmngr_mail::nntp::{Client, Outcome, prepare};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

fn list() -> MailingList {
    let mut list = MailingList::new("dev.example.invalid".parse().unwrap(), "Developers".into());
    list.usenet.gateway_to_news = true;
    list.usenet.linked_newsgroup = "comp.lang.rust.lists".into();
    list
}

const POST: &[u8] = b"Received: from mail.sender.invalid (mail.sender.invalid [192.0.2.25])\r\n\tby mx.example.invalid with ESMTPS id X\r\n\tfor <dev@example.invalid>; Mon, 1 Sep 2026 10:00:00 +0000\r\nFrom: Alice <alice@sender.invalid>\r\nTo: dev@example.invalid\r\nTo: other@example.invalid\r\nCc: bob@example.invalid\r\nSubject: [dev] Release notes\r\nDate: Mon, 1 Sep 2026 10:00:00 +0000\r\nMessage-ID: <notes@sender.invalid>\r\nMIME-Version: 1.0\r\nMIME-Version: 1.0\r\nX-Trace: something\r\nNNTP-Posting-Host: 192.0.2.25\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nline one\r\nline two\r\n\r\nline four\r\n";

#[test]
fn a_post_is_prepared_as_mailmans_nntp_runner_prepares_it() {
    let list = list();
    let config = NntpConfig::default();
    let article = prepare(POST, &list, &config).unwrap();
    // The newsgroup, a line count, the received-path headers gone, the
    // second To and MIME-Version moved aside, the prefix kept by default.
    assert_eq!(
        header_value(&article, "Newsgroups").as_deref(),
        Some("comp.lang.rust.lists")
    );
    assert_eq!(header_value(&article, "Lines").as_deref(), Some("4"));
    for gone in ["Received", "X-Trace", "NNTP-Posting-Host"] {
        assert!(header_value(&article, gone).is_none(), "{gone}");
    }
    let text = String::from_utf8_lossy(&article);
    assert_eq!(
        text.matches("\r\nTo: ").count() + usize::from(text.starts_with("To: ")),
        1
    );
    assert_eq!(
        header_value(&article, "X-Original-To").as_deref(),
        Some("other@example.invalid")
    );
    assert!(
        text.contains("\r\nX-Original-To: other@example.invalid\r\n"),
        "the target header as configured: {text}"
    );
    assert_eq!(text.matches("\r\nMIME-Version: ").count(), 1);
    assert_eq!(
        header_value(&article, "X-MIME-Version").as_deref(),
        Some("1.0")
    );
    assert_eq!(
        header_value(&article, "Cc").as_deref(),
        Some("bob@example.invalid")
    );
    assert_eq!(
        header_value(&article, "Subject").as_deref(),
        Some("[dev] Release notes")
    );
    assert!(header_value(&article, "Approved").is_none());
    assert_eq!(
        header_value(&article, "Message-ID").as_deref(),
        Some("<notes@sender.invalid>")
    );
    assert!(article.ends_with(b"\r\nline one\r\nline two\r\n\r\nline four\r\n"));
    // A moderated or open-moderated group gets Mailman's Approved header;
    // without the prefix option the list's prefix comes off the subject.
    let mut moderated = list.clone();
    moderated.usenet.newsgroup_moderation = NewsgroupModeration::OpenModerated;
    moderated.usenet.nntp_prefix_subject_too = false;
    let article = prepare(POST, &moderated, &config).unwrap();
    assert_eq!(
        header_value(&article, "Approved").as_deref(),
        Some("dev@example.invalid")
    );
    assert_eq!(
        header_value(&article, "Subject").as_deref(),
        Some("Release notes")
    );
    moderated.usenet.newsgroup_moderation = NewsgroupModeration::Moderated;
    let article = prepare(POST, &moderated, &config).unwrap();
    assert_eq!(
        header_value(&article, "Approved").as_deref(),
        Some("dev@example.invalid")
    );
    // A Newsgroups header the poster wrote is kept and the list's group
    // appended once; a Message-ID folded across lines is unfolded; a post
    // without one gets a Message-ID of the list's.
    let crossposted = b"From: a@sender.invalid\r\nNewsgroups: alt.test, comp.lang.rust.lists\r\nSubject: x\r\nMessage-ID: <folded\r\n @sender.invalid>\r\n\r\nbody\r\n";
    let article = prepare(crossposted, &list, &config).unwrap();
    assert_eq!(
        header_value(&article, "Newsgroups").as_deref(),
        Some("alt.test, comp.lang.rust.lists")
    );
    assert_eq!(
        header_value(&article, "Message-ID").as_deref(),
        Some("<folded@sender.invalid>")
    );
    let other = b"From: a@sender.invalid\r\nNewsgroups: alt.test\r\nSubject: x\r\n\r\nbody\r\n";
    let article = prepare(other, &list, &config).unwrap();
    assert_eq!(
        header_value(&article, "Newsgroups").as_deref(),
        Some("alt.test, comp.lang.rust.lists")
    );
    let id = header_value(&article, "Message-ID").unwrap();
    assert!(
        id.starts_with('<') && id.ends_with("dev@example.invalid>"),
        "{id}"
    );
}

#[derive(Default)]
struct Server {
    /// Articles the server accepted, dot-unstuffed.
    posted: Arc<Mutex<Vec<Vec<u8>>>>,
    /// `AUTHINFO USER`/`PASS` pairs it was given.
    logins: Arc<Mutex<Vec<(String, String)>>>,
    /// Reply to the article with this code once, then accept.
    refuse_first_with: Option<&'static str>,
    /// Require `AUTHINFO` before `POST`.
    auth: bool,
}

/// A small NNTP server: reader mode, optional AUTHINFO, POST, QUIT.
async fn serve(server: Server) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let refusal = Arc::new(Mutex::new(server.refuse_first_with));
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let posted = server.posted.clone();
            let logins = server.logins.clone();
            let refusal = refusal.clone();
            let auth = server.auth;
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut read = BufReader::new(read);
                write
                    .write_all(b"200 news.example.invalid InterNetNews ready\r\n")
                    .await
                    .unwrap();
                let mut authenticated = !auth;
                let mut user = String::new();
                loop {
                    let mut line = String::new();
                    if read.read_line(&mut line).await.unwrap() == 0 {
                        return;
                    }
                    let line = line.trim_end_matches(['\r', '\n']).to_owned();
                    let reply: String = if line.eq_ignore_ascii_case("MODE READER") {
                        "200 Reader mode, posting permitted\r\n".into()
                    } else if let Some(name) = line.strip_prefix("AUTHINFO USER ") {
                        name.clone_into(&mut user);
                        "381 Enter password\r\n".into()
                    } else if let Some(password) = line.strip_prefix("AUTHINFO PASS ") {
                        logins
                            .lock()
                            .unwrap()
                            .push((user.clone(), password.to_owned()));
                        if password == "s3cret" {
                            authenticated = true;
                            "281 Authentication accepted\r\n".into()
                        } else {
                            "481 Authentication failed\r\n".into()
                        }
                    } else if line.eq_ignore_ascii_case("POST") && !authenticated {
                        "480 Authentication required\r\n".into()
                    } else if line.eq_ignore_ascii_case("POST") {
                        write.write_all(b"340 Send article\r\n").await.unwrap();
                        let mut article = Vec::new();
                        loop {
                            let mut body_line = Vec::new();
                            read.read_until(b'\n', &mut body_line).await.unwrap();
                            if body_line == b".\r\n" {
                                break;
                            }
                            // `..x` on the wire came from `.x`.
                            let start = usize::from(body_line.starts_with(b".."));
                            article.extend_from_slice(&body_line[start..]);
                        }
                        let refused = refusal.lock().unwrap().take();
                        refused.map_or_else(
                            || {
                                posted.lock().unwrap().push(article);
                                "240 Article received OK\r\n".into()
                            },
                            |code| format!("{code}\r\n"),
                        )
                    } else if line.eq_ignore_ascii_case("QUIT") {
                        write.write_all(b"205 Bye\r\n").await.unwrap();
                        return;
                    } else {
                        "500 Unknown command\r\n".into()
                    };
                    write.write_all(reply.as_bytes()).await.unwrap();
                }
            });
        }
    });
    port
}

fn config(port: u16) -> NntpConfig {
    NntpConfig {
        host: "127.0.0.1".into(),
        port,
        ..NntpConfig::default()
    }
}

#[tokio::test]
async fn the_client_posts_an_article_in_reader_mode_with_dot_stuffing() {
    let posted = Arc::new(Mutex::new(Vec::new()));
    let port = serve(Server {
        posted: posted.clone(),
        ..Server::default()
    })
    .await;
    let article = b"From: a@sender.invalid\r\nNewsgroups: alt.test\r\nSubject: dots\r\nMessage-ID: <dots@sender.invalid>\r\n\r\n.starts with a dot\r\n..two dots\r\nplain\r\n";
    let outcome = Client::new(&config(port)).post(article).await.unwrap();
    assert_eq!(outcome, Outcome::Accepted);
    let received = posted.lock().unwrap().clone();
    assert_eq!(received.len(), 1);
    assert_eq!(
        received[0],
        article,
        "{}",
        String::from_utf8_lossy(&received[0])
    );
    // LF-only bytes go out as CRLF and a missing final line break is added.
    let bare = b"From: a@sender.invalid\r\nSubject: bare\r\n\r\nno final newline";
    assert_eq!(
        Client::new(&config(port)).post(bare).await.unwrap(),
        Outcome::Accepted
    );
    let received = posted.lock().unwrap().clone();
    assert!(received[1].ends_with(b"no final newline\r\n"));
}

#[tokio::test]
async fn the_client_authenticates_when_credentials_are_configured() {
    let logins = Arc::new(Mutex::new(Vec::new()));
    let posted = Arc::new(Mutex::new(Vec::new()));
    let port = serve(Server {
        posted: posted.clone(),
        logins: logins.clone(),
        auth: true,
        ..Server::default()
    })
    .await;
    let article = b"From: a@sender.invalid\r\nSubject: auth\r\n\r\nbody\r\n";
    // Without credentials the server wants authentication: a refusal the
    // caller sees as such, not a crash.
    assert_eq!(
        Client::new(&config(port)).post(article).await.unwrap(),
        Outcome::Refused("480 Authentication required".into())
    );
    let mut with = config(port);
    with.user = Some("gateway".into());
    with.password = Some("s3cret".into());
    assert_eq!(
        Client::new(&with).post(article).await.unwrap(),
        Outcome::Accepted
    );
    assert_eq!(
        logins.lock().unwrap().clone(),
        vec![("gateway".to_owned(), "s3cret".to_owned())]
    );
    let mut wrong = with.clone();
    wrong.password = Some("nope".into());
    let error = Client::new(&wrong).post(article).await.unwrap_err();
    assert!(
        !error.to_string().contains("nope"),
        "a password never appears in an error: {error}"
    );
    assert_eq!(posted.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_refused_article_and_a_dead_server_are_told_apart() {
    let posted = Arc::new(Mutex::new(Vec::new()));
    let port = serve(Server {
        posted: posted.clone(),
        refuse_first_with: Some("441 435 Duplicate"),
        ..Server::default()
    })
    .await;
    let article = b"From: a@sender.invalid\r\nSubject: dup\r\nMessage-ID: <dup@sender.invalid>\r\n\r\nbody\r\n";
    assert_eq!(
        Client::new(&config(port)).post(article).await.unwrap(),
        Outcome::Refused("441 435 Duplicate".into())
    );
    assert_eq!(
        Client::new(&config(port)).post(article).await.unwrap(),
        Outcome::Accepted
    );
    // Nothing listens: an error, for the runner to retry.
    let free = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead = free.local_addr().unwrap().port();
    drop(free);
    assert!(Client::new(&config(dead)).post(article).await.is_err());
}
