use super::score_cases::{finish, member};
#[path = "increment_supplement.rs"]
mod supplement;
use super::*;
use listmngr_core::{MemberRole, SmtpFailureStage, SubscriptionMode};

async fn setup_at(url: &str) -> (Database, Lease) {
    let (db, lease) = fixture_at(url).await;
    member(&db, MemberRole::Member, true).await;
    db.lists()
        .update(
            &"test.example.com".parse().unwrap(),
            &serde_json::json!({"bounce_notify_owner_on_bounce_increment":true}),
        )
        .await
        .unwrap();
    for (email, role) in [
        ("Owner@Example.com", MemberRole::Owner),
        ("Owner@Example.com", MemberRole::Moderator),
        ("Mod@example.com", MemberRole::Moderator),
    ] {
        db.members()
            .create(listmngr_db::NewMember {
                list_id: "test.example.com".parse().unwrap(),
                email: email.into(),
                display_name: String::new(),
                role,
                subscription_mode: SubscriptionMode::AsAddress,
            })
            .await
            .unwrap();
    }
    db.mail_queue()
        .begin_delivery(&lease, 103, &["Mixed@Example.com".into()])
        .await
        .unwrap();
    (db, lease)
}

async fn notices(db: &Database) -> Vec<Vec<u8>> {
    sqlx::query_scalar("SELECT b.raw FROM message_blobs b JOIN messages m ON m.store_key=b.store_key JOIN queue_jobs q ON q.message_id=m.id JOIN workflow_notices n ON n.job_id=q.id ORDER BY q.id").fetch_all(db.pool()).await.unwrap()
}

#[tokio::test]
async fn increment_notice_score_driven_snapshot_privacy_and_replay() {
    let (db, lease) = setup_at("sqlite::memory:").await;
    finish(&db, &lease, SmtpFailureStage::Rcpt, 550)
        .await
        .unwrap();
    let raws = notices(&db).await;
    assert_eq!(
        raws.len(),
        2,
        "fresh below-threshold observation must publish admin notices"
    );
    let recipients:Vec<String>=sqlx::query_scalar("SELECT r.email FROM delivery_recipients r JOIN workflow_notices n ON n.job_id=r.job_id ORDER BY r.email").fetch_all(db.pool()).await.unwrap();
    assert_eq!(recipients, vec!["Mod@example.com", "Owner@Example.com"]);
    for raw in raws {
        assert!(raw.len() <= 4096);
        let raw = String::from_utf8(raw).unwrap();
        assert!(raw.contains("bounce score incremented on test@example.com\r\n"));
        assert!(raw.contains("Mixed@Example.com's bounce score on test@example.com"));
        assert!(raw.contains("has been incremented to 1"));
        assert!(raw.contains("Auto-Submitted: auto-generated\r\n"));
        assert!(!raw.contains("author@example.com"));
        assert!(!raw.contains("private"));
    }
    assert!(
        finish(&db, &lease, SmtpFailureStage::Rcpt, 550)
            .await
            .is_err()
    );
    assert_eq!(notices(&db).await.len(), 2);
}
