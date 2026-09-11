use listmngr_core::{
    Config, DeliveryMode, DeliveryStatus, ListId, MemberRole, Preferences, SubscriptionMode,
};
use listmngr_db::{Database, NewList, NewMember};
use listmngr_mail::lmtp::LmtpHandler;
use listmngr_runners::{InboundHandler, MailRoleConfig, run_in_processor, run_out_processor};
use std::time::Duration;
#[path = "support/dkim_capture.rs"]
mod dkim_capture;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    sync::watch,
};

async fn sink(listener: TcpListener, count: usize) -> Vec<(String, Vec<u8>)> {
    let mut received = Vec::new();
    for _ in 0..count {
        let (stream, _) = listener.accept().await.unwrap();
        let (r, mut w) = stream.into_split();
        let mut r = BufReader::new(r);
        w.write_all(b"220 sink\r\n").await.unwrap();
        let mut recipient = String::new();
        let mut raw = Vec::new();
        loop {
            let mut line = String::new();
            assert!(r.read_line(&mut line).await.unwrap() > 0);
            if line.starts_with("RCPT TO:") {
                recipient.push_str(&line);
            }
            if line == "DATA\r\n" {
                w.write_all(b"354 go\r\n").await.unwrap();
                loop {
                    let mut line = Vec::new();
                    assert!(r.read_until(b'\n', &mut line).await.unwrap() > 0);
                    if line == b".\r\n" {
                        break;
                    }
                    raw.extend_from_slice(if line.starts_with(b"..") {
                        &line[1..]
                    } else {
                        &line
                    });
                }
                w.write_all(b"250 accepted\r\n").await.unwrap();
                break;
            }
            w.write_all(b"250 ok\r\n").await.unwrap();
        }
        received.push((recipient, raw));
    }
    received
}
#[tokio::test]
async fn accepted_post_routes_mixed_modes_to_real_smtp_after_restart_without_regular_duplicate() {
    mixed_modes(false, false).await;
}
#[tokio::test]
async fn munge_only_rewrites_regular_delivery_not_digest_authors() {
    mixed_modes(true, false).await;
}
#[tokio::test]
async fn dkim_signs_regular_and_all_producer_digest_modes() {
    mixed_modes(true, true).await;
}
async fn mixed_modes(munge: bool, signing: bool) {
    tokio::time::timeout(Duration::from_secs(15), async {
    let owned_db = tempfile::tempdir().unwrap();
    let dir = owned_db.path().to_path_buf();
    let url=format!("sqlite://{}?mode=rwc",dir.join("runner.db").display());
    let db=Database::connect(&url,1).await.unwrap();db.migrate().await.unwrap();
    db.domains().create("example.invalid","",None).await.unwrap();
    let list:ListId="test.example.invalid".parse().unwrap();
    db.lists().create(NewList{list_id:list.clone(),display_name:"Test".into(),style:"legacy-default".into()}).await.unwrap();
    db.lists().update(&list,&if munge { serde_json::json!({"dmarc_mitigate_action":"munge_from","dmarc_mitigate_unconditionally":true}) } else {serde_json::json!({"anonymous_list":true})}).await.unwrap();
    for (name,mode,status,own,copy) in [
        ("regular",DeliveryMode::Regular,DeliveryStatus::Enabled,true,true),
        ("PlainCase",DeliveryMode::PlaintextDigests,DeliveryStatus::Enabled,true,true),
        ("MimeCase",DeliveryMode::MimeDigests,DeliveryStatus::Enabled,true,true),
        ("SummaryCase",DeliveryMode::SummaryDigests,DeliveryStatus::Enabled,true,true),
        ("disabled",DeliveryMode::MimeDigests,DeliveryStatus::ByUser,true,true),
        ("Author",DeliveryMode::MimeDigests,DeliveryStatus::Enabled,false,true),
        ("Direct",DeliveryMode::MimeDigests,DeliveryStatus::Enabled,true,false),
    ] {
        let member=db.members().create(NewMember{list_id:list.clone(),email:format!("{name}@example.invalid"),role:MemberRole::Member,subscription_mode:SubscriptionMode::AsAddress,display_name:name.into()}).await.unwrap();
        db.preferences().set_member(member.id,Preferences{delivery_mode:Some(mode),delivery_status:Some(status),receive_own_postings:Some(own),receive_list_copy:Some(copy),..Default::default()}).await.unwrap();
    }
    let raw=b"From: author@private.invalid\r\nTo: test@example.invalid, direct@example.invalid\r\nBcc: hidden@private.invalid\r\nApproved: password\r\nReceived: private.invalid\r\nMessage-ID: <private@private.invalid>\r\nSubject: Digest evidence\r\n\r\nbody evidence\r\n";
    let mut inbound=InboundHandler{db:db.clone(),local_hostname:"example.invalid".into(),max_message_bytes:100_000,max_recipients:10,command_timeout:Duration::from_secs(2),in_max_attempts:3};
    assert_eq!(inbound.deliver(Some("author@example.invalid"),&["test@example.invalid".into()],raw).await[0].code,250);
    let mut config=Config::default(); config.mta.smtp_tls="plaintext_trusted_relay".into(); config.mailman.default_member_action=listmngr_core::ModerationAction::Accept;
    let keys=tempfile::tempdir().unwrap();
    if signing {
        let key=keys.path().join("owned.pem");
        let output=std::process::Command::new("openssl").args(["genpkey","-algorithm","RSA","-pkeyopt","rsa_keygen_bits:2048","-out"]).arg(&key).output().unwrap();
        assert!(output.status.success());
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        config.mta.dkim_signing.push(listmngr_core::DkimSigningConfig{domain:"example.invalid".into(),selector:"fixture".into(),private_key_file:key});
    }
    let listener=TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut role=MailRoleConfig::from_core(&config).unwrap();role.smtp_relay=listener.local_addr().unwrap();
    let (stop,rx)=watch::channel(false);
    let task=tokio::spawn(run_in_processor(db.clone(),config,role.clone(),"in-test".into(),rx));
    loop { let n:i64=sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs WHERE queue='in' AND state='done'").fetch_one(db.pool()).await.unwrap(); if n==1{break;} tokio::task::yield_now().await; }
    stop.send(true).unwrap();task.await.unwrap();
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM queue_jobs WHERE queue='digest'").fetch_one(db.pool()).await.unwrap(),1);
    listmngr_runners::digests::tick(&db,"digest-test",false).await.unwrap();
    db.pool().close().await;
    let db=Database::connect(&url,1).await.unwrap();
    assert_eq!(listmngr_runners::digests::send(&db,&list,true).await.unwrap(),1);
    assert_eq!(listmngr_runners::digests::send(&db,&list,true).await.unwrap(),0);
    let (stop,rx)=watch::channel(false);
    let task=tokio::spawn(run_out_processor(db.clone(),role,"out-test".into(),rx));
    let received=sink(listener,4).await;
    stop.send(true).unwrap();task.await.unwrap();
    let recipients=received.iter().map(|(r,_)|r.as_str()).collect::<String>();
    for name in ["regular","PlainCase","MimeCase","SummaryCase"] {assert_eq!(recipients.matches(&format!("<{name}@example.invalid>")).count(),1,"{recipients}");}
    for (recipient,raw) in &received {
        assert_eq!(listmngr_mail::header_value(raw,"DKIM-Signature").is_some(),signing);
        if signing {
            let name = if recipient.contains("regular@") { "digest-regular" }
                else if recipient.contains("PlainCase@") { "digest-plain" }
                else if recipient.contains("MimeCase@") { "digest-mime" }
                else if recipient.contains("SummaryCase@") { "digest-summary" }
                else { panic!("unexpected fixture recipient") };
            dkim_capture::export_capture(name, raw, &keys.path().join("owned.pem"), "example.invalid");
        }
        let text=String::from_utf8_lossy(raw);
        if munge && recipient.contains("regular@") {
            assert!(text.contains("author@private.invalid via test@example.invalid"),"missing attribution: {text}");
            assert!(!text.contains("From: author@private.invalid\r\n"),"unmitigated From: {text}");
        } else if munge {
            let parsed = mail_parser::MessageParser::default().parse(raw).unwrap();
            if recipient.contains("PlainCase@") {
                assert!(parsed.body_text(0).unwrap().contains("body evidence"));
                assert!(!parsed.body_text(0).unwrap().contains("via test@example.invalid"));
            } else {
                let nested = parsed.attachments().next().unwrap().message().unwrap();
                assert_eq!(nested.from().unwrap().first().unwrap().address(), Some("author@private.invalid"));
                assert!(nested.body_text(0).unwrap().contains("body evidence"));
            }
        } else { assert!(!text.contains("private.invalid"),"privacy leak: {text}"); }
        assert!(!text.contains("password"));
        if recipient.contains("MimeCase@") {assert!(text.contains("multipart/digest")); assert!(text.contains("body evidence"));}
        if recipient.contains("SummaryCase@") {assert!(text.contains("multipart/digest")); assert!(text.contains("body evidence"));}
    }
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM queue_jobs WHERE queue='out' AND state='done'").fetch_one(db.pool()).await.unwrap(),4);
    db.pool().close().await;std::fs::remove_dir_all(dir).unwrap();
    }).await.unwrap();
}
