//! Mailman regenerates the MTA's lookup maps when a list is created or
//! removed through REST; `[mta] incoming` selects the format and directory.
use super::*;

#[tokio::test]
async fn creating_and_removing_a_list_republishes_the_mta_maps() {
    let dir = tempfile::tempdir().unwrap();
    let maps = dir.path().join("mta");
    let mut config = config_with_rate(100);
    config.mta.incoming = "postfix".into();
    config.mta.map_directory = maps.display().to_string();
    config.mta.lmtp_map_target = Some("listmngr:8024".into());
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let user = db
        .users()
        .create(NewUser {
            display_name: "Admin".into(),
            email: "admin@example.com".into(),
            password: "very secure password".into(),
            server_owner: true,
        })
        .await
        .unwrap();
    let token = db
        .tokens()
        .create(user.id, "test", &["admin"], None)
        .await
        .unwrap()
        .token;
    let app = listmngr_api::router(db, config);
    assert!(!maps.exists(), "nothing is written before a list changes");

    create_configurable_list(&app, &token).await;
    let current = maps.join("current");
    let transport = std::fs::read_to_string(current.join("transport.regexp")).unwrap();
    assert!(
        transport.contains("/^dev@example\\.com$/ lmtp:[listmngr]:8024\n"),
        "{transport}"
    );
    assert!(
        transport.contains("/^dev-bounces\\+[^@=]+=[^@=]+@example\\.com$/ lmtp:[listmngr]:8024\n"),
        "{transport}"
    );
    assert_eq!(
        std::fs::read_to_string(current.join("domains.regexp")).unwrap(),
        "/^example\\.com$/ OK\n"
    );

    assert_eq!(
        call(
            &app,
            "DELETE",
            "/api/v1/lists/dev.example.com",
            Some(&token),
            None
        )
        .await
        .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        std::fs::read_to_string(current.join("transport.regexp")).unwrap(),
        ""
    );
    let generations = std::fs::read_dir(&maps)
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("generation-")
        })
        .count();
    assert_eq!(generations, 2);
}

#[tokio::test]
async fn without_an_incoming_mta_nothing_is_written() {
    let (app, token, _) = setup(&["admin"]).await;
    let before = std::env::current_dir().unwrap().join("data");
    let existed = before.exists();
    create_configurable_list(&app, &token).await;
    assert_eq!(
        before.exists(),
        existed,
        "the default `data/mta` stays untouched"
    );
}
