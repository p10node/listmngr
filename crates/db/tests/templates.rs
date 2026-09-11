//! Template resolution across scopes and languages, template-URI management
//! with the Mailman names, and the fail-safe fallback to the built-in catalog.
use listmngr_db::templates::{Scope, TemplateUri};
use listmngr_db::{Database, NewList};

async fn fixture() -> (Database, listmngr_core::MailingList) {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: "dev.example.invalid".parse().unwrap(),
            display_name: "Dev".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    (db, list)
}

const WELCOME: &str = "list:user:notice:welcome";

#[tokio::test]
async fn resolution_falls_back_to_the_builtin_catalog() {
    let (db, list) = fixture().await;
    let resolved = db.templates().resolve(WELCOME, &list, "en").await.unwrap();
    assert_eq!(
        resolved.body,
        listmngr_mail::templates::builtin(WELCOME).unwrap()
    );
    assert_eq!(resolved.source, "builtin:en");
    // The built-in catalog is served in the negotiated language too.
    let resolved = db
        .templates()
        .resolve(WELCOME, &list, "vi-VN")
        .await
        .unwrap();
    assert_eq!(
        resolved.body,
        listmngr_mail::templates::builtin_in(WELCOME, "vi").unwrap()
    );
    assert_eq!(resolved.source, "builtin:vi");
    assert!(
        db.templates()
            .resolve("list:user:notice:made-up", &list, "en")
            .await
            .is_err(),
        "an unknown name is a programming error, not a silent empty notice"
    );
}

#[tokio::test]
async fn list_beats_domain_beats_site_and_languages_fall_back_to_english() {
    let (db, list) = fixture().await;
    let site = Scope::Site;
    let domain = Scope::Domain("example.invalid".into());
    let scope = Scope::List(list.id.clone());
    db.templates()
        .set_body(&site, WELCOME, "en", "site en")
        .await
        .unwrap();
    assert_eq!(
        db.templates()
            .resolve(WELCOME, &list, "vi")
            .await
            .unwrap()
            .body,
        "site en",
        "site English serves a Vietnamese request when nothing closer exists"
    );
    db.templates()
        .set_body(&domain, WELCOME, "vi", "domain vi")
        .await
        .unwrap();
    assert_eq!(
        db.templates()
            .resolve(WELCOME, &list, "vi")
            .await
            .unwrap()
            .body,
        "domain vi"
    );
    assert_eq!(
        db.templates()
            .resolve(WELCOME, &list, "en")
            .await
            .unwrap()
            .body,
        "site en",
        "an English request does not pick up the Vietnamese domain body"
    );
    db.templates()
        .set_body(&scope, WELCOME, "en", "list en")
        .await
        .unwrap();
    assert_eq!(
        db.templates()
            .resolve(WELCOME, &list, "vi")
            .await
            .unwrap()
            .body,
        "list en",
        "the closest scope wins even when only its English body exists"
    );
    let resolved = db.templates().resolve(WELCOME, &list, "en").await.unwrap();
    assert_eq!(resolved.body, "list en");
    assert_eq!(resolved.source, "list:en");
}

#[tokio::test]
async fn template_uris_are_validated_listed_without_secrets_and_deletable() {
    let (db, list) = fixture().await;
    let scope = Scope::List(list.id.clone());
    assert!(db.templates().list_uris(&scope).await.unwrap().is_empty());
    for (name, uri) in [
        (
            "list:user:notice:made-up",
            "mailman:///list:user:notice:welcome",
        ),
        (WELCOME, "mailman:///list:user:notice:made-up"),
        (WELCOME, "http://plain.example.invalid/x.txt"),
        (WELCOME, "file://relative.txt"),
        (WELCOME, ""),
        (WELCOME, "https://x.invalid/a b"),
    ] {
        assert!(
            db.templates()
                .set_uri(&scope, name, uri, None, None)
                .await
                .is_err(),
            "{name} {uri:?}"
        );
    }
    db.templates()
        .set_uri(
            &scope,
            WELCOME,
            "https://templates.example.invalid/welcome.txt",
            Some("reader"),
            Some("hunter2"),
        )
        .await
        .unwrap();
    db.templates()
        .set_uri(
            &scope,
            "list:user:notice:goodbye",
            "mailman:///list:user:notice:goodbye",
            None,
            None,
        )
        .await
        .unwrap();
    let listed = db.templates().list_uris(&scope).await.unwrap();
    assert_eq!(
        listed,
        vec![
            TemplateUri {
                name: "list:user:notice:goodbye".into(),
                uri: "mailman:///list:user:notice:goodbye".into(),
                username: None,
            },
            TemplateUri {
                name: WELCOME.into(),
                uri: "https://templates.example.invalid/welcome.txt".into(),
                username: Some("reader".into()),
            },
        ]
    );
    let audit: String = sqlx::query_scalar::<_, String>(
        "SELECT diff FROM audit_log WHERE action='template.set' ORDER BY rowid DESC LIMIT 2",
    )
    .fetch_all(db.pool())
    .await
    .unwrap()
    .join("|");
    assert!(!audit.contains("hunter2"), "password leaked into audit");

    // Setting again replaces; an https template is never fetched, so
    // resolution falls back to the built-in rather than failing the notice.
    let resolved = db.templates().resolve(WELCOME, &list, "en").await.unwrap();
    assert_eq!(resolved.source, "builtin:en");

    db.templates().delete(&scope, Some(WELCOME)).await.unwrap();
    assert_eq!(db.templates().list_uris(&scope).await.unwrap().len(), 1);
    db.templates().delete(&scope, None).await.unwrap();
    assert!(db.templates().list_uris(&scope).await.unwrap().is_empty());
}

#[tokio::test]
async fn file_templates_are_read_and_a_broken_file_falls_back_to_the_builtin() {
    let (db, list) = fixture().await;
    let scope = Scope::List(list.id.clone());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("welcome.txt");
    std::fs::write(&path, "Custom welcome for $display_name\n").unwrap();
    db.templates()
        .set_uri(
            &scope,
            WELCOME,
            &format!("file://{}", path.display()),
            None,
            None,
        )
        .await
        .unwrap();
    let resolved = db.templates().resolve(WELCOME, &list, "en").await.unwrap();
    assert_eq!(resolved.body, "Custom welcome for $display_name\n");
    assert_eq!(resolved.source, "list:en");

    std::fs::remove_file(&path).unwrap();
    let resolved = db.templates().resolve(WELCOME, &list, "en").await.unwrap();
    assert_eq!(
        resolved.source, "builtin:en",
        "a missing file must not block notices"
    );
}

#[tokio::test]
async fn bodies_are_bounded_and_must_be_plain_text() {
    let (db, list) = fixture().await;
    let scope = Scope::List(list.id.clone());
    assert!(
        db.templates()
            .set_body(&scope, WELCOME, "en", &"x".repeat(70_000))
            .await
            .is_err()
    );
    assert!(
        db.templates()
            .set_body(&scope, WELCOME, "", "no language")
            .await
            .is_err()
    );
    assert!(
        db.templates()
            .set_body(&Scope::Domain("nope.invalid".into()), WELCOME, "en", "x")
            .await
            .is_err(),
        "unknown domain scope"
    );
    assert!(
        db.templates()
            .set_body(
                &Scope::List("ghost.example.invalid".parse().unwrap()),
                WELCOME,
                "en",
                "x"
            )
            .await
            .is_err(),
        "unknown list scope"
    );
    db.templates()
        .set_body(&scope, WELCOME, "en", "line one\r\nline two")
        .await
        .unwrap();
    assert_eq!(
        db.templates()
            .resolve(WELCOME, &list, "en")
            .await
            .unwrap()
            .body,
        "line one\nline two",
        "CRLF is normalized to LF at rest"
    );
}
