use assert_cmd::Command;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::time::{Duration, Instant};

fn status(address: SocketAddr) -> Command {
    let mut command = Command::cargo_bin("listmngr").unwrap();
    for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("LISTMNGR")) {
        command.env_remove(key);
    }
    command
        .arg("status")
        .env("LISTMNGR__WEB__LISTEN", address.to_string())
        .env("LISTMNGR__DATABASE__URL", "invalid-DATABASE-SENTINEL")
        .timeout(Duration::from_secs(8));
    command
}

fn fixture(
    bind: &str,
    responses: Vec<(&str, &str)>,
) -> (SocketAddr, std::thread::JoinHandle<Vec<String>>) {
    let responses = responses
        .into_iter()
        .map(|(code, headers)| (code.to_owned(), headers.to_owned()))
        .collect::<Vec<_>>();
    let listener = TcpListener::bind(bind).unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let handle = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(6);
        let mut paths = Vec::new();
        for (code, headers) in responses {
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            return paths;
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            loop {
                let mut byte = [0];
                if stream.read(&mut byte).unwrap() == 0 {
                    break;
                }
                request.push(byte[0]);
                if request.ends_with(b"\r\n\r\n") || request.len() >= 8192 {
                    break;
                }
            }
            paths.push(
                String::from_utf8(request)
                    .unwrap()
                    .lines()
                    .next()
                    .unwrap()
                    .to_owned(),
            );
            write!(
                stream,
                "HTTP/1.1 {code}\r\nContent-Length: 2\r\nConnection: close\r\n{headers}\r\n{{}}"
            )
            .unwrap();
        }
        paths
    });
    (address, handle)
}

#[test]
fn status_checks_health_then_readiness_without_opening_database() {
    let (address, server) = fixture("127.0.0.1:0", vec![("200 OK", ""), ("200 OK", "")]);
    let output = status(address).output().unwrap();
    let paths = server.join().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "service: healthy and ready\n"
    );
    assert_eq!(paths, ["GET /healthz HTTP/1.1", "GET /readyz HTTP/1.1"]);
}

#[test]
fn status_distinguishes_unreachable_unhealthy_and_not_ready() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    status(address)
        .assert()
        .code(3)
        .stderr(predicates::str::contains("error[CLI-STATUS-UNREACHABLE]"));
    for (responses, code, category) in [
        (vec![("503 Unavailable", "")], 4, "CLI-STATUS-UNHEALTHY"),
        (
            vec![("200 OK", ""), ("503 Unavailable", "")],
            5,
            "CLI-STATUS-NOT-READY",
        ),
    ] {
        let count = responses.len();
        let (address, server) = fixture("127.0.0.1:0", responses);
        let output = status(address).output().unwrap();
        assert_eq!(server.join().unwrap().len(), count);
        assert_eq!(output.status.code(), Some(code));
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains(category));
        assert!(!stderr.contains("DATABASE-SENTINEL"));
    }
}

#[test]
fn status_ignores_environment_proxies() {
    let (address, server) = fixture("127.0.0.1:0", vec![("200 OK", ""), ("200 OK", "")]);
    let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
    proxy.set_nonblocking(true).unwrap();
    let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
    let output = status(address)
        .env("HTTP_PROXY", &proxy_url)
        .env("http_proxy", &proxy_url)
        .env("ALL_PROXY", &proxy_url)
        .env("all_proxy", &proxy_url)
        .env("NO_PROXY", "")
        .env("no_proxy", "")
        .output()
        .unwrap();
    let paths = server.join().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(paths.len(), 2);
    assert_eq!(
        proxy.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn status_does_not_follow_redirects() {
    let target = TcpListener::bind("127.0.0.1:0").unwrap();
    target.set_nonblocking(true).unwrap();
    let location = format!(
        "Location: http://{}/healthz\r\n",
        target.local_addr().unwrap()
    );
    let (address, server) = fixture("127.0.0.1:0", vec![("302 Found", &location)]);
    let output = status(address).output().unwrap();
    assert_eq!(server.join().unwrap().len(), 1);
    assert_eq!(output.status.code(), Some(4));
    assert_eq!(
        target.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn status_probes_ipv4_and_ipv6_wildcards_over_loopback() {
    for (bind, wildcard) in [("127.0.0.1:0", "0.0.0.0"), ("[::1]:0", "[::]")] {
        let (address, server) = fixture(bind, vec![("200 OK", ""), ("200 OK", "")]);
        let output = status(address)
            .env(
                "LISTMNGR__WEB__LISTEN",
                format!("{wildcard}:{}", address.port()),
            )
            .output()
            .unwrap();
        assert_eq!(server.join().unwrap().len(), 2);
        assert!(output.status.success(), "{output:?}");
    }
}

#[test]
fn status_times_out_when_either_endpoint_never_responds() {
    for healthy_first in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = std::sync::mpsc::channel();
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(6);
            let mut requests = 0;
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(3)))
                            .unwrap();
                        stream
                            .set_write_timeout(Some(Duration::from_secs(3)))
                            .unwrap();
                        let mut data = [0; 4096];
                        assert!(stream.read(&mut data).unwrap() > 0);
                        requests += 1;
                        if healthy_first && requests == 1 {
                            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").unwrap();
                        } else {
                            // Hold the socket until the client exits. Without the
                            // production timeout, the CLI watchdog kills the child.
                            let _ = stopped.recv_timeout(Duration::from_secs(6));
                            return requests;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            return requests;
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                }
            }
        });
        let output = status(address)
            .timeout(Duration::from_secs(5))
            .output()
            .unwrap();
        let _ = stop.send(());
        assert_eq!(server.join().unwrap(), if healthy_first { 2 } else { 1 });
        assert_eq!(output.status.code(), Some(3));
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .contains("CLI-STATUS-UNREACHABLE")
        );
    }
}
