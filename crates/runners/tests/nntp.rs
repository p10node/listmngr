//! Mailman's mail → news gateway on the runners: an accepted post on a
//! gatewayed list becomes an `nntp` job, and the `nntp` runner offers it to
//! the news server prepared as Mailman prepares it, answering a duplicate
//! `Message-ID` as Mailman does.
use listmngr_core::{Config, MemberRole, NntpConfig, SubscriptionMode};
use listmngr_db::{
    Database, NewList, NewMember,
    mail_queue::{JobState, NewMessage, Queue},
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

const LIST: &str = "dev.example.invalid";

/// A newsgroup on the fake server: articles by number, changeable while
/// the server runs.
type Articles = Arc<Mutex<Vec<(u64, Vec<u8>)>>>;

/// A news server that records what it is given and refuses once on request.
async fn news_server(refuse_first_with: Option<&'static str>) -> (u16, Arc<Mutex<Vec<Vec<u8>>>>) {
    let (port, posted, _) = news_server_with_group(refuse_first_with, "alt.test").await;
    (port, posted)
}

/// [`news_server`] that also serves `group`'s articles to a reader.
async fn news_server_with_group(
    refuse_first_with: Option<&'static str>,
    group: &'static str,
) -> (u16, Arc<Mutex<Vec<Vec<u8>>>>, Articles) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let posted: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
    let articles: Articles = Arc::default();
    let refusal = Arc::new(Mutex::new(refuse_first_with));
    let recorded = posted.clone();
    let served = articles.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let posted = recorded.clone();
            let refusal = refusal.clone();
            let articles = served.clone();
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
                    if command.eq_ignore_ascii_case("QUIT") {
                        let _ = write.write_all(b"205 bye\r\n").await;
                        return;
                    }
                    if let Some(name) = command.strip_prefix("GROUP ") {
                        let reply = if name == group {
                            let articles = articles.lock().unwrap();
                            let first = articles.first().map_or(0, |(n, _)| *n);
                            let last = articles.last().map_or(0, |(n, _)| *n);
                            format!("211 {} {first} {last} {name}\r\n", articles.len())
                        } else {
                            "411 No such newsgroup\r\n".to_owned()
                        };
                        write.write_all(reply.as_bytes()).await.unwrap();
                        continue;
                    }
                    if let Some(reply) = article_reply(command, &articles) {
                        write.write_all(reply.as_bytes()).await.unwrap();
                        continue;
                    }
                    if !command.eq_ignore_ascii_case("POST") {
                        write.write_all(b"200 ok\r\n").await.unwrap();
                        continue;
                    }
                    write.write_all(b"340 send it\r\n").await.unwrap();
                    let mut article = Vec::new();
                    loop {
                        let mut body_line = Vec::new();
                        read.read_until(b'\n', &mut body_line).await.unwrap();
                        if body_line == b".\r\n" {
                            break;
                        }
                        let start = usize::from(body_line.starts_with(b".."));
                        article.extend_from_slice(&body_line[start..]);
                    }
                    let reply = refusal.lock().unwrap().take().map_or_else(
                        || {
                            posted.lock().unwrap().push(article);
                            "240 posted\r\n".to_owned()
                        },
                        |code| format!("{code}\r\n"),
                    );
                    write.write_all(reply.as_bytes()).await.unwrap();
                }
            });
        }
    });
    (port, posted, articles)
}

/// `HEAD n` / `ARTICLE n` over the served articles, dot-stuffed.
fn article_reply(command: &str, articles: &Articles) -> Option<String> {
    let rest = command
        .strip_prefix("HEAD ")
        .or_else(|| command.strip_prefix("ARTICLE "))?;
    let whole = command.starts_with("ARTICLE ");
    let number: u64 = rest.trim().parse().unwrap_or(0);
    let found = articles
        .lock()
        .unwrap()
        .iter()
        .find(|(n, _)| *n == number)
        .cloned();
    Some(match found {
        Some((n, article)) => {
            let text = String::from_utf8_lossy(&article).into_owned();
            let sent = if whole {
                text
            } else {
                text.split("\r\n\r\n").next().unwrap_or("").to_owned() + "\r\n"
            };
            let code = if whole { 220 } else { 221 };
            let mut out = format!("{code} {n} <{n}@news.invalid>\r\n");
            for body_line in sent.split_inclusive("\r\n") {
                if body_line.starts_with('.') {
                    out.push('.');
                }
                out.push_str(body_line);
            }
            out.push_str(".\r\n");
            out
        }
        None => "423 No such article number\r\n".to_owned(),
    })
}

async fn fixture(settings: Value) -> Database {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: LIST.parse().unwrap(),
            display_name: "Dev".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let mut settings = settings;
    settings["respond_to_post_requests"] = json!(false);
    settings["admin_immed_notify"] = json!(false);
    settings["default_nonmember_action"] = json!("accept");
    db.lists().update(&list.id, &settings).await.unwrap();
    db.members()
        .create(NewMember {
            list_id: list.id,
            email: "reader@example.invalid".into(),
            display_name: String::new(),
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsAddress,
        })
        .await
        .unwrap();
    db
}

/// Admit `raw` through the real `in` runner and return the jobs it left.
async fn admit(db: &Database, raw: &[u8], context: Value) -> Vec<(String, String)> {
    let job = db
        .mail_queue()
        .enqueue(
            NewMessage {
                raw: raw.to_vec(),
                external_id: format!("<{}@example.invalid>", uuid::Uuid::now_v7()),
                context: context.to_string(),
                queue: Queue::In,
                max_attempts: 3,
            },
            chrono::Utc::now().timestamp_millis(),
        )
        .await
        .unwrap();
    let config = Config::default();
    let role = listmngr_runners::MailRoleConfig::from_core(&config).unwrap();
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let mut worker = tokio::spawn(listmngr_runners::run_in_processor(
        db.clone(),
        config,
        role,
        "nntp-fixture".into(),
        receiver,
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        while db.mail_queue().job(job.id).await.unwrap().state != JobState::Done {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("in runner did not finish the job");
    stop.send(true).unwrap();
    if tokio::time::timeout(Duration::from_secs(2), &mut worker)
        .await
        .is_err()
    {
        worker.abort();
    }
    sqlx::query_as("SELECT queue, state FROM queue_jobs ORDER BY queue")
        .fetch_all(db.pool())
        .await
        .unwrap()
}

/// Run the `nntp` runner until the one `nntp` job leaves `ready`/`leased`
/// for good, or five seconds pass.
async fn gate(db: &Database, config: NntpConfig) -> (String, String) {
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let mut worker = tokio::spawn(listmngr_runners::nntp::run(db.clone(), config, receiver));
    let settled = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let (state, error): (String, String) =
                sqlx::query_as("SELECT state, last_error FROM queue_jobs WHERE queue='nntp'")
                    .fetch_one(db.pool())
                    .await
                    .unwrap();
            if state == "done" || state == "shunted" {
                return (state, error);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    stop.send(true).unwrap();
    if tokio::time::timeout(Duration::from_secs(2), &mut worker)
        .await
        .is_err()
    {
        worker.abort();
    }
    settled.expect("the nntp job settled")
}

fn post() -> Vec<u8> {
    format!(
        "From: Alice <alice@sender.invalid>\r\nTo: dev@example.invalid\r\nSubject: Release notes\r\nDate: Mon, 1 Sep 2026 10:00:00 +0000\r\nMessage-ID: <{}@sender.invalid>\r\nX-Trace: keep-out\r\n\r\nhello news\r\n",
        uuid::Uuid::now_v7()
    )
    .into_bytes()
}

fn header(raw: &[u8], name: &str) -> Option<String> {
    listmngr_mail::header_value(raw, name)
}

#[tokio::test]
async fn a_gatewayed_post_is_queued_and_offered_to_the_news_server_prepared() {
    let db = fixture(json!({
        "gateway_to_news": true,
        "linked_newsgroup": "comp.lang.rust.lists",
        "newsgroup_moderation": "open_moderated",
        "nntp_prefix_subject_too": false
    }))
    .await;
    let jobs = admit(
        &db,
        &post(),
        json!({"list_id": LIST, "envelope_sender": "alice@sender.invalid"}),
    )
    .await;
    assert!(
        jobs.iter()
            .any(|(queue, state)| queue == "nntp" && state == "ready"),
        "the to-usenet handler queued the post: {jobs:?}"
    );
    let (port, posted) = news_server(None).await;
    let (state, _) = gate(
        &db,
        NntpConfig {
            host: "127.0.0.1".into(),
            port,
            ..NntpConfig::default()
        },
    )
    .await;
    assert_eq!(state, "done");
    let posted = posted.lock().unwrap().clone();
    assert_eq!(posted.len(), 1);
    let article = &posted[0];
    // Mailman's preparation on the list's cooked copy: the newsgroup, the
    // Approved header, the subject without the list's prefix, the list
    // headers of the copy, the transport header gone, a line count.
    assert_eq!(
        header(article, "Newsgroups").as_deref(),
        Some("comp.lang.rust.lists"),
        "{}",
        String::from_utf8_lossy(article)
    );
    assert_eq!(
        header(article, "Approved").as_deref(),
        Some("dev@example.invalid")
    );
    assert_eq!(header(article, "Subject").as_deref(), Some("Release notes"));
    assert_eq!(
        header(article, "List-Id").as_deref(),
        Some("<dev.example.invalid>")
    );
    assert!(header(article, "X-Trace").is_none());
    assert_eq!(header(article, "Lines").as_deref(), Some("1"));
    assert!(article.ends_with(b"\r\nhello news\r\n"));
    // A list that stopped gatewaying in the meantime: the job is done
    // without a post.
    let db = fixture(json!({"gateway_to_news": true, "linked_newsgroup": "alt.test"})).await;
    admit(
        &db,
        &post(),
        json!({"list_id": LIST, "envelope_sender": "alice@sender.invalid"}),
    )
    .await;
    db.lists()
        .update(&LIST.parse().unwrap(), &json!({"gateway_to_news": false}))
        .await
        .unwrap();
    let (port, posted) = news_server(None).await;
    let (state, _) = gate(
        &db,
        NntpConfig {
            host: "127.0.0.1".into(),
            port,
            ..NntpConfig::default()
        },
    )
    .await;
    assert_eq!(state, "done");
    assert!(posted.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_post_from_the_newsgroup_and_an_ungatewayed_list_queue_nothing() {
    let db = fixture(json!({"gateway_to_news": true, "linked_newsgroup": "alt.test"})).await;
    let jobs = admit(
        &db,
        &post(),
        json!({"list_id": LIST, "envelope_sender": "alice@sender.invalid", "fromusenet": true}),
    )
    .await;
    assert!(
        !jobs.iter().any(|(queue, _)| queue == "nntp"),
        "gated in from the newsgroup, not gated back: {jobs:?}"
    );
    let db = fixture(json!({})).await;
    let jobs = admit(
        &db,
        &post(),
        json!({"list_id": LIST, "envelope_sender": "alice@sender.invalid"}),
    )
    .await;
    assert!(!jobs.iter().any(|(queue, _)| queue == "nntp"), "{jobs:?}");
    assert!(jobs.iter().any(|(queue, _)| queue == "out"), "{jobs:?}");
}

#[tokio::test]
async fn a_refused_message_id_is_replaced_once_and_a_dead_server_waits() {
    let db = fixture(json!({"gateway_to_news": true, "linked_newsgroup": "alt.test"})).await;
    let raw = post();
    let original_id = header(&raw, "Message-ID").unwrap();
    admit(
        &db,
        &raw,
        json!({"list_id": LIST, "envelope_sender": "alice@sender.invalid"}),
    )
    .await;
    let (port, posted) = news_server(Some("441 435 Duplicate")).await;
    let (state, _) = gate(
        &db,
        NntpConfig {
            host: "127.0.0.1".into(),
            port,
            ..NntpConfig::default()
        },
    )
    .await;
    assert_eq!(state, "done");
    let posted = posted.lock().unwrap().clone();
    assert_eq!(posted.len(), 1, "the second offer was taken");
    let replaced = header(&posted[0], "Message-ID").unwrap();
    assert_ne!(replaced, original_id);
    assert!(
        replaced.ends_with(".dev@example.invalid>"),
        "a Message-ID of the list's own: {replaced}"
    );
    let stored: String = sqlx::query_scalar("SELECT context FROM messages")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let stored: Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(stored["nntp_message_id"], replaced);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT attempts FROM queue_jobs WHERE queue='nntp'")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        2
    );
    // The stored post is untouched; only the article carried the new id.
    let blob: Vec<u8> = sqlx::query_scalar("SELECT raw FROM message_blobs")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(blob, raw);
    // Nothing listening: the job waits for another try, nothing is lost.
    let db = fixture(json!({"gateway_to_news": true, "linked_newsgroup": "alt.test"})).await;
    admit(
        &db,
        &post(),
        json!({"list_id": LIST, "envelope_sender": "alice@sender.invalid"}),
    )
    .await;
    let free = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead = free.local_addr().unwrap().port();
    drop(free);
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(listmngr_runners::nntp::run(
        db.clone(),
        NntpConfig {
            host: "127.0.0.1".into(),
            port: dead,
            ..NntpConfig::default()
        },
        receiver,
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let attempts: i64 =
                sqlx::query_scalar("SELECT attempts FROM queue_jobs WHERE queue='nntp'")
                    .fetch_one(db.pool())
                    .await
                    .unwrap();
            if attempts >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    stop.send(true).unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(2), worker).await;
    let (state, run_after, error): (String, i64, String) =
        sqlx::query_as("SELECT state, run_after, last_error FROM queue_jobs WHERE queue='nntp'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(state, "ready", "{error}");
    assert!(
        run_after > chrono::Utc::now().timestamp_millis() + 30_000,
        "backed off: {run_after}"
    );
    assert!(!error.is_empty());
}

fn article(number: u64, from: &str, subject: &str, list_id: Option<&str>) -> (u64, Vec<u8>) {
    let list_header = list_id.map_or(String::new(), |id| format!("List-Id: <{id}>\r\n"));
    (
        number,
        format!(
            "Path: news.invalid!not-for-mail\r\nFrom: {from}\r\nNewsgroups: comp.lang.rust.lists\r\nSubject: {subject}\r\nDate: Tue, 2 Sep 2026 09:00:00 +0000\r\nMessage-ID: <{number}@news.invalid>\r\nTo: readers@elsewhere.invalid\r\n{list_header}\r\narticle {number}\r\n"
        )
        .into_bytes(),
    )
}

async fn watermark(db: &Database) -> Value {
    serde_json::to_value(db.lists().get(&LIST.parse().unwrap()).await.unwrap()).unwrap()
        ["usenet_watermark"]
        .clone()
}

/// Mailman's `gatenews`: the first poll of a list only catches up to the
/// group's last article; the next gates what arrived since — except the
/// list's own posts, recognised by their `List-Id` — as `in` jobs marked
/// `fromusenet`, and moves the watermark article by article.
#[tokio::test]
async fn gatenews_catches_up_then_gates_new_articles_and_moves_the_watermark() {
    let db =
        fixture(json!({"gateway_to_mail": true, "linked_newsgroup": "comp.lang.rust.lists"})).await;
    let (port, _, articles) = news_server_with_group(None, "comp.lang.rust.lists").await;
    let config = NntpConfig {
        host: "127.0.0.1".into(),
        port,
        ..NntpConfig::default()
    };
    *articles.lock().unwrap() = vec![
        article(40, "old@elsewhere.invalid", "before the gateway", None),
        article(41, "old@elsewhere.invalid", "also before", None),
    ];
    // First poll: catch up, gate nothing.
    let report = listmngr_runners::nntp::gate_news(&db, &config)
        .await
        .unwrap();
    assert_eq!(report.len(), 1, "{report:?}");
    assert_eq!(report[0].list_id, LIST);
    assert_eq!(report[0].gated, 0);
    assert_eq!(report[0].watermark, Some(41));
    assert_eq!(watermark(&db).await, 41);
    let jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(jobs, 0);
    // Nothing new: nothing gated, the watermark stays.
    let report = listmngr_runners::nntp::gate_news(&db, &config)
        .await
        .unwrap();
    assert_eq!(report[0].gated, 0);
    assert_eq!(watermark(&db).await, 41);
    // Three new articles: one is the list's own post come back, one is a
    // newsgroup post, one has no From.
    articles.lock().unwrap().extend([
        article(42, "alice@sender.invalid", "[dev] Gated out", Some(LIST)),
        article(43, "Carol <carol@elsewhere.invalid>", "From the newsgroup", None),
        (
            44,
            b"Path: news.invalid\r\nNewsgroups: comp.lang.rust.lists\r\nSubject: no author\r\n\r\nbody\r\n".to_vec(),
        ),
    ]);
    let report = listmngr_runners::nntp::gate_news(&db, &config)
        .await
        .unwrap();
    assert_eq!(report[0].gated, 1, "{report:?}");
    assert_eq!(report[0].watermark, Some(44));
    assert_eq!(watermark(&db).await, 44);
    let queued: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT q.queue, m.context, m.external_id FROM queue_jobs q JOIN messages m ON m.id=q.message_id",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(queued.len(), 1, "{queued:?}");
    let (queue, context, external_id) = &queued[0];
    assert_eq!(queue, "in");
    assert_eq!(external_id, "<43@news.invalid>");
    let context: Value = serde_json::from_str(context).unwrap();
    assert_eq!(context["list_id"], LIST);
    assert_eq!(context["fromusenet"], true);
    assert_eq!(context["envelope_sender"], "carol@elsewhere.invalid");
    let raw: Vec<u8> = sqlx::query_scalar("SELECT raw FROM message_blobs")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(header(&raw, "To").as_deref(), Some("dev@example.invalid"));
    assert_eq!(
        header(&raw, "X-Originally-To").as_deref(),
        Some("readers@elsewhere.invalid")
    );
    // The audit trail names each article the watermark passed.
    let audited: Vec<String> = sqlx::query_scalar(
        "SELECT diff FROM audit_log WHERE action='usenet.watermark' ORDER BY at",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(audited.len(), 4, "{audited:?}");
    admitted_to_members_not_gated_back(&db).await;
    unknown_group_and_no_gateway(&db, &config).await;
}

/// A group the server does not know is reported and moves nothing; a list
/// that does not gateway from news is not polled at all.
async fn unknown_group_and_no_gateway(db: &Database, config: &NntpConfig) {
    db.lists()
        .update(
            &LIST.parse().unwrap(),
            &json!({"linked_newsgroup": "alt.missing"}),
        )
        .await
        .unwrap();
    let report = listmngr_runners::nntp::gate_news(db, config).await.unwrap();
    assert_eq!(report.len(), 1);
    assert!(report[0].error.is_some(), "{report:?}");
    assert_eq!(
        watermark(db).await,
        44,
        "an unreachable group moves nothing"
    );
    db.lists()
        .update(&LIST.parse().unwrap(), &json!({"gateway_to_mail": false}))
        .await
        .unwrap();
    assert!(
        listmngr_runners::nntp::gate_news(db, config)
            .await
            .unwrap()
            .is_empty()
    );
}

/// The in runner admits the gated article to the list's members and does
/// not gate it back out.
async fn admitted_to_members_not_gated_back(db: &Database) {
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let role = listmngr_runners::MailRoleConfig::from_core(&Config::default()).unwrap();
    let worker = tokio::spawn(listmngr_runners::run_in_processor(
        db.clone(),
        Config::default(),
        role,
        "gate-fixture".into(),
        receiver,
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let out: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs WHERE queue='out'")
                .fetch_one(db.pool())
                .await
                .unwrap();
            if out == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    stop.send(true).unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(2), worker).await;
    let nntp_jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs WHERE queue='nntp'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(nntp_jobs, 0, "not gated back to the newsgroup");
}

/// The `nntp` runner polls on its own, first at once, then every
/// `gatenews_every_secs`; zero leaves polling to the command.
#[tokio::test]
async fn the_nntp_runner_polls_the_newsgroups_on_its_schedule() {
    let db =
        fixture(json!({"gateway_to_mail": true, "linked_newsgroup": "comp.lang.rust.lists"})).await;
    let (port, _, articles) = news_server_with_group(None, "comp.lang.rust.lists").await;
    *articles.lock().unwrap() = vec![article(7, "old@elsewhere.invalid", "old", None)];
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(listmngr_runners::nntp::run(
        db.clone(),
        NntpConfig {
            host: "127.0.0.1".into(),
            port,
            gatenews_every_secs: 3600,
            ..NntpConfig::default()
        },
        receiver,
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        while watermark(&db).await != 7 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the runner caught up at start");
    stop.send(true).unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(2), worker).await;
    // With polling off the runner leaves the watermark alone.
    let db =
        fixture(json!({"gateway_to_mail": true, "linked_newsgroup": "comp.lang.rust.lists"})).await;
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(listmngr_runners::nntp::run(
        db.clone(),
        NntpConfig {
            host: "127.0.0.1".into(),
            port,
            gatenews_every_secs: 0,
            ..NntpConfig::default()
        },
        receiver,
    ));
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(watermark(&db).await, Value::Null);
    stop.send(true).unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(2), worker).await;
}
