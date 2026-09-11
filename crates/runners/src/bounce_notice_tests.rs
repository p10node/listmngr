use super::*;
use listmngr_core::{MemberRole, SmtpFailure, SmtpFailureStage, SubscriptionMode};
use listmngr_db::mail_queue::RecipientOutcome;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

async fn notice_fixture() -> (Database, Lease, MailRoleConfig, tokio::net::TcpListener) {
    let (db, lease, role, sink) = tests::fixture_at(
        "sqlite::memory:",
        "{\"list_id\":\"test.example.invalid\"}",
        b"From: author@example.invalid\r\n\r\nSECRET POST",
        vec!["member@example.invalid".into()],
    )
    .await;
    db.lists()
        .update(
            &"test.example.invalid".parse().unwrap(),
            &serde_json::json!({"process_bounces":true,"bounce_score_threshold":1}),
        )
        .await
        .unwrap();
    for (email, role) in [
        ("member@example.invalid", MemberRole::Member),
        ("Admin@Example.invalid", MemberRole::Owner),
        ("Admin@Example.invalid", MemberRole::Member),
    ] {
        db.members()
            .create(listmngr_db::NewMember {
                list_id: "test.example.invalid".parse().unwrap(),
                email: email.into(),
                display_name: String::new(),
                role,
                subscription_mode: SubscriptionMode::AsAddress,
            })
            .await
            .unwrap();
    }
    let now = chrono::Utc::now().timestamp_millis();
    db.mail_queue()
        .begin_delivery(&lease, now, &["member@example.invalid".into()])
        .await
        .unwrap();
    db.mail_queue()
        .finish_delivery_with_smtp(
            &lease,
            now,
            &[(
                "member@example.invalid".into(),
                RecipientOutcome::Failed,
                "SECRET DIAGNOSTIC".into(),
            )],
            0,
            &[(
                "member@example.invalid".into(),
                SmtpFailure {
                    stage: SmtpFailureStage::Rcpt,
                    code: 550,
                },
            )],
        )
        .await
        .unwrap();
    let notice = db
        .mail_queue()
        .claim(
            Queue::Out,
            "notice",
            chrono::Utc::now().timestamp_millis(),
            30000,
        )
        .await
        .unwrap()
        .unwrap();
    (db, notice, role, sink)
}

async fn peer(sink: &tokio::net::TcpListener, fail: bool) -> String {
    let (stream, _) = sink.accept().await.unwrap();
    let (read, mut write) = stream.into_split();
    let mut read = BufReader::new(read);
    write.write_all(b"220 fixture\r\n").await.unwrap();
    for (prefix, response) in [
        ("EHLO", "250 ok\r\n"),
        ("MAIL FROM:<>", "250 ok\r\n"),
        (
            "RCPT TO:<Admin@Example.invalid>",
            if fail {
                "550 private fixture\r\n"
            } else {
                "250 ok\r\n"
            },
        ),
    ] {
        let mut line = String::new();
        assert!(read.read_line(&mut line).await.unwrap() > 0);
        assert!(line.starts_with(prefix), "{line}");
        write.write_all(response.as_bytes()).await.unwrap();
    }
    if fail {
        return String::new();
    }
    let mut line = String::new();
    read.read_line(&mut line).await.unwrap();
    assert_eq!(line, "DATA\r\n");
    write.write_all(b"354 go\r\n").await.unwrap();
    let mut raw = String::new();
    loop {
        line.clear();
        assert!(read.read_line(&mut line).await.unwrap() > 0);
        if line == ".\r\n" {
            break;
        }
        raw.push_str(&line);
    }
    write.write_all(b"250 accepted\r\n").await.unwrap();
    raw
}

#[tokio::test]
async fn disable_notice_safe_null_envelope_and_permanent_failure_never_scores() {
    for fail in [false, true] {
        let (db, notice, role, sink) = notice_fixture().await;
        let id = notice.job.id;
        let (raw, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(peer(&sink, fail), deliver_one(&db, &role, notice))
        })
        .await
        .unwrap();
        if !fail {
            assert!(raw.len() <= 4096);
            assert!(raw.contains("Auto-Submitted: auto-generated\r\n"));
            assert!(raw.contains(
                "member@example.invalid's subscription has been disabled on test@example.invalid"
            ));
            assert!(!raw.contains("SECRET"));
            assert!(!raw.contains("List-Post:"));
        }
        let counts: (i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM bounce_events),(SELECT COUNT(*) FROM workflow_notices)",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(counts, (1, 1));
        let status:String=sqlx::query_scalar("SELECT p.delivery_status FROM preferences p JOIN members m ON m.preferences_id=p.id JOIN addresses a ON a.id=m.address_id WHERE a.email='member@example.invalid'").fetch_one(db.pool()).await.unwrap();
        assert_eq!(status, "by_bounces");
        let score:f64=sqlx::query_scalar("SELECT bounce_score FROM members m JOIN addresses a ON a.id=m.address_id WHERE a.email='admin@example.invalid' AND m.role='member'").fetch_one(db.pool()).await.unwrap();
        assert!(score.abs() < f64::EPSILON);
        assert_eq!(
            db.mail_queue().job(id).await.unwrap().state,
            listmngr_db::mail_queue::JobState::Done
        );
    }
}
