//! Independent acceptance of PLAN §4.6 through real intake and the in runner.
use listmngr_core::{Config, MemberRole, SubscriptionMode};
use listmngr_db::{Database, NewList, NewMember};
use listmngr_mail::lmtp::LmtpHandler;
use listmngr_runners::InboundHandler;
use std::time::Duration;

#[test]
fn owner_loop_guard_uses_the_same_domain_identity_as_lmtp() {
    let list = "test.example.com".parse().unwrap();
    for address in [
        "TEST-owner@EXAMPLE.COM",
        "test-owner@example.com.",
        "test+extension@example.com.",
    ] {
        assert!(listmngr_mail::owner::safe_mailbox(address));
        assert!(
            listmngr_mail::owner::points_to_list(address, &list),
            "LMTP normalizes the domain of {address}; the loop guard must agree"
        );
    }
    assert!(!listmngr_mail::owner::points_to_list(
        "test-owner@elsewhere.example",
        &list
    ));
    assert!(!listmngr_mail::owner::points_to_list(
        "different-owner@example.com",
        &list
    ));
}

#[test]
fn owner_transport_mailbox_rejects_domain_whitespace_not_just_invalid_identity() {
    assert!(listmngr_mail::owner::safe_mailbox(
        "Valid+tag@outside.example"
    ));
    assert!(!listmngr_mail::owner::safe_mailbox(
        "valid@ outside.example"
    ));
}

async fn fixture() -> (Database, InboundHandler) {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains().create("example.com", "", None).await.unwrap();
    let list_id = "test.example.com".parse().unwrap();
    db.lists()
        .create(NewList {
            list_id,
            display_name: "Owner contract".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    for (email, role) in [
        ("Owner@outside.example", MemberRole::Owner),
        ("Moderator@outside.example", MemberRole::Moderator),
        ("Member@outside.example", MemberRole::Member),
        ("Owner@outside.example", MemberRole::Moderator),
    ] {
        db.members()
            .create(NewMember {
                list_id: "test.example.com".parse().unwrap(),
                email: email.into(),
                display_name: String::new(),
                role,
                subscription_mode: SubscriptionMode::AsUser,
            })
            .await
            .unwrap();
    }
    let handler = InboundHandler {
        db: db.clone(),
        local_hostname: "localhost".into(),
        max_message_bytes: 65_536,
        max_recipients: 10,
        command_timeout: Duration::from_secs(5),
        in_max_attempts: 5,
    };
    (db, handler)
}

#[tokio::test]
async fn owner_route_snapshots_owners_and_moderators_without_post_fanout() {
    let (db, mut handler) = fixture().await;
    let recipient = "test-owner@example.com";
    assert!(handler.validate_recipient(recipient).await.is_ok());
    let raw = b"From: Reporter@outside.example\r\nTo: test-owner@example.com\r\nMessage-ID: <owner-plan@outside.example>\r\nSubject: Private administration question\r\n\r\nNot a subscriber post.\r\n";
    assert_eq!(
        handler
            .deliver(Some("Reporter@outside.example"), &[recipient.into()], raw)
            .await[0]
            .code,
        250
    );
    let config = Config::default();
    let role = listmngr_runners::MailRoleConfig::from_core(&config).unwrap();
    let (shutdown, receiver) = tokio::sync::watch::channel(false);
    let mut task = tokio::spawn(listmngr_runners::run_in_processor(
        db.clone(),
        config,
        role,
        "owner-plan".into(),
        receiver,
    ));
    let completed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM queue_jobs WHERE queue='in' AND state='done'",
            )
            .fetch_one(db.pool())
            .await
            .unwrap();
            if count == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    shutdown.send(true).unwrap();
    if let Ok(result) = tokio::time::timeout(Duration::from_secs(2), &mut task).await {
        result.unwrap();
    } else {
        task.abort();
        let _ = task.await;
        panic!("in runner did not stop");
    }
    completed.expect("owner intake must complete its durable handoff");
    let recipients: Vec<String> =
        sqlx::query_scalar("SELECT email FROM delivery_recipients ORDER BY email")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(
        recipients,
        ["Moderator@outside.example", "Owner@outside.example"]
    );
    let jobs: Vec<String> = sqlx::query_scalar("SELECT queue FROM queue_jobs ORDER BY queue")
        .fetch_all(db.pool())
        .await
        .unwrap();
    assert_eq!(jobs, ["in", "out"], "owner mail must not archive or digest");
    let retained: Vec<u8> = sqlx::query_scalar(
        "SELECT b.raw FROM message_blobs b JOIN messages m ON m.store_key=b.store_key JOIN queue_jobs q ON q.message_id=m.id WHERE q.queue='in'",
    ).fetch_one(db.pool()).await.unwrap();
    assert_eq!(retained, raw, "intake retains the original message");
}
