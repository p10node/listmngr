//! Per-row header match edits behind Mailman's `header-matches` resource:
//! append, patch (with a move), remove, clear, and the duplicate rule.
use listmngr_core::Error;
use listmngr_db::{AuditContext, Database, FieldEdit, HeaderMatchPatch, HeaderMatchRow, NewList};

async fn fixture() -> (Database, listmngr_core::ListId) {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    let id = prepare(&db).await;
    (db, id)
}

async fn prepare(db: &Database) -> listmngr_core::ListId {
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    db.lists()
        .create(NewList {
            list_id: "rules.example.invalid".parse().unwrap(),
            display_name: "Rules".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap()
        .id
}

fn row(header: &str, pattern: &str) -> HeaderMatchRow {
    HeaderMatchRow {
        header: header.into(),
        pattern: pattern.into(),
        chain: None,
        tag: None,
    }
}

async fn audit(db: &Database) -> Vec<serde_json::Value> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT diff FROM audit_log WHERE action='list.header_matches' ORDER BY id",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    rows.iter()
        .map(|diff| serde_json::from_str(diff).unwrap())
        .collect()
}

#[tokio::test]
async fn rows_are_appended_patched_moved_removed_and_cleared_in_position_order() {
    let (db, id) = fixture().await;
    rows_are_appended_patched_moved_removed_and_cleared_in_position_order_on(&db, &id).await;
}

async fn rows_are_appended_patched_moved_removed_and_cleared_in_position_order_on(
    db: &Database,
    id: &listmngr_core::ListId,
) {
    appends_number_from_zero(db, id).await;
    let rows = a_patch_moves_and_edits_one_row(db, id).await;
    bad_patches_and_duplicates_leave_the_set_untouched(db, id, &rows).await;
    clearing_a_field_differs_from_keeping_it(db, id).await;
    removal_shifts_and_clear_empties(db, id).await;
    every_successful_edit_is_audited(db).await;
}

async fn appends_number_from_zero(db: &Database, id: &listmngr_core::ListId) {
    let context = AuditContext::system();
    let repo = db.header_matches();
    assert_eq!(
        repo.append(id, row("x-spam-flag", "^yes$"), &context)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        repo.append(id, row("subject", "viagra"), &context)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        repo.append(id, row("from", "@spam\\.invalid$"), &context)
            .await
            .unwrap(),
        2
    );
    assert_eq!(repo.get(id, 1).await.unwrap(), row("subject", "viagra"));
    assert!(matches!(repo.get(id, 3).await, Err(Error::NotFound(_))));
}

/// A patch changes only what it names; moving the last row first shifts
/// the others down and the row keeps its edits.
async fn a_patch_moves_and_edits_one_row(
    db: &Database,
    id: &listmngr_core::ListId,
) -> Vec<HeaderMatchRow> {
    let (position, moved) = db
        .header_matches()
        .update(
            id,
            2,
            HeaderMatchPatch {
                chain: FieldEdit::Set("discard".into()),
                tag: FieldEdit::Set("spam".into()),
                position: Some(0),
                ..HeaderMatchPatch::default()
            },
            &AuditContext::system(),
        )
        .await
        .unwrap();
    assert_eq!(position, 0);
    assert_eq!(moved.chain.as_deref(), Some("discard"));
    let rows = db.header_matches().list(id).await.unwrap();
    assert_eq!(
        rows.iter()
            .map(|row| row.header.as_str())
            .collect::<Vec<_>>(),
        ["from", "x-spam-flag", "subject"]
    );
    assert_eq!(rows[0].tag.as_deref(), Some("spam"));
    rows
}

/// A patch past the end, a duplicate of another row (header case-insensitive)
/// and a bad pattern are refused and leave the set untouched.
async fn bad_patches_and_duplicates_leave_the_set_untouched(
    db: &Database,
    id: &listmngr_core::ListId,
    rows: &[HeaderMatchRow],
) {
    let context = AuditContext::system();
    let repo = db.header_matches();
    for patch in [
        HeaderMatchPatch {
            position: Some(3),
            ..HeaderMatchPatch::default()
        },
        HeaderMatchPatch {
            header: Some("SUBJECT".into()),
            pattern: Some("viagra".into()),
            ..HeaderMatchPatch::default()
        },
        HeaderMatchPatch {
            pattern: Some("^(".into()),
            ..HeaderMatchPatch::default()
        },
    ] {
        assert!(matches!(
            repo.update(id, 1, patch, &context).await,
            Err(Error::Validation(_))
        ));
    }
    assert!(matches!(
        repo.update(id, 5, HeaderMatchPatch::default(), &context)
            .await,
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        repo.append(id, row("Subject", "viagra"), &context).await,
        Err(Error::Validation(_))
    ));
    assert_eq!(repo.list(id).await.unwrap(), rows);
}

async fn clearing_a_field_differs_from_keeping_it(db: &Database, id: &listmngr_core::ListId) {
    let (_, cleared) = db
        .header_matches()
        .update(
            id,
            0,
            HeaderMatchPatch {
                chain: FieldEdit::Clear,
                ..HeaderMatchPatch::default()
            },
            &AuditContext::system(),
        )
        .await
        .unwrap();
    assert_eq!(cleared.chain, None);
    assert_eq!(cleared.tag.as_deref(), Some("spam"));
}

async fn removal_shifts_and_clear_empties(db: &Database, id: &listmngr_core::ListId) {
    let context = AuditContext::system();
    let repo = db.header_matches();
    repo.remove(id, 1, &context).await.unwrap();
    assert_eq!(
        repo.list(id)
            .await
            .unwrap()
            .iter()
            .map(|row| row.header.as_str())
            .collect::<Vec<_>>(),
        ["from", "subject"]
    );
    assert!(matches!(
        repo.remove(id, 2, &context).await,
        Err(Error::NotFound(_))
    ));
    repo.clear(id, &context).await.unwrap();
    assert!(repo.list(id).await.unwrap().is_empty());
}

/// Every successful edit is audited with what it did; failures are not.
async fn every_successful_edit_is_audited(db: &Database) {
    let changes: Vec<String> = audit(db)
        .await
        .iter()
        .map(|diff| {
            format!(
                "{}:{}:{}",
                diff["change"].as_str().unwrap(),
                diff["position"],
                diff["count"]
            )
        })
        .collect();
    assert_eq!(
        changes,
        [
            "append:null:1",
            "append:null:2",
            "append:null:3",
            "update:2:3",
            "update:0:3",
            "remove:1:2",
            "clear:null:0"
        ]
    );
}

/// The same edits on a live `PostgreSQL` schema: the writer reservation,
/// the whole-set rewrite and the bound positions must behave identically.
#[tokio::test]
#[ignore = "requires explicit disposable TEST_POSTGRES_URL; uses own schema"]
async fn postgres_header_matches_contract() {
    sqlx::any::install_default_drivers();
    let url = std::env::var("TEST_POSTGRES_URL").expect("explicit fixture database required");
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("header_matches_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let isolated = format!(
        "{url}{}options=-csearch_path%3D{schema}",
        if url.contains('?') { '&' } else { '?' }
    );
    let result = tokio::spawn(async move {
        let db = Database::connect(&isolated, 2).await.unwrap();
        let id = prepare(&db).await;
        rows_are_appended_patched_moved_removed_and_cleared_in_position_order_on(&db, &id).await;
    })
    .await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
    result.unwrap();
}
