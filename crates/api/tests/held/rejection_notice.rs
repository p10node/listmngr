use super::*;

#[tokio::test]
async fn moderator_reject_publishes_one_private_notice_on_both_prefixes_and_encodings() {
    let f = fixture().await;
    for prefix in ["/api/v1", "/3.1"] {
        for json in [false, true] {
            let id = seed_held(
                &f.db,
                &f.list_id,
                "Exact@example.com",
                "private original subject",
            )
            .await;
            let source = f.db.moderation().get(id).await.unwrap().message_id;
            let original = f.db.mail_queue().message(source).await.unwrap().raw;
            let uri = format!("{prefix}/lists/{}/held/{}", f.list_id, id.0);
            assert_eq!(
                post_action(&f, &uri, "reject", json).await.status(),
                StatusCode::NO_CONTENT
            );
            let now = chrono::Utc::now().timestamp_millis();
            let notice =
                f.db.mail_queue()
                    .claim(Queue::Out, "notice-probe", now, 60_000)
                    .await
                    .unwrap()
                    .expect("reject must publish a durable author notice");
            assert_ne!(notice.job.message_id, source);
            assert!(f.db.workflows().is_notice(notice.job.id).await.unwrap());
            let stored =
                f.db.mail_queue()
                    .message(notice.job.message_id)
                    .await
                    .unwrap();
            let context: Value = serde_json::from_str(&stored.context).unwrap();
            // Routing context is not notice authority, nor a subscription claim.
            assert_eq!(context, serde_json::json!({"list_id": f.list_id.as_str()}));
            assert_eq!(
                f.db.mail_queue()
                    .pending_recipients(notice.job.id)
                    .await
                    .unwrap(),
                ["Exact@example.com"]
            );
            let raw =
                f.db.mail_queue()
                    .message(notice.job.message_id)
                    .await
                    .unwrap()
                    .raw;
            let text = String::from_utf8(raw).unwrap();
            assert!(text.contains("Auto-Submitted: auto-generated"));
            assert!(!text.contains("private original subject"));
            assert!(!text.contains("List-Post:"));
            assert_eq!(
                f.db.mail_queue().message(source).await.unwrap().raw,
                original
            );
            assert_eq!(
                post_action(&f, &uri, "reject", json).await.status(),
                StatusCode::CONFLICT
            );
            for queue in [Queue::Out, Queue::Archive, Queue::Digest] {
                assert!(
                    f.db.mail_queue()
                        .claim(queue, "no-extra", now, 60_000)
                        .await
                        .unwrap()
                        .is_none()
                );
            }
        }
    }
}
