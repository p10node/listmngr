//! `validate-authenticity` in the `in` runner: the verdict reaches every
//! copy as `Authentication-Results`, and `dmarc-mitigation` follows the
//! From domain's published policy — munging only when it is restrictive,
//! refusing when the list's action says so.
use base64::Engine;
use listmngr_core::{Config, MemberRole, SubscriptionMode};
use listmngr_db::{
    Database, NewList, NewMember,
    mail_queue::{JobState, NewMessage, Queue},
};
use listmngr_mail::authenticity::{TxtCache, Verifier};
use listmngr_mail::dkim::SigningKeys;
use mail_auth::common::parse::TxtRecordParser;
use mail_auth::hickory_resolver::proto::op::ResponseCode;
use mail_auth::{DnsError, Error, Txt};
use mail_parser::MimeHeaders;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

const LIST: &str = "auth.example.invalid";
const POSTING: &str = "auth@example.invalid";
const DOMAIN: &str = "sender.invalid";

struct Fixture {
    db: Database,
    key: std::path::PathBuf,
    record: String,
    _dir: tempfile::TempDir,
}

async fn setup(settings: Value) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("owned-test.pem");
    assert!(
        std::process::Command::new("openssl")
            .args([
                "genpkey",
                "-algorithm",
                "RSA",
                "-pkeyopt",
                "rsa_keygen_bits:2048",
                "-out"
            ])
            .arg(&key)
            .output()
            .unwrap()
            .status
            .success()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let public = std::process::Command::new("openssl")
        .args(["pkey", "-in"])
        .arg(&key)
        .args(["-pubout", "-outform", "DER"])
        .output()
        .unwrap();
    let record = format!(
        "v=DKIM1; k=rsa; p={}",
        base64::engine::general_purpose::STANDARD.encode(public.stdout)
    );
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
            display_name: "Auth".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let mut settings = settings;
    settings["respond_to_post_requests"] = json!(false);
    settings["admin_immed_notify"] = json!(false);
    settings["default_nonmember_action"] = json!("accept");
    db.lists().update(&list.id, &settings).await.unwrap();
    db.members()
        .create(NewMember {
            list_id: list.id,
            email: "reader@example.invalid".into(),
            display_name: String::new(),
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsAddress,
        })
        .await
        .unwrap();
    Fixture {
        db,
        key,
        record,
        _dir: dir,
    }
}

const fn not_found() -> Txt {
    Txt::Error(Error::Dns(DnsError::RecordNotFound(ResponseCode::NXDomain)))
}

fn verifier(record: &str, policy: Option<&str>) -> Arc<Verifier> {
    let cache = TxtCache::default();
    cache.seed(
        &format!("fixture._domainkey.{DOMAIN}."),
        Txt::DomainKey(Arc::new(
            mail_auth::common::verify::DomainKey::parse(record.as_bytes()).unwrap(),
        )),
    );
    cache.seed(
        &format!("{DOMAIN}."),
        Txt::Spf(Arc::new(
            mail_auth::spf::Spf::parse(b"v=spf1 ip4:192.0.2.25 -all").unwrap(),
        )),
    );
    if let Some(policy) = policy {
        cache.seed(
            &format!("_dmarc.{DOMAIN}."),
            Txt::Dmarc(Arc::new(
                mail_auth::dmarc::Dmarc::parse(policy.as_bytes()).unwrap(),
            )),
        );
    } else {
        cache.seed(&format!("_dmarc.{DOMAIN}."), not_found());
        cache.seed("_dmarc.invalid.", not_found());
    }
    Arc::new(
        Verifier::system("mx.example.invalid")
            .unwrap()
            .with_txt_cache(cache),
    )
}

fn signed(key: &std::path::Path) -> Vec<u8> {
    let keys = SigningKeys::load(&[listmngr_core::DkimSigningConfig {
        domain: DOMAIN.into(),
        selector: "fixture".into(),
        private_key_file: key.to_path_buf(),
    }])
    .unwrap();
    let raw = format!(
        "Received: from mail.{DOMAIN} (mail.{DOMAIN} [192.0.2.25])\r\n\tby mx.example.invalid (Postfix) with ESMTPS id X\r\n\tfor <{POSTING}>; Mon, 1 Sep 2026 10:00:00 +0000\r\nFrom: Alice <alice@{DOMAIN}>\r\nTo: {POSTING}\r\nSubject: signed post\r\nMessage-ID: <{}@{DOMAIN}>\r\nDate: Mon, 1 Sep 2026 10:00:00 +0000\r\n\r\nhello list\r\n",
        uuid::Uuid::now_v7()
    );
    keys.sign(DOMAIN, raw.into_bytes()).unwrap()
}

/// Post through the `in` runner with `verifier` and return the job state,
/// the stored context, and the subscriber copy when one was scheduled.
async fn post(
    db: &Database,
    verifier: Option<Arc<Verifier>>,
    raw: &[u8],
) -> (JobState, Value, Option<Vec<u8>>, Vec<String>) {
    let job = db
        .mail_queue()
        .enqueue(
            NewMessage {
                raw: raw.to_vec(),
                external_id: format!("<{}@example.invalid>", uuid::Uuid::now_v7()),
                context: json!({"list_id": LIST, "envelope_sender": format!("alice@{DOMAIN}")})
                    .to_string(),
                queue: Queue::In,
                max_attempts: 3,
            },
            chrono::Utc::now().timestamp_millis(),
        )
        .await
        .unwrap();
    let config = Config::default();
    let mut role = listmngr_runners::MailRoleConfig::from_core(&config).unwrap();
    role.authenticity = verifier;
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let mut worker = tokio::spawn(listmngr_runners::run_in_processor(
        db.clone(),
        config,
        role,
        "auth-fixture".into(),
        receiver,
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        while db.mail_queue().job(job.id).await.unwrap().state != JobState::Done {
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
    let stored = db.mail_queue().message(job.message_id).await.unwrap();
    let context: Value = serde_json::from_str(&stored.context).unwrap();
    let posts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs WHERE queue='out' AND id NOT IN (SELECT job_id FROM workflow_notices)")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let copy = if posts > 0 {
        Some(
            listmngr_runners::prepare_individual(
                db,
                &stored.raw,
                &stored.context,
                uuid::Uuid::now_v7(),
            )
            .await
            .ok()
            .unwrap()
            .0,
        )
    } else {
        None
    };
    let audit: Vec<String> =
        sqlx::query_scalar("SELECT action FROM audit_log WHERE action LIKE 'post.%' ORDER BY at")
            .fetch_all(db.pool())
            .await
            .unwrap();
    (
        db.mail_queue().job(job.id).await.unwrap().state,
        context,
        copy,
        audit,
    )
}

#[tokio::test]
async fn a_restrictive_policy_munges_conditionally_and_the_results_travel_with_the_post() {
    let fixture = setup(json!({"dmarc_mitigate_action": "munge_from"})).await;
    let raw = signed(&fixture.key);
    let (state, context, copy, _) = post(
        &fixture.db,
        Some(verifier(&fixture.record, Some("v=DMARC1; p=reject"))),
        &raw,
    )
    .await;
    assert_eq!(state, JobState::Done);
    assert_eq!(context["dmarc_mitigate"], true);
    let results = context["authentication_results"].as_str().unwrap();
    assert!(results.starts_with("mx.example.invalid;"), "{results}");
    assert!(results.contains("dkim=pass"), "{results}");
    let copy = copy.expect("delivered");
    let from = listmngr_mail::header_value(&copy, "From").unwrap();
    assert!(
        from.contains("via auth@example.invalid"),
        "conditional munging applied: {from}"
    );
    let header = listmngr_mail::header_value(&copy, "Authentication-Results").unwrap();
    assert!(header.contains("dmarc=pass"), "{header}");
    assert!(header.contains("spf=pass"), "{header}");
}

/// `wrap_message`: the same conditional decision, but the subscriber copy
/// is a list-addressed message carrying the signed post whole, and a domain
/// without a policy is delivered as it came.
#[tokio::test]
async fn a_restrictive_policy_wraps_the_delivered_copy_when_the_list_wraps() {
    let fixture = setup(json!({
        "dmarc_mitigate_action": "wrap_message",
        "dmarc_wrapped_message_text": "The original post is attached."
    }))
    .await;
    let raw = signed(&fixture.key);
    let (state, context, copy, _) = post(
        &fixture.db,
        Some(verifier(&fixture.record, Some("v=DMARC1; p=quarantine"))),
        &raw,
    )
    .await;
    assert_eq!(state, JobState::Done);
    assert_eq!(context["dmarc_mitigate"], true);
    let copy = copy.expect("delivered");
    let outer = mail_parser::MessageParser::default().parse(&copy).unwrap();
    let from = outer.from().unwrap().first().unwrap();
    assert_eq!(from.address(), Some(POSTING));
    assert!(from.name().unwrap().contains("Alice"), "{from:?}");
    assert!(
        outer
            .content_type()
            .is_some_and(|c| c.ctype() == "multipart" && c.subtype() == Some("mixed")),
        "{copy:?}"
    );
    let inner = outer
        .parts
        .iter()
        .find_map(|part| match &part.body {
            mail_parser::PartType::Message(inner) => Some(inner),
            _ => None,
        })
        .expect("the post inside");
    assert_eq!(
        inner.from().unwrap().first().unwrap().address(),
        Some(format!("alice@{DOMAIN}").as_str())
    );
    // `cleanse-dkim` dropped the author's signature before the wrapper was
    // made, as it does for every copy; the post itself is inside whole.
    assert_eq!(inner.subject(), Some("[auth] signed post"));
    assert!(inner.body_text(0).unwrap().contains("hello list"));
    assert!(
        outer
            .parts
            .iter()
            .any(|part| matches!(&part.body, mail_parser::PartType::Text(text) if text.contains("The original post is attached."))),
        "{copy:?}"
    );
    // The stored post — what the archive and digest cook from — is untouched.
    let stored: Vec<u8> = sqlx::query_scalar("SELECT raw FROM message_blobs LIMIT 1")
        .fetch_one(fixture.db.pool())
        .await
        .unwrap();
    assert_eq!(stored, raw);
    // No policy: no wrapper.
    let fixture = setup(json!({"dmarc_mitigate_action": "wrap_message"})).await;
    let raw = signed(&fixture.key);
    let (_, context, copy, _) =
        post(&fixture.db, Some(verifier(&fixture.record, None)), &raw).await;
    assert!(context.get("dmarc_mitigate").is_none());
    let copy = copy.expect("delivered");
    assert_eq!(
        listmngr_mail::header_value(&copy, "From").as_deref(),
        Some(format!("Alice <alice@{DOMAIN}>").as_str())
    );
}

#[tokio::test]
async fn a_domain_without_a_policy_is_delivered_unmunged_with_its_results() {
    let fixture = setup(json!({"dmarc_mitigate_action": "munge_from"})).await;
    let raw = signed(&fixture.key);
    let (state, context, copy, _) =
        post(&fixture.db, Some(verifier(&fixture.record, None)), &raw).await;
    assert_eq!(state, JobState::Done);
    assert!(context.get("dmarc_mitigate").is_none());
    let copy = copy.expect("delivered");
    let from = listmngr_mail::header_value(&copy, "From").unwrap();
    assert_eq!(from, format!("Alice <alice@{DOMAIN}>"));
    let header = listmngr_mail::header_value(&copy, "Authentication-Results").unwrap();
    assert!(header.contains("dmarc=none"), "{header}");
}

#[tokio::test]
async fn reject_and_discard_actions_refuse_posts_from_restrictive_domains() {
    let fixture = setup(json!({"dmarc_mitigate_action": "reject", "dmarc_moderation_notice": "Post through the web form instead."})).await;
    let raw = signed(&fixture.key);
    let (state, _, copy, audit) = post(
        &fixture.db,
        Some(verifier(&fixture.record, Some("v=DMARC1; p=quarantine"))),
        &raw,
    )
    .await;
    assert_eq!(state, JobState::Done);
    assert!(copy.is_none(), "not delivered");
    assert_eq!(audit, ["post.reject"]);
    let diff: String = sqlx::query_scalar("SELECT diff FROM audit_log WHERE action='post.reject'")
        .fetch_one(fixture.db.pool())
        .await
        .unwrap();
    assert!(
        diff.contains("Post through the web form instead."),
        "{diff}"
    );
    let notices: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workflow_notices")
        .fetch_one(fixture.db.pool())
        .await
        .unwrap();
    assert_eq!(notices, 1, "the author is told");

    let fixture = setup(json!({"dmarc_mitigate_action": "discard"})).await;
    let raw = signed(&fixture.key);
    let (_, _, copy, audit) = post(
        &fixture.db,
        Some(verifier(&fixture.record, Some("v=DMARC1; p=reject"))),
        &raw,
    )
    .await;
    assert!(copy.is_none());
    assert_eq!(audit, ["post.discard"]);

    // The same list delivers a domain that publishes no restrictive policy.
    let fixture = setup(json!({"dmarc_mitigate_action": "reject"})).await;
    let raw = signed(&fixture.key);
    let (_, _, copy, audit) = post(&fixture.db, Some(verifier(&fixture.record, None)), &raw).await;
    assert!(copy.is_some());
    assert!(audit.is_empty());
}

#[tokio::test]
async fn without_authenticity_checks_only_unconditional_lists_munge() {
    let fixture = setup(json!({"dmarc_mitigate_action": "munge_from"})).await;
    let raw = signed(&fixture.key);
    let (_, context, copy, _) = post(&fixture.db, None, &raw).await;
    assert!(context.get("authentication_results").is_none());
    let copy = copy.expect("delivered");
    assert!(listmngr_mail::header_value(&copy, "Authentication-Results").is_none());
    assert_eq!(
        listmngr_mail::header_value(&copy, "From").unwrap(),
        format!("Alice <alice@{DOMAIN}>")
    );

    let fixture = setup(
        json!({"dmarc_mitigate_action": "munge_from", "dmarc_mitigate_unconditionally": true}),
    )
    .await;
    let raw = signed(&fixture.key);
    let (_, _, copy, _) = post(&fixture.db, None, &raw).await;
    assert!(
        listmngr_mail::header_value(&copy.unwrap(), "From")
            .unwrap()
            .contains("via auth@example.invalid")
    );
}
