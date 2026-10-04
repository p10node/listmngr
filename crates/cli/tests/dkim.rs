//! `listmngr dkim` on the real binary: `gen` writes a key readable by nobody
//! else and prints the record and never the key, refuses to overwrite,
//! `records` reads the configuration, and `dns` compares what a resolver
//! publishes with the keys, exit 12 unless every record is right.
#![cfg(unix)]
use assert_cmd::Command;
use hickory_resolver::proto::{
    op::Message,
    rr::{RData, Record, RecordType, rdata::TXT},
};
use serde_json::Value;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

type Records = Arc<Mutex<HashMap<String, String>>>;

fn listmngr(dir: &Path) -> Command {
    let mut command = Command::cargo_bin("listmngr").unwrap();
    for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("LISTMNGR")) {
        command.env_remove(key);
    }
    command.current_dir(dir);
    command
}

fn json_lines(output: &std::process::Output) -> Vec<Value> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

const fn gen_args<'a>(selector: &'a str, algorithm: &'a str, file: &'a str) -> [&'a str; 10] {
    [
        "dkim",
        "gen",
        "--domain",
        "lists.example.test",
        "--selector",
        selector,
        "--algorithm",
        algorithm,
        "--out",
        file,
    ]
}

/// `dkim gen`: a `0600` PKCS#8 file, the record on stdout, the key nowhere.
fn generate(dir: &Path, selector: &str, algorithm: &str, file: &str) -> Value {
    let output = listmngr(dir)
        .args(gen_args(selector, algorithm, file))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("PRIVATE"), "the key must never be printed");
    let mode = std::fs::metadata(dir.join(file))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
    let pem = std::fs::read_to_string(dir.join(file)).unwrap();
    assert!(pem.starts_with("-----BEGIN PRIVATE KEY-----\n"), "{pem}");
    json_lines(&output).remove(0)
}

/// Both keys generated, and a second `gen` to a file that exists refused.
fn generate_both(path: &Path) -> (Value, Value) {
    let ed = generate(path, "ed", "ed25519", "ed.pem");
    assert_eq!(ed["algorithm"], "ed25519");
    assert_eq!(ed["name"], "ed._domainkey.lists.example.test");
    assert!(
        ed["txt"]
            .as_str()
            .unwrap()
            .starts_with("v=DKIM1; k=ed25519; p="),
        "{ed}"
    );
    let rsa = generate(path, "rsa", "rsa", "rsa.pem");
    assert_eq!(rsa["algorithm"], "rsa");
    assert!(
        rsa["txt"]
            .as_str()
            .unwrap()
            .starts_with("v=DKIM1; k=rsa; p=")
    );
    let before = std::fs::read(path.join("ed.pem")).unwrap();
    let refused = listmngr(path)
        .args(gen_args("ed", "ed25519", "ed.pem"))
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("error[CLI-IO]"));
    assert_eq!(
        before,
        std::fs::read(path.join("ed.pem")).unwrap(),
        "the file is untouched"
    );
    (ed, rsa)
}

fn write_config(path: &Path) -> PathBuf {
    let config = path.join("listmngr.toml");
    std::fs::write(
        &config,
        format!(
            "[[mta.dkim_signing]]\ndomain = \"lists.example.test\"\nselector = \"ed\"\nprivate_key_file = \"{}\"\n\n[[mta.dkim_signing]]\ndomain = \"lists.example.test\"\nselector = \"rsa\"\nprivate_key_file = \"{}\"\n",
            path.join("ed.pem").display(),
            path.join("rsa.pem").display()
        ),
    )
    .unwrap();
    config
}

/// `dkim dns` against `server`: the exit code and one JSON line per key.
fn dns_check(dir: &Path, config: &Path, server: SocketAddr) -> (Option<i32>, Vec<Value>) {
    let output = listmngr(dir)
        .args([
            "--config",
            config.to_str().unwrap(),
            "dkim",
            "dns",
            "--dns-server",
            &server.to_string(),
        ])
        .output()
        .unwrap();
    (output.status.code(), json_lines(&output))
}

/// A DNS server answering TXT queries from `records`, in strings of at
/// most 255 bytes as DNS carries a long record, and nothing else.
async fn dns_server(records: Records) -> SocketAddr {
    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let address = socket.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buffer = [0; 4096];
        loop {
            let (size, peer) = socket.recv_from(&mut buffer).await.unwrap();
            let request = Message::from_vec(&buffer[..size]).unwrap();
            let query = request.queries[0].clone();
            let mut response = Message::response(request.id, request.op_code);
            response.metadata.recursion_desired = true;
            response.metadata.recursion_available = true;
            response.metadata.authoritative = true;
            response.add_query(query.clone());
            if query.query_type() == RecordType::TXT {
                let name = query.name().to_string();
                if let Some(value) = records.lock().unwrap().get(&name) {
                    let strings = value
                        .as_bytes()
                        .chunks(255)
                        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
                        .collect();
                    response.add_answer(Record::from_rdata(
                        query.name().clone(),
                        60,
                        RData::TXT(TXT::new(strings)),
                    ));
                }
            }
            socket
                .send_to(&response.to_vec().unwrap(), peer)
                .await
                .unwrap();
        }
    });
    address
}

/// The three states of `dkim dns`: the RSA record not yet published, then
/// published wrong, then right.
fn published_then_checked(
    path: &Path,
    config: &Path,
    server: SocketAddr,
    records: &Records,
    ed: &Value,
    rsa: &Value,
) {
    let listed = listmngr(path)
        .args(["--config", config.to_str().unwrap(), "dkim", "records"])
        .output()
        .unwrap();
    assert!(listed.status.success(), "{listed:?}");
    let lines = json_lines(&listed);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["txt"], ed["txt"]);
    assert_eq!(lines[1]["txt"], rsa["txt"]);
    let rsa_name = "rsa._domainkey.lists.example.test.".to_owned();
    let (code, statuses) = dns_check(path, config, server);
    assert_eq!(code, Some(12), "{statuses:?}");
    assert_eq!(statuses[0]["status"], "ok", "{statuses:?}");
    assert_eq!(statuses[1]["status"], "missing", "{statuses:?}");
    records
        .lock()
        .unwrap()
        .insert(rsa_name.clone(), ed["txt"].as_str().unwrap().to_owned());
    let (code, statuses) = dns_check(path, config, server);
    assert_eq!(code, Some(12));
    assert_eq!(statuses[1]["status"], "mismatch", "{statuses:?}");
    records
        .lock()
        .unwrap()
        .insert(rsa_name, rsa["txt"].as_str().unwrap().to_owned());
    let (code, statuses) = dns_check(path, config, server);
    assert_eq!(code, Some(0), "{statuses:?}");
    assert!(
        statuses.iter().all(|status| status["status"] == "ok"),
        "{statuses:?}"
    );
}

#[tokio::test]
async fn keys_are_generated_listed_and_checked_against_dns() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let (ed, rsa) = tokio::task::spawn_blocking({
        let path = path.clone();
        move || generate_both(&path)
    })
    .await
    .unwrap();
    let config = write_config(&path);
    let records: Records = Arc::new(Mutex::new(HashMap::from([(
        "ed._domainkey.lists.example.test.".to_owned(),
        ed["txt"].as_str().unwrap().to_owned(),
    )])));
    let server = dns_server(records.clone()).await;
    tokio::task::spawn_blocking(move || {
        published_then_checked(&path, &config, server, &records, &ed, &rsa);
    })
    .await
    .unwrap();
}
