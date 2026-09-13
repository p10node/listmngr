#![cfg(unix)]

use listmngr_core::DkimSigningConfig;
use listmngr_mail::dkim::SigningKeys;
use std::{
    process::Command,
    time::{Duration, Instant},
};

#[test]
fn fifo_key_is_rejected_without_waiting_for_a_writer() {
    const FIXTURE: &str = "LISTMNGR_TEST_OWNED_FIFO_KEY";
    if let Some(path) = std::env::var_os(FIXTURE) {
        let config = [DkimSigningConfig {
            domain: "fixture.invalid".into(),
            selector: "test".into(),
            private_key_file: path.into(),
        }];
        assert!(SigningKeys::load(&config).is_err());
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let fifo = dir.path().join("owned.fifo");
    assert!(
        Command::new("mkfifo")
            .args(["-m", "600"])
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "fifo_key_is_rejected_without_waiting_for_a_writer",
            "--nocapture",
        ])
        .env(FIXTURE, &fifo)
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "FIFO loader child failed");
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("DKIM key loader blocked opening a FIFO without a writer");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
