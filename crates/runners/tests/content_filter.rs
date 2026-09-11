//! The `in` runner applies the list's content filter before fan-out and, when
//! nothing deliverable remains, the durable outcome follows `filter_action`:
//! discard, reject with a notice to the author, forward the only copy to the
//! moderators, or preserve it for the site administrator.
use listmngr_core::{Config, MemberRole, SubscriptionMode};
use listmngr_db::{
    Database, NewList, NewMember,
    mail_queue::{JobState, NewMessage, Queue},
};
use serde_json::{Value, json};
use std::time::Duration;

const LIST: &str = "filter.example.invalid";
const POSTING: &str = "filter@example.invalid";

async fn fixture(settings: Value) -> Database {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: LIST.parse().unwrap(),
            display_name: "Filter".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let mut settings = settings;
    settings["filter_content"] = json!(true);
    settings["respond_to_post_requests"] = json!(false);
    settings["admin_immed_notify"] = json!(false);
    db.lists().update(&list.id, &settings).await.unwrap();
    for (email, role) in [
        ("member@example.invalid", MemberRole::Member),
        ("owner@example.invalid", MemberRole::Owner),
        ("mod@example.invalid", MemberRole::Moderator),
    ] {
        db.members()
            .create(NewMember {
                list_id: list.id.clone(),
                email: email.into(),
                display_name: String::new(),
                role,
                subscription_mode: SubscriptionMode::AsAddress,
            })
            .await
            .unwrap();
    }
    db
}

struct Outcome {
    state: JobState,
    last_error: String,
    /// Outgoing jobs that are list posts (not generated notices).
    posts: i64,
    /// Generated notices as (recipients, subject, raw bytes).
    notices: Vec<(String, String, Vec<u8>)>,
    audit_actions: Vec<String>,
}

async fn post(db: &Database, config: Config, raw: &[u8]) -> Outcome {
    let job = db
        .mail_queue()
        .enqueue(
            NewMessage {
                raw: raw.to_vec(),
                external_id: format!("<{}@example.invalid>", uuid::Uuid::now_v7()),
                context: json!({"list_id": LIST, "envelope_sender": "member@example.invalid"})
                    .to_string(),
                queue: Queue::In,
                max_attempts: 3,
            },
            chrono::Utc::now().timestamp_millis(),
        )
        .await
        .unwrap();
    drive(db, config, job.id).await
}

/// Run the `in` processor until `job` finishes and collect what it left.
async fn drive(db: &Database, config: Config, job: listmngr_db::mail_queue::JobId) -> Outcome {
    let role = listmngr_runners::MailRoleConfig::from_core(&config).unwrap();
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let mut worker = tokio::spawn(listmngr_runners::run_in_processor(
        db.clone(),
        config,
        role,
        "filter-fixture".into(),
        receiver,
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let state = db.mail_queue().job(job).await.unwrap().state;
            if matches!(state, JobState::Done | JobState::Shunted) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("in runner did not finish the job");
    stop.send(true).unwrap();
    if tokio::time::timeout(Duration::from_secs(2), &mut worker)
        .await
        .is_err()
    {
        worker.abort();
        let _ = worker.await;
        panic!("in runner did not stop");
    }
    let finished = db.mail_queue().job(job).await.unwrap();
    let posts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs WHERE queue IN ('out','archive','digest') AND id NOT IN (SELECT job_id FROM workflow_notices)")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let mut notices = Vec::new();
    while let Some(lease) = db
        .mail_queue()
        .claim(
            Queue::Out,
            "collector",
            chrono::Utc::now().timestamp_millis() + 1_000,
            100,
        )
        .await
        .unwrap()
    {
        let message = db.mail_queue().message(lease.job.message_id).await.unwrap();
        let recipients = db
            .mail_queue()
            .pending_recipients(lease.job.id)
            .await
            .unwrap();
        let parsed = mail_parser::MessageParser::default()
            .parse(&message.raw)
            .unwrap();
        notices.push((
            recipients.join(","),
            parsed.subject().unwrap_or_default().to_owned(),
            message.raw.clone(),
        ));
    }
    notices.sort();
    let audit_actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM audit_log WHERE action LIKE 'post.%' ORDER BY at, action",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    Outcome {
        state: finished.state,
        last_error: finished.last_error,
        posts,
        notices,
        audit_actions,
    }
}

fn message_with_pdf() -> Vec<u8> {
    format!(
        "From: member@example.invalid\r\nTo: {POSTING}\r\nSubject: report\r\nMessage-ID: <{}@example.invalid>\r\nContent-Type: multipart/mixed; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nSee attached.\r\n--b\r\nContent-Type: application/pdf; name=\"r.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERi0x\r\n--b--\r\n",
        uuid::Uuid::now_v7()
    )
    .into_bytes()
}

#[tokio::test]
async fn a_filtered_attachment_is_removed_from_every_copy_before_fan_out() {
    let db = fixture(json!({"filter_types": ["application/pdf"]})).await;
    let outcome = post(&db, Config::default(), &message_with_pdf()).await;
    assert_eq!(outcome.state, JobState::Done);
    assert!(outcome.posts >= 1, "the post was delivered");
    let list = db.lists().get(&LIST.parse().unwrap()).await.unwrap();
    let raw = message_with_pdf();
    for target in [
        listmngr_mail::handlers::Target::Out,
        listmngr_mail::handlers::Target::Archive,
        listmngr_mail::handlers::Target::Digest,
    ] {
        let cooked = listmngr_mail::handlers::cook_for(target, &raw, &list, "identity").unwrap();
        let text = String::from_utf8_lossy(&cooked);
        assert!(
            !text.contains("JVBERi0x"),
            "{target:?} still carries the pdf"
        );
        assert!(text.contains("See attached."), "{target:?}");
        assert!(
            text.contains("X-Content-Filtered-By: listmngr/mime-delete"),
            "{target:?}"
        );
    }
}

#[tokio::test]
async fn discard_drops_the_post_silently_with_an_audit_trail() {
    let db =
        fixture(json!({"filter_types": ["multipart/mixed"], "filter_action": "discard"})).await;
    let outcome = post(&db, Config::default(), &message_with_pdf()).await;
    assert_eq!(outcome.state, JobState::Done);
    assert_eq!(outcome.posts, 0);
    assert!(outcome.notices.is_empty(), "{:?}", outcome.notices);
    assert_eq!(outcome.audit_actions, ["post.discard"]);
    let diff: String = sqlx::query_scalar("SELECT diff FROM audit_log WHERE action='post.discard'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(
        diff.contains("The message's content type was explicitly disallowed"),
        "{diff}"
    );
    assert!(diff.contains("mime-delete"), "{diff}");
}

#[tokio::test]
async fn reject_tells_the_author_why_in_their_language() {
    let db = fixture(json!({"filter_types": ["multipart/mixed"], "filter_action": "reject"})).await;
    let outcome = post(&db, Config::default(), &message_with_pdf()).await;
    assert_eq!(outcome.state, JobState::Done);
    assert_eq!(outcome.posts, 0);
    assert_eq!(outcome.audit_actions, ["post.reject"]);
    assert_eq!(outcome.notices.len(), 1, "{:?}", outcome.notices);
    let (to, subject, raw) = &outcome.notices[0];
    assert_eq!(to, "member@example.invalid");
    assert_eq!(subject, "Request to mailing list \"Filter\" rejected");
    let body = mail_parser::MessageParser::default()
        .parse(raw)
        .unwrap()
        .body_text(0)
        .unwrap()
        .into_owned();
    assert!(
        body.contains("The message's content type was explicitly disallowed"),
        "{body}"
    );
}

#[tokio::test]
async fn forward_sends_the_only_copy_to_the_moderators_then_drops_it() {
    let db =
        fixture(json!({"filter_types": ["multipart/mixed"], "filter_action": "forward"})).await;
    let outcome = post(&db, Config::default(), &message_with_pdf()).await;
    assert_eq!(outcome.state, JobState::Done);
    assert_eq!(outcome.posts, 0);
    assert_eq!(outcome.audit_actions, ["post.forward"]);
    assert_eq!(outcome.notices.len(), 1, "{:?}", outcome.notices);
    let (to, subject, raw) = &outcome.notices[0];
    assert_eq!(
        to, "mod@example.invalid",
        "Mailman's roster for this notice is the moderators"
    );
    assert_eq!(subject, "Content filter message notification");
    let parsed = mail_parser::MessageParser::default().parse(raw).unwrap();
    let body = parsed.body_text(0).unwrap();
    assert!(
        body.contains("matched the Filter mailing list's content"),
        "{body}"
    );
    assert!(body.contains("only remaining copy"), "{body}");
    let attached = parsed
        .parts
        .iter()
        .find_map(|part| match &part.body {
            mail_parser::PartType::Message(inner) => Some(inner),
            _ => None,
        })
        .expect("the original is attached as message/rfc822");
    assert_eq!(attached.subject(), Some("report"));
    assert!(
        String::from_utf8_lossy(raw).contains("JVBERi0x"),
        "the attached original is unfiltered"
    );
}

#[tokio::test]
async fn forward_falls_back_to_the_owners_when_there_are_no_moderators() {
    let db =
        fixture(json!({"filter_types": ["multipart/mixed"], "filter_action": "forward"})).await;
    let moderator = db
        .members()
        .roster(&LIST.parse().unwrap(), MemberRole::Moderator)
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    db.members().delete(moderator.id).await.unwrap();
    let outcome = post(&db, Config::default(), &message_with_pdf()).await;
    assert_eq!(outcome.notices.len(), 1, "{:?}", outcome.notices);
    assert_eq!(outcome.notices[0].0, "owner@example.invalid");
}

#[tokio::test]
async fn preserve_is_a_discard_unless_the_site_keeps_filtered_messages() {
    let db =
        fixture(json!({"filter_types": ["multipart/mixed"], "filter_action": "preserve"})).await;
    let outcome = post(&db, Config::default(), &message_with_pdf()).await;
    assert_eq!(
        outcome.state,
        JobState::Done,
        "not preservable by default, as in Mailman"
    );
    assert_eq!(outcome.posts, 0);
    assert_eq!(outcome.audit_actions, ["post.discard"]);

    let db =
        fixture(json!({"filter_types": ["multipart/mixed"], "filter_action": "preserve"})).await;
    let mut config = Config::default();
    config.mailman.filtered_messages_are_preservable = true;
    let outcome = post(&db, config, &message_with_pdf()).await;
    assert_eq!(
        outcome.state,
        JobState::Shunted,
        "the copy waits in the shunt store"
    );
    assert!(
        outcome.last_error.contains("content filter"),
        "{}",
        outcome.last_error
    );
    assert_eq!(outcome.posts, 0);
    assert!(outcome.notices.is_empty());
    assert_eq!(outcome.audit_actions, ["post.preserve"]);
}

#[tokio::test]
async fn a_chain_rejection_tells_the_author_why_and_a_chain_discard_is_silent() {
    let db = fixture(
        json!({"reject_these_nonmembers": ["^outsider@"], "discard_these_nonmembers": ["^junk@"]}),
    )
    .await;
    for (sender, expected_audit, notices) in [
        ("outsider@example.invalid", "post.reject", 1),
        ("junk@example.invalid", "post.discard", 0),
    ] {
        let raw = format!(
            "From: {sender}\r\nTo: {POSTING}\r\nSubject: hello\r\nMessage-ID: <{}@example.invalid>\r\n\r\nplain\r\n",
            uuid::Uuid::now_v7()
        );
        let job = db
            .mail_queue()
            .enqueue(
                NewMessage {
                    raw: raw.into_bytes(),
                    external_id: format!("<{}@example.invalid>", uuid::Uuid::now_v7()),
                    context: json!({"list_id": LIST, "envelope_sender": sender}).to_string(),
                    queue: Queue::In,
                    max_attempts: 3,
                },
                chrono::Utc::now().timestamp_millis(),
            )
            .await
            .unwrap();
        let outcome = drive(&db, Config::default(), job.id).await;
        assert_eq!(outcome.state, JobState::Done, "{sender}");
        assert_eq!(outcome.posts, 0, "{sender}");
        assert!(
            outcome.audit_actions.contains(&expected_audit.to_owned()),
            "{sender}: {:?}",
            outcome.audit_actions
        );
        assert_eq!(
            outcome.notices.len(),
            notices,
            "{sender}: {:?}",
            outcome.notices
        );
        if notices == 1 {
            let (to, subject, raw) = &outcome.notices[0];
            assert_eq!(to, sender);
            assert_eq!(subject, "Request to mailing list \"Filter\" rejected");
            let body = mail_parser::MessageParser::default()
                .parse(raw)
                .unwrap()
                .body_text(0)
                .unwrap()
                .into_owned();
            assert!(body.contains("moderation policy"), "{body}");
        }
    }
}
