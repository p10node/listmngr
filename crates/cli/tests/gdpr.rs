//! `listmngr user export` and `user erase`: the same export and erasure the
//! browser offers, from the command line.
use assert_cmd::Command;
use listmngr_db::{Database, NewList, NewMember, NewUser};
use predicates::prelude::*;
use std::path::Path;

fn command(root: &Path, url: &str) -> Command {
    let mut command = Command::cargo_bin("listmngr").unwrap();
    command
        .env_clear()
        .current_dir(root)
        .env("LISTMNGR__DATABASE__URL", url);
    command
}

/// A domain, a list, a member account with a token, and a server owner.
fn setup(root: &Path) -> (String, String, String) {
    let url = format!("sqlite://{}?mode=rwc", root.join("fixture.db").display());
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let db = Database::connect(&url, 1).await.unwrap();
        db.migrate().await.unwrap();
        db.domains()
            .create("example.invalid", "", None)
            .await
            .unwrap();
        db.lists()
            .create(NewList {
                list_id: "dev.example.invalid".parse().unwrap(),
                display_name: "Dev".into(),
                style: "legacy-default".into(),
            })
            .await
            .unwrap();
        let member = db
            .users()
            .create(NewUser {
                display_name: "Member".into(),
                email: "member@example.invalid".into(),
                password: "long enough password".into(),
                server_owner: false,
            })
            .await
            .unwrap();
        db.members()
            .create(NewMember {
                list_id: "dev.example.invalid".parse().unwrap(),
                email: "member@example.invalid".into(),
                role: listmngr_core::MemberRole::Member,
                subscription_mode: listmngr_core::SubscriptionMode::AsAddress,
                display_name: String::new(),
            })
            .await
            .unwrap();
        db.tokens()
            .create(member.id, "cli token", &["lists:read"], None)
            .await
            .unwrap();
        let owner = db
            .users()
            .create(NewUser {
                display_name: "Owner".into(),
                email: "owner@example.invalid".into(),
                password: "long enough password".into(),
                server_owner: true,
            })
            .await
            .unwrap();
        db.addresses()
            .verify("owner@example.invalid", true)
            .await
            .unwrap();
        (url, member.id.to_string(), owner.id.to_string())
    })
}

#[test]
fn user_export_prints_the_account_without_secrets_and_erase_removes_it() {
    let root = tempfile::tempdir().unwrap();
    let (url, member, owner) = setup(root.path());
    command(root.path(), &url)
        .args(["user", "export", &member])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "\"format\": \"listmngr-account-export/1\"",
        ))
        .stdout(predicate::str::contains("member@example.invalid"))
        .stdout(predicate::str::contains("dev.example.invalid"))
        .stdout(predicate::str::contains("cli token"))
        .stdout(predicate::str::contains("password_hash").not())
        .stdout(predicate::str::contains("token_hash").not());
    command(root.path(), &url)
        .args(["user", "erase", &owner])
        .assert()
        .failure()
        .stderr(predicate::str::contains("CLI-VALIDATION"));
    command(root.path(), &url)
        .args(["user", "erase", &member])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "1 memberships, 1 addresses, 1 tokens",
        ));
    command(root.path(), &url)
        .args(["user", "export", &member])
        .assert()
        .failure();
    command(root.path(), &url)
        .args(["user", "export", &owner])
        .assert()
        .success()
        .stdout(predicate::str::contains("owner@example.invalid"));
    let erased = tokio::runtime::Runtime::new().unwrap().block_on(async {
        let db = Database::connect(&url, 1).await.unwrap();
        let members: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM members")
            .fetch_one(db.pool())
            .await
            .unwrap();
        let audited: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='user.delete'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        (members, audited)
    });
    assert_eq!(erased, (0, 1));
}
