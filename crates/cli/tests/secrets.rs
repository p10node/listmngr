//! `listmngr secrets` and the doctor's `master_key` check on the real
//! binary: a site without a key is warned, a sealed row without a key is a
//! failure, `encrypt` seals the rows in the clear, `rewrap` moves them to a
//! new key, and `new-key` prints a usable key and nothing else.
use assert_cmd::Command;
use listmngr_db::{Database, NewUser, web_sessions::LoginOutcome};
use serde_json::Value;

const PASSWORD: &str = "a very secure fixture password";
const KEY_A: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn listmngr(url: &str, key: Option<&str>) -> Command {
    let mut command = Command::cargo_bin("listmngr").unwrap();
    for (name, _) in std::env::vars().filter(|(name, _)| name.starts_with("LISTMNGR")) {
        command.env_remove(name);
    }
    command.env("LISTMNGR__DATABASE__URL", url);
    if let Some(key) = key {
        command.env("LISTMNGR__SECURITY__MASTER_KEY", key);
    }
    command
}

fn json(output: &std::process::Output) -> Value {
    serde_json::from_str(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap()
}

fn doctor_check(url: &str, key: Option<&str>, id: &str) -> (Option<i32>, String, String) {
    let output = listmngr(url, key).arg("doctor").output().unwrap();
    let report = json(&output);
    let check = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["id"] == id)
        .unwrap_or_else(|| panic!("no {id} check: {report}"))
        .clone();
    (
        output.status.code(),
        check["status"].as_str().unwrap().to_owned(),
        check["detail"].as_str().unwrap().to_owned(),
    )
}

/// A user with a confirmed second factor, stored as a site without a key
/// stores it.
async fn enrolled_in_the_clear(url: &str) {
    let db = Database::connect(url, 1).await.unwrap();
    db.migrate().await.unwrap();
    // No mail domain: the doctor's DNS check has nothing to resolve.
    db.users()
        .create(NewUser {
            email: "alice@example.invalid".into(),
            display_name: "Alice".into(),
            password: PASSWORD.into(),
            server_owner: false,
        })
        .await
        .unwrap();
    db.addresses()
        .verify("alice@example.invalid", true)
        .await
        .unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    let anon = db.create_web_session(None, None, now).await.unwrap();
    let LoginOutcome::Complete(session) = db
        .browser_login("alice@example.invalid", PASSWORD, &anon)
        .await
        .unwrap()
    else {
        panic!("nothing enrolled yet")
    };
    let status = db.browser_totp_status(&session, &[], now).await.unwrap();
    let (secret, _, _) = status.pending.unwrap();
    let code = listmngr_db::totp::code(&listmngr_db::totp::decode(&secret).unwrap(), now / 30_000);
    db.browser_totp_confirm(&session, &code, now).await.unwrap();
    db.pool().close().await;
}

#[tokio::test]
async fn secrets_are_sealed_rewrapped_and_watched_by_the_doctor() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("site.db").display());
    enrolled_in_the_clear(&url).await;
    let previous = dir.path().join("previous.key");
    tokio::task::spawn_blocking(move || {
        // No key: the doctor warns, the row is in the clear, encrypt refuses.
        let (code, status, detail) = doctor_check(&url, None, "master_key");
        assert_eq!((code, status.as_str()), (Some(0), "warn"), "{detail}");
        assert_eq!(detail, "no_master_key_totp_secrets_in_clear");
        let state = json(
            &listmngr(&url, None)
                .args(["secrets", "status"])
                .output()
                .unwrap(),
        );
        assert_eq!(state["master_key"], false);
        assert_eq!(
            (state["totp_sealed"].as_u64(), state["totp_plain"].as_u64()),
            (Some(0), Some(1))
        );
        let refused = listmngr(&url, None)
            .args(["secrets", "encrypt"])
            .output()
            .unwrap();
        assert!(!refused.status.success());
        assert!(String::from_utf8_lossy(&refused.stderr).contains("error[CLI-VALIDATION]"));
        // A key configured but rows still in the clear: a warning, then encrypt.
        let (_, status, detail) = doctor_check(&url, Some(KEY_A), "master_key");
        assert_eq!(
            (status.as_str(), detail.as_str()),
            ("warn", "plain_totp_secrets_remain_run_secrets_encrypt")
        );
        let sealed = json(
            &listmngr(&url, Some(KEY_A))
                .args(["secrets", "encrypt"])
                .output()
                .unwrap(),
        );
        assert_eq!(sealed["sealed"], 1);
        let (code, status, detail) = doctor_check(&url, Some(KEY_A), "master_key");
        assert_eq!(
            (code, status.as_str(), detail.as_str()),
            (Some(0), "ok", "totp_secrets_sealed")
        );
        // The key withdrawn: the sealed row is a failure, exit 12.
        let (code, status, detail) = doctor_check(&url, None, "master_key");
        assert_eq!(
            (code, status.as_str(), detail.as_str()),
            (Some(12), "fail", "sealed_totp_secrets_without_master_key")
        );
        // A new key minted; the rows rewrapped from the previous one.
        let minted = listmngr(&url, None)
            .args(["secrets", "new-key"])
            .output()
            .unwrap();
        assert!(minted.status.success());
        let new_key = String::from_utf8_lossy(&minted.stdout).trim().to_owned();
        assert_eq!(new_key.len(), 64);
        assert!(new_key.bytes().all(|b| b.is_ascii_hexdigit()));
        std::fs::write(&previous, KEY_A).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&previous, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let wrong = listmngr(&url, Some(KEY_A))
            .args(["secrets", "rewrap", "--previous-key-file"])
            .arg(&previous)
            .output()
            .unwrap();
        assert!(wrong.status.success(), "{wrong:?}");
        assert_eq!(
            json(&wrong)["rewrapped"],
            1,
            "the same key in and out still rewraps"
        );
        let moved = listmngr(&url, Some(&new_key))
            .args(["secrets", "rewrap", "--previous-key-file"])
            .arg(&previous)
            .output()
            .unwrap();
        assert!(moved.status.success(), "{moved:?}");
        assert_eq!(json(&moved)["rewrapped"], 1);
        let (code, status, _) = doctor_check(&url, Some(&new_key), "master_key");
        assert_eq!((code, status.as_str()), (Some(0), "ok"));
        let old_key_again = listmngr(&url, Some(&new_key))
            .args(["secrets", "rewrap", "--previous-key-file"])
            .arg(&previous)
            .output()
            .unwrap();
        assert!(
            !old_key_again.status.success(),
            "the old key no longer opens the rows"
        );
        assert!(String::from_utf8_lossy(&old_key_again.stderr).contains("error[CLI-VALIDATION]"));
    })
    .await
    .unwrap();
}
