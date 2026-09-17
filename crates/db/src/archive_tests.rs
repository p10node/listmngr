use super::*;

#[tokio::test]
async fn dmarc_delivery_policy_does_not_rewrite_archive_authorship() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    let mut settings =
        listmngr_core::MailingList::new("test.example.invalid".parse().unwrap(), "Test".into());
    settings.dmarc.action = listmngr_core::DmarcMitigateAction::MungeFrom;
    settings.dmarc.unconditional = true;
    let raw = base64::engine::general_purpose::STANDARD
        .encode(b"From: author@elsewhere.invalid\r\nMessage-ID: <p@elsewhere.invalid>\r\n\r\nbody");
    let rows = sqlx::query("SELECT 'hash' AS hash,'hash' AS thread,0 AS anonymous_list,'' AS subject_prefix,'munge_from' AS dmarc_mitigate_action,1 AS dmarc_mitigate_unconditionally,$1 AS raw_b64,'' AS sender_name,'' AS sender_email,NULL AS message_date,NULL AS parent_hash").bind(raw).fetch_all(db.pool()).await.unwrap();
    let rendered = render_rows(settings, &rows, None).unwrap();
    let parsed = mail_parser::MessageParser::default()
        .parse(&rendered[0].raw)
        .unwrap();
    assert_eq!(
        parsed.from().unwrap().first().unwrap().address(),
        Some("author@elsewhere.invalid")
    );
}

#[tokio::test]
async fn policy_change_after_authorization_cannot_leak_snapshot() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    let list: ListId = "test.example.invalid".parse().unwrap();
    db.lists()
        .create(crate::NewList {
            list_id: list.clone(),
            display_name: "Test".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    sqlx::query("INSERT INTO archive_messages(list_id,hash,thread,subject,body,raw_b64,created_at) VALUES($1,'hash','hash','private subject','private body','',0)")
        .bind(list.as_str()).execute(db.pool()).await.unwrap();
    db.archive().authorize(&list, None).await.unwrap();
    db.lists()
        .update(&list, &serde_json::json!({"archive_policy":"private"}))
        .await
        .unwrap();
    // Deterministically interleave privacy change between initial authorization
    // and the production SELECT, without scheduler timing or a live database.
    assert!(
        db.archive()
            .read_snapshot(&list, None, None, "", 100, 0)
            .await
            .unwrap()
            .is_empty()
    );
}
