use listmngr_core::{Config, MemberRole, SubscriptionMode};
use listmngr_db::{Database, NewList, NewMember};
use listmngr_mail::lmtp::LmtpHandler;
use listmngr_runners::InboundHandler;
use std::time::Duration;

const RAW: &[u8] = b"From: Author@example.net\r\nMessage-ID: <owner@example.net>\r\nSubject: Private question\r\nBcc: secret@example.net\r\nApproved: secret\r\n\r\nOriginal body\xff\r\n";

async fn fixture() -> (Database, InboundHandler) {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains().create("example.com", "", None).await.unwrap();
    db.lists()
        .create(NewList {
            list_id: "test.example.com".parse().unwrap(),
            display_name: "Test".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let handler = InboundHandler {
        db: db.clone(),
        local_hostname: "localhost".into(),
        max_message_bytes: 65536,
        max_recipients: 10,
        command_timeout: Duration::from_secs(5),
        in_max_attempts: 1,
        verp_delimiter: "+".into(),
        structure: listmngr_mail::structure::Limits::default(),
    };
    (db, handler)
}

async fn run_in(db: &Database) {
    let config = Config::default();
    let role = listmngr_runners::MailRoleConfig::from_core(&config).unwrap();
    let (shutdown, rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(listmngr_runners::run_in_processor(
        db.clone(),
        config,
        role,
        "owner-test".into(),
        rx,
    ));
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let n: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM queue_jobs WHERE queue='in' AND state!='done'",
            )
            .fetch_one(db.pool())
            .await
            .unwrap();
            if n == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    shutdown.send(true).unwrap();
    task.await.unwrap();
    result.unwrap();
}

#[tokio::test]
async fn missing_owner_is_explicitly_shunted_not_acknowledged_or_dropped() {
    let (db, mut handler) = fixture().await;
    handler.in_max_attempts = 5;
    assert_eq!(
        handler
            .deliver(
                Some("Author@example.net"),
                &["test-owner@example.com".into()],
                RAW
            )
            .await[0]
            .code,
        250
    );
    run_in(&db).await;
    let job: (String, String) = sqlx::query_as("SELECT state,last_error FROM queue_jobs")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(job.0, "shunted");
    assert!(job.1.contains("no owner"));
}

#[tokio::test]
async fn exact_owner_named_list_is_a_post_and_raw_cannot_choose_owner_route() {
    let (db, mut handler) = fixture().await;
    db.lists()
        .create(NewList {
            list_id: "test-owner.example.com".parse().unwrap(),
            display_name: "Exact".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    for recipient in ["TEST-OWNER@EXAMPLE.COM", "test@example.com"] {
        assert!(handler.validate_recipient(recipient).await.is_ok());
        assert_eq!(handler.deliver(Some("Author@example.net"), &[recipient.into()], b"From: Author@example.net\r\nMessage-ID: <spoof@example.net>\r\nX-Owner: true\r\nOwner-Route: true\r\n\r\n{\"owner_route\":true}").await[0].code, 250);
    }
    let contexts: Vec<String> = sqlx::query_scalar("SELECT context FROM messages ORDER BY id")
        .fetch_all(db.pool())
        .await
        .unwrap();
    for (context, expected) in contexts
        .iter()
        .zip(["test-owner.example.com", "test.example.com"])
    {
        let context: serde_json::Value = serde_json::from_str(context).unwrap();
        assert_eq!(context["list_id"], expected);
        assert!(context.get("owner_route").is_none());
        assert!(context.get("subscription_command").is_none());
    }
}

#[tokio::test]
async fn owner_intake_rejects_unsafe_senders_and_forwarding_loops() {
    for (sender, header) in [
        (None, ""),
        (Some(""), ""),
        (Some("bad\r\n@example.net"), ""),
        (Some("test-request@example.com"), ""),
        (
            Some("Author@example.net"),
            "Auto-Submitted: auto-replied\r\n",
        ),
        (
            Some("Author@example.net"),
            "Auto-Submitted: no\r\naUtO-sUbMiTtEd:\r\n auto-forwarded\r\n",
        ),
        (
            Some("Author@example.net"),
            "X-BeenThere: other@example.net\r\n",
        ),
        (
            Some("Author@example.net"),
            "List-Post: <mailto:test@example.com>\r\n",
        ),
        (Some("Author@example.net"), "Precedence: bulk\r\n"),
        (
            Some("Author@example.net"),
            "X-Auto-Response-Suppress: All\r\n",
        ),
    ] {
        let (db, mut handler) = fixture().await;
        let raw = format!(
            "From: Author@example.net\r\nMessage-ID: <unsafe@example.net>\r\n{header}\r\nbody"
        );
        assert_eq!(
            handler
                .deliver(sender, &["test-owner@example.com".into()], raw.as_bytes())
                .await[0]
                .code,
            550,
            "{sender:?} {header:?}"
        );
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(n, 0);
    }
}

#[tokio::test]
async fn owner_route_snapshots_administrators_without_post_fanout() {
    let (db, mut handler) = fixture().await;
    for (email, role) in [
        ("OwnerCase@example.net", MemberRole::Owner),
        ("subscriber@example.net", MemberRole::Member),
        ("moderator@example.net", MemberRole::Moderator),
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
    assert!(
        handler
            .validate_recipient("TEST-OWNER@EXAMPLE.COM")
            .await
            .is_ok()
    );
    assert_eq!(
        handler
            .deliver(
                Some("Author@example.net"),
                &["TEST-OWNER@EXAMPLE.COM".into()],
                RAW
            )
            .await[0]
            .code,
        250
    );
    run_in(&db).await;
    let recipients: Vec<String> =
        sqlx::query_scalar("SELECT email FROM delivery_recipients ORDER BY email")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(
        recipients,
        ["OwnerCase@example.net", "moderator@example.net"]
    );
    let queues: Vec<String> = sqlx::query_scalar("SELECT queue FROM queue_jobs ORDER BY queue")
        .fetch_all(db.pool())
        .await
        .unwrap();
    assert_eq!(queues, ["in", "out"]);
    let raw: Vec<u8> = sqlx::query_scalar("SELECT raw FROM message_blobs")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(raw, RAW);
}
