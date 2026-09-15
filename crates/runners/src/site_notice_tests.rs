//! Mail the site sends to a person outside any list: envelope, headers,
//! signing domain and template scope.
use super::*;
use base64::Engine as _;
use listmngr_db::mail_queue::JobState;
use listmngr_db::site_notices::SiteNotice;
use listmngr_db::templates::Scope;
use listmngr_mail::templates::Placeholders;
use std::time::Duration;

async fn site_database(url: &str) -> Database {
    let db = Database::connect(url, 1)
        .await
        .unwrap()
        .with_site("Example Lists", "postmaster@lists.example.com");
    db.migrate().await.unwrap();
    db
}

fn placeholders() -> Placeholders {
    Placeholders::new()
        .set("user_email", "person@example.net".to_owned())
        .set("token", "TOKEN-VALUE".to_owned())
        .set(
            "verify_url",
            "https://lists.example.com/web/verify".to_owned(),
        )
}

async fn deliver(db: &Database, role: &MailRoleConfig, sink: &tokio::net::TcpListener) -> Vec<u8> {
    let lease = db
        .mail_queue()
        .claim(
            Queue::Out,
            "site",
            chrono::Utc::now().timestamp_millis(),
            60_000,
        )
        .await
        .unwrap()
        .expect("one outgoing job");
    let (mail, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            dkim_tests::capture(sink, "", "person@example.net"),
            deliver_one(db, role, lease.clone())
        )
    })
    .await
    .unwrap();
    assert_eq!(
        db.mail_queue().job(lease.job.id).await.unwrap().state,
        JobState::Done
    );
    mail
}

#[tokio::test]
async fn a_site_notice_leaves_with_a_null_sender_from_the_site_owner_and_no_list_coupling() {
    let db = site_database("sqlite::memory:").await;
    null_sender_matrix(&db).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_site_notice_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("site_notice")
        .await
        .unwrap();
    let db = site_database(&schema.url).await;
    null_sender_matrix(&db).await;
    db.pool().close().await;
    schema.drop().await.unwrap();
}

async fn null_sender_matrix(db: &Database) {
    db.site_notices()
        .enqueue(
            &SiteNotice {
                to: "person@example.net",
                language: "en",
                subject: "notice-site-verify-subject",
                subject_args: &[("site_name", "Example Lists")],
                template: "site:user:action:verify",
            },
            placeholders(),
            chrono::Utc::now().timestamp_millis(),
        )
        .await
        .unwrap();
    let sink = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut role = MailRoleConfig::from_core(&tests::plaintext_config()).unwrap();
    role.smtp_relay = sink.local_addr().unwrap();
    role.command_timeout = Duration::from_secs(2);
    let mail = String::from_utf8(deliver(db, &role, &sink).await).unwrap();
    assert!(
        mail.contains("From: postmaster@lists.example.com\r\n"),
        "{mail}"
    );
    assert!(mail.contains("To: person@example.net\r\n"));
    assert!(mail.contains("Subject: Confirm your email address for Example Lists\r\n"));
    assert!(mail.contains("Auto-Submitted: auto-generated\r\n"));
    assert!(
        mail.contains("@lists.example.com>\r\n"),
        "message id at the site domain"
    );
    assert!(mail.contains("TOKEN-VALUE"));
    assert!(mail.contains("https://lists.example.com/web/verify"));
    for forbidden in ["List-Post:", "List-Id:", "DKIM-Signature:", "X-BeenThere:"] {
        assert!(!mail.contains(forbidden), "{forbidden} on a site notice");
    }
    let bindings: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM message_delivery_bindings")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(bindings, 0, "no list delivery authority is bound");
    let audits: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='site.notice'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(audits, 1);
}

#[tokio::test]
async fn a_site_notice_is_signed_with_the_site_owner_domain_when_a_key_exists() {
    let dir = tempfile::tempdir().unwrap();
    let path = dkim_tests::key(dir.path());
    let db = site_database("sqlite::memory:").await;
    db.site_notices()
        .enqueue(
            &SiteNotice {
                to: "person@example.net",
                language: "en",
                subject: "notice-site-reset-subject",
                subject_args: &[("site_name", "Example Lists")],
                template: "site:user:action:reset",
            },
            placeholders().set(
                "reset_url",
                "https://lists.example.com/web/reset".to_owned(),
            ),
            chrono::Utc::now().timestamp_millis(),
        )
        .await
        .unwrap();
    let sink = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut role =
        MailRoleConfig::from_core(&dkim_tests::signing_config(&path, "lists.example.com")).unwrap();
    role.smtp_relay = sink.local_addr().unwrap();
    role.command_timeout = Duration::from_secs(2);
    let mail = String::from_utf8(deliver(&db, &role, &sink).await).unwrap();
    assert!(mail.starts_with("DKIM-Signature:"), "{mail}");
    assert!(mail.contains("d=lists.example.com;"), "{mail}");
    assert!(mail.contains("Subject: Reset your password for Example Lists\r\n"));
}

#[tokio::test]
async fn a_site_notice_renders_in_the_reader_language_and_a_site_template_wins() {
    let db = site_database("sqlite::memory:").await;
    db.templates()
        .set_body(
            &Scope::Site,
            "site:user:action:verify",
            "vi",
            "Mã của bạn: $token (site override)",
        )
        .await
        .unwrap();
    db.site_notices()
        .enqueue(
            &SiteNotice {
                to: "person@example.net",
                language: "vi-VN",
                subject: "notice-site-verify-subject",
                subject_args: &[("site_name", "Example Lists")],
                template: "site:user:action:verify",
            },
            placeholders(),
            chrono::Utc::now().timestamp_millis(),
        )
        .await
        .unwrap();
    let sink = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut role = MailRoleConfig::from_core(&tests::plaintext_config()).unwrap();
    role.smtp_relay = sink.local_addr().unwrap();
    role.command_timeout = Duration::from_secs(2);
    let mail = String::from_utf8(deliver(&db, &role, &sink).await).unwrap();
    // Vietnamese subject, RFC 2047-encoded; the body is the site override,
    // base64 because it is not ASCII.
    assert!(mail.contains("Subject: =?utf-8?"), "{mail}");
    let body = mail
        .split("\r\n\r\n")
        .nth(1)
        .unwrap_or_default()
        .replace("\r\n", "");
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(body.trim())
        .unwrap();
    let decoded = String::from_utf8(decoded).unwrap();
    assert!(
        decoded.contains("Mã của bạn: TOKEN-VALUE (site override)"),
        "{decoded}"
    );
}

#[tokio::test]
async fn an_unknown_template_or_an_unusable_owner_address_enqueues_nothing() {
    let db = site_database("sqlite::memory:").await;
    let notice = SiteNotice {
        to: "person@example.net",
        language: "en",
        subject: "notice-site-verify-subject",
        subject_args: &[],
        template: "site:user:action:missing",
    };
    assert!(
        db.site_notices()
            .enqueue(&notice, placeholders(), 0)
            .await
            .is_err()
    );
    let broken = Database::connect("sqlite::memory:", 1)
        .await
        .unwrap()
        .with_site("Broken", "not a mailbox");
    broken.migrate().await.unwrap();
    assert!(
        broken
            .site_notices()
            .enqueue(
                &SiteNotice {
                    template: "site:user:action:verify",
                    ..notice
                },
                placeholders(),
                0
            )
            .await
            .is_err()
    );
    for db in [&db, &broken] {
        let jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(jobs, 0);
    }
}
