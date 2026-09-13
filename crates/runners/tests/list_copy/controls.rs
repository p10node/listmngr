// Supplementary regression corpus; behavioral REDs are in list_copy.rs.
use super::*;

pub async fn controls(db: Database) {
    let (db, list) = fixture(db).await;
    for (headers, suppress) in [
        ("To: Reader@example.invalid\r\n", true),
        (
            "To: outside@outside.invalid\r\nCc: group: READER@EXAMPLE.INVALID.,\r\n Other@outside.invalid;\r\n",
            true,
        ),
        (
            "To: outside@outside.invalid\r\nCc: outside@outside.invalid\r\nCc: Reader@example.invalid\r\n",
            true,
        ),
        ("Bcc: Reader@example.invalid\r\n", false),
        (
            "To: \"Reader@example.invalid\" <outside@outside.invalid>\r\n",
            false,
        ),
        ("To: notReader@example.invalid\r\n", false),
        ("To: Reader@example.invalid (unfinished\r\n", false),
        ("To: Reader@example.invalid, not a mailbox\r\n", false),
        ("Broken header\r\nTo: Reader@example.invalid\r\n", false),
        ("\r\nTo: Reader@example.invalid\r\n", false),
    ] {
        let expected = if suppress {
            vec!["Other@example.invalid"]
        } else {
            vec!["Other@example.invalid", "Reader@example.invalid"]
        };
        let message = post(&db, &list, headers).await;
        assert_eq!(
            recipients(&db, message).await,
            expected,
            "regular {headers}"
        );
        let held = held_post(&db, &list, headers).await;
        db.moderation()
            .review(
                held.id,
                &listmngr_db::AuditContext::system(),
                &listmngr_db::moderation::ReviewAction::Accept { max_attempts: 3 },
                "fixture",
                chrono::Utc::now().timestamp_millis(),
            )
            .await
            .unwrap();
        assert_eq!(
            recipients(&db, held.message_id).await,
            expected,
            "held {headers}"
        );
    }
    preferences(&db, &list).await;
    own_vs_list(&db, &list).await;
    db.pool().close().await;
}

async fn preferences(db: &Database, list: &listmngr_core::ListId) {
    let reader = db
        .members()
        .find("reader@example.invalid")
        .await
        .unwrap()
        .remove(0);
    let inherited = uuid::Uuid::now_v7().to_string();
    sqlx::query("INSERT INTO preferences(id,receive_list_copy) VALUES($1,0)")
        .bind(&inherited)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE addresses SET preferences_id=$1 WHERE id=$2")
        .bind(&inherited)
        .bind(reader.address_id.to_string())
        .execute(db.pool())
        .await
        .unwrap();
    for (copy, own, mode, status, expected) in [
        (None, 1, "regular", "enabled", false),
        (Some(1), 0, "regular", "enabled", true),
        (Some(1), 1, "mime_digests", "enabled", false),
        (Some(1), 1, "regular", "by_user", false),
        (Some(1), 1, "regular", "by_bounces", false),
        (Some(1), 1, "regular", "by_moderator", false),
    ] {
        sqlx::query("UPDATE preferences SET receive_list_copy=$1,receive_own_postings=$2,delivery_mode=$3,delivery_status=$4 WHERE id=$5")
            .bind(copy).bind(own).bind(mode).bind(status).bind(reader.preferences_id.to_string()).execute(db.pool()).await.unwrap();
        let message = post(db, list, "To: reader@example.invalid\r\n").await;
        assert_eq!(
            recipients(db, message)
                .await
                .contains(&"Reader@example.invalid".into()),
            expected
        );
        let held = held_post(db, list, "To: reader@example.invalid\r\n").await;
        db.moderation()
            .review(
                held.id,
                &listmngr_db::AuditContext::system(),
                &listmngr_db::moderation::ReviewAction::Accept { max_attempts: 3 },
                "fixture",
                chrono::Utc::now().timestamp_millis(),
            )
            .await
            .unwrap();
        assert_eq!(
            recipients(db, held.message_id)
                .await
                .contains(&"Reader@example.invalid".into()),
            expected
        );
    }
}

async fn own_vs_list(db: &Database, list: &listmngr_core::ListId) {
    let author = db
        .members()
        .create(NewMember {
            list_id: list.clone(),
            email: "author@example.invalid".into(),
            display_name: String::new(),
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsAddress,
        })
        .await
        .unwrap();
    sqlx::query("UPDATE members SET moderation_action='accept' WHERE id=$1")
        .bind(author.id.to_string())
        .execute(db.pool())
        .await
        .unwrap();
    for (own, copy, headers, expected) in [
        (0, 1, "To: outside@outside.invalid\r\n", false),
        (1, 0, "To: outside@outside.invalid\r\n", true),
        (1, 0, "To: author@example.invalid\r\n", false),
        (1, 1, "To: author@example.invalid\r\n", true),
    ] {
        sqlx::query(
            "UPDATE preferences SET receive_own_postings=$1,receive_list_copy=$2 WHERE id=$3",
        )
        .bind(own)
        .bind(copy)
        .bind(author.preferences_id.to_string())
        .execute(db.pool())
        .await
        .unwrap();
        let message = post(db, list, headers).await;
        assert_eq!(
            recipients(db, message)
                .await
                .contains(&"author@example.invalid".into()),
            expected
        );
        let held = held_post(db, list, headers).await;
        db.moderation()
            .review(
                held.id,
                &listmngr_db::AuditContext::system(),
                &listmngr_db::moderation::ReviewAction::Accept { max_attempts: 3 },
                "fixture",
                chrono::Utc::now().timestamp_millis(),
            )
            .await
            .unwrap();
        assert_eq!(
            recipients(db, held.message_id)
                .await
                .contains(&"author@example.invalid".into()),
            expected
        );
    }
}
