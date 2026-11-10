//! `import3 --db`: a source row's `message.path` selects a file under the
//! core's message store only. `..` components and absolute paths are
//! refused with a warning and never read.
use listmngr_import::db3::fetch_db;
use std::path::PathBuf;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mailman3")
}

#[tokio::test]
async fn a_message_path_cannot_leave_the_message_store() {
    sqlx::any::install_default_drivers();
    let root = std::env::temp_dir().join(format!("listmngr-v16-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("var/messages")).unwrap();
    std::fs::create_dir_all(root.join("outside")).unwrap();
    let pickled = fixtures().join("var/messages/JO/3F/JO3FPU5E5P46AIKO2SWXPXY5QWDPFO4O");
    let leaked = root.join("outside/leaked.pck");
    std::fs::copy(&pickled, &leaked).unwrap();
    for (name, path) in [
        ("rel", "../../outside/leaked.pck".to_string()),
        ("abs", leaked.display().to_string()),
    ] {
        let db = root.join(format!("{name}.db"));
        std::fs::copy(fixtures().join("mailman.db"), &db).unwrap();
        let pool = sqlx::AnyPool::connect(&format!("sqlite://{}?mode=rw", db.display()))
            .await
            .unwrap();
        sqlx::query(&format!("UPDATE message SET path='{path}'"))
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
        let site = fetch_db(
            &format!("sqlite://{}?mode=ro", db.display()),
            Some(&root.join("var")),
        )
        .await
        .unwrap();
        let announce = site
            .lists
            .iter()
            .find(|list| list.list_id.as_str() == "announce.other.invalid")
            .unwrap();
        assert!(announce.held.is_empty(), "{name}: read outside the store");
        assert!(
            site.warnings
                .iter()
                .any(|w| w.contains("<held-1@example.invalid>") && w.contains("not imported")),
            "{name}: {:?}",
            site.warnings
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}
