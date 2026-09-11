use listmngr_core::{Error, ListId};
use listmngr_db::{AuditContext, Database, NewList};

async fn fixture() -> (Database, ListId) {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains().create("bans.invalid", "", None).await.unwrap();
    let id = add_list(&db, "first.bans.invalid").await;
    (db, id)
}

async fn add_list(db: &Database, name: &str) -> ListId {
    db.lists()
        .create(NewList {
            list_id: name.parse().unwrap(),
            display_name: name.into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap()
        .id
}

#[tokio::test]
async fn scoped_collection_paginates_and_deletes_canonical_values() {
    let (db, id) = fixture().await;
    let other = add_list(&db, "other.bans.invalid").await;
    let context = AuditContext::system();
    for value in ["z@example.org", "a@example.org", "m@example.org"] {
        db.bans().create(&id, value, &context).await.unwrap();
    }
    db.bans()
        .create(&other, "a@example.org", &context)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO bans(id,list_id,email_or_regex) VALUES('global',NULL,'global@example.org')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    assert_eq!(db.bans().count(&id).await.unwrap(), 3);
    assert_eq!(db.bans().list(&id, 1, 1).await.unwrap(), ["m@example.org"]);
    assert_eq!(
        db.bans().list(&id, 10, 0).await.unwrap(),
        ["a@example.org", "m@example.org", "z@example.org"]
    );
    assert!(db.bans().list(&id, 1, 3).await.unwrap().is_empty());
    db.bans()
        .delete(&id, "A@EXAMPLE.ORG", &context)
        .await
        .unwrap();
    assert_eq!(db.bans().count(&id).await.unwrap(), 2);
    assert_eq!(
        db.bans().list(&other, 10, 0).await.unwrap(),
        ["a@example.org"]
    );
    assert!(matches!(
        db.bans().delete(&id, "a@example.org", &context).await,
        Err(Error::NotFound(_))
    ));
    let missing = "missing.bans.invalid".parse().unwrap();
    assert!(matches!(
        db.bans().list(&missing, 10, 0).await,
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        db.bans().count(&missing).await,
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        db.bans().create(&missing, "a@example.org", &context).await,
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        db.bans().delete(&missing, "a@example.org", &context).await,
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        db.bans().list(&id, -1, 0).await,
        Err(Error::Validation(_))
    ));
    assert!(matches!(
        db.bans().list(&id, 10, -1).await,
        Err(Error::Validation(_))
    ));
}

#[tokio::test]
async fn regex_is_bounded_validated_and_preserved_verbatim() {
    let (db, id) = fixture().await;
    let context = AuditContext::system();
    for value in [
        r"^Alice@EXAMPLE\.ORG$",
        r"^alice@example\.org$",
        r"^(?i)Mixed@Example\.org$",
        &format!("^{}", "a".repeat(1023)),
    ] {
        assert_eq!(db.bans().create(&id, value, &context).await.unwrap(), value);
        db.bans().delete(&id, value, &context).await.unwrap();
    }
    let before = db.audit().list().await.unwrap().len();
    for value in [
        "",
        " a@example.org",
        "a @example.org",
        "^a b",
        "^a\n",
        "^a\u{00a0}b",
        "^[",
        r"^(a)\1",
        r"^(?=a)",
        "^a{100000000}",
        &format!("^{}", "a".repeat(1024)),
    ] {
        assert!(
            matches!(
                db.bans().create(&id, value, &context).await,
                Err(Error::Validation(_))
            ),
            "{value:?}"
        );
        assert!(
            matches!(
                db.bans().delete(&id, value, &context).await,
                Err(Error::Validation(_))
            ),
            "{value:?}"
        );
    }
    assert_eq!(db.bans().count(&id).await.unwrap(), 0);
    assert_eq!(db.audit().list().await.unwrap().len(), before);
}

#[tokio::test]
async fn regex_case_identity_and_idna_mailbox_follow_runner_storage_contract() {
    let (db, id) = fixture().await;
    let context = AuditContext::system();
    let upper = r"^Alice@EXAMPLE\.ORG$";
    let lower = r"^alice@example\.org$";
    db.bans().create(&id, upper, &context).await.unwrap();
    db.bans().create(&id, lower, &context).await.unwrap();
    assert!(matches!(
        db.bans().create(&id, upper, &context).await,
        Err(Error::Conflict(_))
    ));
    let values = db.bans().list(&id, 10, 0).await.unwrap();
    assert_eq!(values, [upper, lower]);
    let regex = regex::Regex::new(&values[0]).unwrap();
    assert!(regex.is_match("Alice@EXAMPLE.ORG"));
    assert!(!regex.is_match("alice@example.org"));
    db.bans().delete(&id, upper, &context).await.unwrap();
    assert_eq!(db.bans().list(&id, 10, 0).await.unwrap(), [lower]);
    assert_eq!(
        db.bans()
            .create(&id, "Alice@BÜCHER.example", &context)
            .await
            .unwrap(),
        "alice@xn--bcher-kva.example"
    );
    db.bans()
        .delete(&id, "ALICE@xn--bcher-kva.example", &context)
        .await
        .unwrap();
}

#[tokio::test]
async fn invalid_regex_fails_before_writer_reservation() {
    let (db, id) = fixture().await;
    sqlx::query("CREATE TRIGGER reject_list_write BEFORE UPDATE ON mailing_lists BEGIN SELECT RAISE(ABORT, 'fixture writer reached'); END")
        .execute(db.pool()).await.unwrap();
    let context = AuditContext::system();
    assert!(matches!(
        db.bans().create(&id, "^[", &context).await,
        Err(Error::Validation(_))
    ));
    assert!(matches!(
        db.bans().delete(&id, "^[", &context).await,
        Err(Error::Validation(_))
    ));
    assert!(matches!(
        db.bans().create(&id, "^valid$", &context).await,
        Err(Error::Database(_))
    ));
    assert_eq!(db.bans().count(&id).await.unwrap(), 0);
}

async fn reject_audit(db: &Database) {
    sqlx::query("CREATE TRIGGER reject_ban_audit BEFORE INSERT ON audit_log BEGIN SELECT RAISE(ABORT, 'fixture audit failure'); END")
        .execute(db.pool()).await.unwrap();
}

async fn allow_audit(db: &Database) {
    sqlx::query("DROP TRIGGER reject_ban_audit")
        .execute(db.pool())
        .await
        .unwrap();
}

#[tokio::test]
async fn audit_failure_rolls_back_both_writes_and_retry_preserves_attribution() {
    let (db, id) = fixture().await;
    let user = listmngr_core::UserId::new();
    let token = listmngr_core::TokenId::new();
    let ip = "192.0.2.9".parse().unwrap();
    let context = AuditContext::new(Some(user), Some(token), Some(ip));
    let before = db.audit().list().await.unwrap().len();
    reject_audit(&db).await;
    assert!(matches!(
        db.bans().create(&id, "Alice@EXAMPLE.ORG", &context).await,
        Err(Error::Database(_))
    ));
    assert_eq!(db.bans().count(&id).await.unwrap(), 0);
    assert_eq!(db.audit().list().await.unwrap().len(), before);
    allow_audit(&db).await;
    db.bans()
        .create(&id, "Alice@EXAMPLE.ORG", &context)
        .await
        .unwrap();
    reject_audit(&db).await;
    assert!(matches!(
        db.bans().delete(&id, "ALICE@example.org", &context).await,
        Err(Error::Database(_))
    ));
    assert_eq!(
        db.bans().list(&id, 10, 0).await.unwrap(),
        ["alice@example.org"]
    );
    assert_eq!(db.audit().list().await.unwrap().len(), before + 1);
    allow_audit(&db).await;
    db.bans()
        .delete(&id, "ALICE@example.org", &context)
        .await
        .unwrap();
    assert_eq!(db.bans().count(&id).await.unwrap(), 0);
    let entries = db.audit().list().await.unwrap();
    let bans: Vec<_> = entries
        .iter()
        .filter(|entry| entry.action.starts_with("ban."))
        .collect();
    assert_eq!(bans.len(), 2);
    for (entry, action) in bans.iter().zip(["ban.create", "ban.delete"]) {
        assert_eq!(entry.action, action);
        assert_eq!(entry.target_type, "list");
        assert_eq!(entry.target_id, id.as_str());
        assert_eq!(entry.actor_user_id, Some(user));
        assert_eq!(entry.actor_token_id, Some(token));
        assert_eq!(entry.ip, Some(ip));
        assert_eq!(
            entry.diff,
            serde_json::json!({"email_or_regex":"alice@example.org"})
        );
    }
}

#[tokio::test]
async fn exact_mailbox_is_canonical_and_duplicate_is_conflict() {
    let (db, id) = fixture().await;
    let context = AuditContext::system();
    assert_eq!(
        db.bans()
            .create(&id, "Alice@EXAMPLE.ORG", &context)
            .await
            .unwrap(),
        "alice@example.org"
    );
    assert!(matches!(
        db.bans().create(&id, "ALICE@example.org", &context).await,
        Err(Error::Conflict(_))
    ));
}
