use assert_cmd::Command;
use listmngr_db::{Database, NewList};
#[test]
fn digest_commands_run_against_isolated_file_database() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("digest.db").display()
    );
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let db = Database::connect(&url, 1).await.unwrap();
        db.migrate().await.unwrap();
        db.domains()
            .create("example.invalid", "", None)
            .await
            .unwrap();
        db.lists()
            .create(NewList {
                list_id: "test.example.invalid".parse().unwrap(),
                display_name: "Test".into(),
                style: "legacy-default".into(),
            })
            .await
            .unwrap();
        db.pool().close().await;
    });
    for args in [
        vec!["digests", "send", "test.example.invalid"],
        vec!["digests", "periodic"],
        vec!["digests", "bump", "test.example.invalid"],
    ] {
        Command::cargo_bin("listmngr")
            .unwrap()
            .env_clear()
            .current_dir(dir.path())
            .env("LISTMNGR__DATABASE__URL", &url)
            .args(args)
            .assert()
            .success();
    }
    rt.block_on(async {
        let db = Database::connect(&url, 1).await.unwrap();
        let list = db
            .lists()
            .get(&"test.example.invalid".parse().unwrap())
            .await
            .unwrap();
        assert_eq!(list.volume, 2);
        assert_eq!(list.next_digest_number, 1);
        db.pool().close().await;
    });
}
