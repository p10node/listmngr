//! The Mailman 3 REST client: Basic authentication, the collection
//! envelope and its pagination, and the errors a wrong password or a
//! stopped core gives — against an in-process server, and against a real
//! core when one is named.
use listmngr_import::import3::fetch;
use listmngr_import::rest3::Rest;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A server that answers Mailman's shapes: `/3.1/domains` in two pages,
/// everything else an empty collection, and `401` without the expected
/// credentials. Returns its port and the paths it was asked for.
async fn core(credentials: Option<&'static str>) -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&requests);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let counter = Arc::clone(&counter);
            tokio::spawn(async move {
                let mut buffer = vec![0; 4096];
                let read = stream.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
                counter.fetch_add(1, Ordering::SeqCst);
                let line = request.lines().next().unwrap_or_default().to_owned();
                let target = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let authorized = credentials.is_none_or(|expected| {
                    // hyper writes header names in lower case.
                    let wanted = format!("authorization: basic {}", expected.to_lowercase());
                    request
                        .lines()
                        .any(|header| header.trim().to_lowercase() == wanted)
                });
                let body = if !authorized {
                    None
                } else if target.starts_with("/3.1/domains") {
                    // Mailman pages with `count` and `page`.
                    let page = target
                        .split_once("page=")
                        .and_then(|(_, rest)| rest.split('&').next())
                        .unwrap_or("1")
                        .parse::<usize>()
                        .unwrap_or(1);
                    let host = if page == 1 {
                        "one.invalid"
                    } else {
                        "two.invalid"
                    };
                    Some(format!(
                        r#"{{"entries": [{{"mail_host": "{host}", "description": "", "alias_domain": null, "http_etag": "\"x\""}}], "http_etag": "\"y\"", "start": {}, "total_size": 2}}"#,
                        page - 1
                    ))
                } else {
                    Some(r#"{"http_etag": "\"z\"", "start": 0, "total_size": 0}"#.to_owned())
                };
                let response = body.map_or_else(
                    || {
                        "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_owned()
                    },
                    |body| {
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                    },
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
            });
        }
    });
    (port, requests)
}

#[tokio::test]
async fn a_collection_is_read_page_by_page_under_basic_authentication() {
    // restadmin:restpass
    let (port, requests) = core(Some("cmVzdGFkbWluOnJlc3RwYXNz")).await;
    let rest = Rest::new(
        &format!("http://127.0.0.1:{port}/3.1"),
        "restadmin",
        "restpass",
    )
    .unwrap();
    let first = rest.get("domains").await.unwrap();
    assert_eq!(first["total_size"], 2);
    let site = fetch(&rest, None).await.unwrap();
    assert_eq!(
        site.domains
            .iter()
            .map(|domain| domain.mail_host.as_str())
            .collect::<Vec<_>>(),
        ["one.invalid", "two.invalid"],
        "both pages"
    );
    assert!(site.lists.is_empty());
    assert!(requests.load(Ordering::SeqCst) >= 3);
}

#[tokio::test]
async fn a_wrong_password_and_a_stopped_core_are_errors_that_never_name_the_password() {
    let (port, _) = core(Some("cmVzdGFkbWluOnJlc3RwYXNz")).await;
    let rest = Rest::new(
        &format!("http://127.0.0.1:{port}/3.1"),
        "restadmin",
        "hunter2hunter2",
    )
    .unwrap();
    let error = rest.get("domains").await.unwrap_err().to_string();
    assert!(error.contains("401"), "{error}");
    assert!(!error.contains("hunter2hunter2"), "{error}");
    // Nothing listens on port 1: an error, and still no password in it.
    let rest = Rest::new("http://127.0.0.1:1/3.1", "restadmin", "hunter2hunter2").unwrap();
    let error = rest.get("domains").await.unwrap_err().to_string();
    assert!(!error.contains("hunter2hunter2"), "{error}");
}

/// A real Mailman 3 core, when one is running: the same snapshot the
/// recorded fixtures stand for.
#[tokio::test]
#[ignore = "requires MAILMAN3_REST_URL (a disposable Mailman 3 core), MAILMAN3_REST_USER and MAILMAN3_REST_PASSWORD"]
async fn import3_reads_a_real_mailman3_core() {
    let url = std::env::var("MAILMAN3_REST_URL").expect("MAILMAN3_REST_URL");
    let user = std::env::var("MAILMAN3_REST_USER").unwrap_or_else(|_| "restadmin".into());
    let password = std::env::var("MAILMAN3_REST_PASSWORD").expect("MAILMAN3_REST_PASSWORD");
    let rest = Rest::new(&url, &user, &password).unwrap();
    let site = fetch(&rest, None).await.unwrap();
    let plan = listmngr_import::import3::plan(&site);
    println!(
        "{} domains, {} lists, {} members, {} warnings",
        site.domains.len(),
        site.lists.len(),
        site.lists
            .iter()
            .map(|list| list.members.len())
            .sum::<usize>(),
        plan.warnings.len(),
    );
    for warning in &plan.warnings {
        println!("warning: {warning}");
    }
    assert_eq!(site.users.len(), 8, "every Mailman account");
    assert!(
        site.users
            .iter()
            .any(|user| user.is_server_owner && user.has_password)
    );
    let list = site
        .lists
        .iter()
        .find(|list| list.list_id.as_str() == "rust-users.example.invalid")
        .expect("the populated list");
    assert_eq!(list.display_name, "Rust-Users");
    assert_eq!(list.members.len(), 7);
    let planned = plan
        .lists
        .iter()
        .find(|entry| entry.list_id == list.list_id)
        .unwrap();
    assert_eq!(planned.settings["subject_prefix"], "[Rust] ");
    assert_eq!(planned.settings["bounce_info_stale_after"], 7);
}
