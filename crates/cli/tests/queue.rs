use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;

struct Fixture {
    dir: tempfile::TempDir,
    url: String,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("queue.db").display()
        );
        let fixture = Self { dir, url };
        fixture.command(&["migrate"]).assert().success();
        fixture
            .command(&["domains", "add", "example.invalid"])
            .assert()
            .success();
        fixture
            .command(&[
                "lists",
                "create",
                "dev.example.invalid",
                "--display-name",
                "Dev",
            ])
            .assert()
            .success();
        fixture
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::cargo_bin("listmngr").unwrap();
        cmd.env_clear()
            .current_dir(self.dir.path())
            .env("LISTMNGR__DATABASE__URL", &self.url)
            .args(args);
        cmd
    }

    fn json(&self, args: &[&str]) -> Value {
        let output = self
            .command(args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice(&output).unwrap()
    }
}

#[cfg(unix)]
#[test]
fn queue_injection_rejects_special_files_without_waiting_for_a_writer() {
    let fixture = Fixture::new();
    let path = fixture.dir.path().join("private-input-sentinel");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    fixture
        .command(&[
            "queue",
            "inject",
            "dev.example.invalid",
            path.to_str().unwrap(),
            "--sender",
            "alice@example.invalid",
        ])
        .timeout(std::time::Duration::from_secs(3))
        .assert()
        .code(2)
        .stderr(predicate::str::contains("private-input-sentinel").not());
    assert_eq!(fixture.json(&["queue", "ls"]), serde_json::json!([]));
}

#[test]
fn queue_validation_rejects_bad_metadata_sender_list_and_size_without_writes() {
    let fixture = Fixture::new();
    let path = fixture.dir.path().join("private-content-sentinel.eml");
    let valid = b"Message-ID: <valid@example.invalid>\r\n\r\nprivate-content-sentinel";
    for (list, sender, raw, code) in [
        ("missing.example.invalid", "alice@example.invalid", valid.to_vec(), 7),
        ("dev.example.invalid", "private-content-sentinel", valid.to_vec(), 2),
        ("dev.example.invalid", "alice@example.invalid", b"Subject: private-content-sentinel\r\n\r\nbody".to_vec(), 2),
        ("dev.example.invalid", "alice@example.invalid", b"Message-ID: <a@example.invalid>\r\nMessage-ID: <b@example.invalid>\r\n\r\nprivate-content-sentinel".to_vec(), 2),
        ("dev.example.invalid", "alice@example.invalid", vec![b'x'; 10 * 1024 * 1024 + 1], 2),
    ] {
        std::fs::write(&path, raw).unwrap();
        fixture.command(&["queue", "inject", list, path.to_str().unwrap(), "--sender", sender])
            .assert().code(code)
            .stdout(predicate::str::contains("private-content-sentinel").not())
            .stderr(predicate::str::contains("private-content-sentinel").not());
        assert_eq!(fixture.json(&["queue", "ls"]), serde_json::json!([]));
    }
    fixture
        .command(&["queue", "show", "00000000-0000-0000-0000-000000000001"])
        .assert()
        .code(7);
    fixture
        .command(&["queue", "ls", "--queue", "private-content-sentinel"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("private-content-sentinel").not());
}

#[test]
fn queue_injection_persists_exact_bytes_across_cli_processes() {
    let fixture = Fixture::new();
    for (index, raw) in [
        b"Message-ID: <first@example.invalid>\r\nFrom: Alice <alice@example.invalid>\r\nSubject: First\r\n\r\nbody\0\xff\r\n".as_slice(),
        b"Message-ID: <second@example.invalid>\r\nFrom: bob@example.invalid\r\nSubject: Second\r\n\r\ndifferent body\r\n".as_slice(),
    ].into_iter().enumerate() {
        let path = fixture.dir.path().join(format!("message-{index}.eml"));
        std::fs::write(&path, raw).unwrap();
        let result = fixture.json(&["queue", "inject", "dev.example.invalid", path.to_str().unwrap(), "--sender", "alice@example.invalid"]);
        let id = result["id"].as_str().expect("job id");
        let job = fixture.json(&["queue", "show", id]);
        assert_eq!(job["queue"], "in");
        assert_eq!(job["attempts"], 0);
        assert!(job.get("raw").is_none(), "raw mail is opt-in only");
        fixture.command(&["queue", "show", id, "--raw"]).assert().success().stdout(raw);
    }
    let jobs = fixture.json(&["queue", "ls", "--queue", "in"]);
    assert_eq!(jobs.as_array().unwrap().len(), 2);
}

#[test]
fn queue_unshunt_replays_a_shunted_job_and_validates_the_target() {
    let fixture = Fixture::new();
    let path = fixture.dir.path().join("message.eml");
    std::fs::write(&path, b"Message-ID: <unshunt@example.invalid>\r\n\r\nbody").unwrap();
    let job = fixture.json(&[
        "queue",
        "inject",
        "dev.example.invalid",
        path.to_str().unwrap(),
        "--sender",
        "alice@example.invalid",
    ]);
    let id = job["id"].as_str().unwrap();
    // A ready (never-shunted) job cannot be unshunted.
    fixture
        .command(&["queue", "unshunt", id, "--target", "in"])
        .assert()
        .code(6);
    // The quarantine queue itself is not a valid replay target.
    fixture
        .command(&["queue", "unshunt", id, "--target", "shunt"])
        .assert()
        .code(2);
}

#[test]
fn queue_stats_reports_depth_per_queue_and_state_with_the_oldest_ready_age() {
    let fixture = Fixture::new();
    let empty = fixture.json(&["queue", "stats"]);
    assert_eq!(empty["queues"], serde_json::json!({}));
    assert_eq!(empty["shunted"], 0);
    assert!(empty["oldest_ready_age_secs"].is_null());
    let path = fixture.dir.path().join("private-content-sentinel.eml");
    std::fs::write(
        &path,
        b"Message-ID: <stats@example.invalid>\r\n\r\nprivate-content-sentinel",
    )
    .unwrap();
    for _ in 0..2 {
        fixture
            .command(&[
                "queue",
                "inject",
                "dev.example.invalid",
                path.to_str().unwrap(),
                "--sender",
                "alice@example.invalid",
            ])
            .assert()
            .success();
    }
    let stats = fixture.json(&["queue", "stats"]);
    assert_eq!(stats["queues"]["in"]["ready"], 2);
    assert_eq!(stats["shunted"], 0);
    let age = stats["oldest_ready_age_secs"].as_i64().unwrap();
    assert!((0..60).contains(&age), "{age}");
    let text = String::from_utf8(
        fixture
            .command(&["queue", "stats"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap();
    assert!(
        !text.contains("private-content-sentinel"),
        "stats never export message content"
    );
}
