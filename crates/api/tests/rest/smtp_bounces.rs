use super::*;
use listmngr_db::mail_queue::{ChildJob, NewMessage, Queue, RecipientOutcome};

async fn populated() -> (axum::Router, String) {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains().create("example.com", "", None).await.unwrap();
    for name in ["other", "dev"] {
        db.lists()
            .create(listmngr_db::NewList {
                list_id: format!("{name}.example.com").parse().unwrap(),
                display_name: name.into(),
                style: "legacy-default".into(),
            })
            .await
            .unwrap();
        for recipient in ["One@Example.net", "Two@Example.net"] {
            publish(&db, name, recipient).await;
        }
    }
    let user = db
        .users()
        .create(NewUser {
            display_name: "reader".into(),
            email: "reader@example.com".into(),
            password: "Orbit!Cobalt7-River$Quartz".into(),
            server_owner: true,
        })
        .await
        .unwrap();
    let token = db
        .tokens()
        .create_scoped(
            user.id,
            "reader",
            &["lists:read"],
            Some(&"dev.example.com".parse().unwrap()),
            None,
            None,
        )
        .await
        .unwrap()
        .token;
    (listmngr_api::router(db, config_with_rate(100)), token)
}

async fn publish(db: &Database, name: &str, recipient: &str) {
    db.mail_queue()
        .enqueue(
            NewMessage {
                raw: b"private body".to_vec(),
                external_id: "private external token".into(),
                context:
                    serde_json::json!({"list_id": format!("{name}.example.com"),"secret":"token"})
                        .to_string(),
                queue: Queue::In,
                max_attempts: 3,
            },
            100,
        )
        .await
        .unwrap();
    let source = db
        .mail_queue()
        .claim(Queue::In, "in", 100, 1000)
        .await
        .unwrap()
        .unwrap();
    db.mail_queue()
        .complete_with_children(
            &source,
            101,
            &[ChildJob {
                queue: Queue::Out,
                max_attempts: 3,
                recipients: vec![recipient.into()],
            }],
        )
        .await
        .unwrap();
    let lease = db
        .mail_queue()
        .claim(Queue::Out, "out", 102, 1000)
        .await
        .unwrap()
        .unwrap();
    db.mail_queue()
        .begin_delivery(&lease, 103, &[recipient.into()])
        .await
        .unwrap();
    db.mail_queue()
        .finish_delivery_with_smtp(
            &lease,
            104,
            &[(
                recipient.into(),
                RecipientOutcome::Failed,
                "550 private detail".into(),
            )],
            0,
            &if recipient == "One@Example.net" {
                vec![(
                    recipient.into(),
                    listmngr_core::SmtpFailure {
                        stage: listmngr_core::SmtpFailureStage::MailFrom,
                        code: 554,
                    },
                )]
            } else {
                vec![]
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn smtp_bounce_api_reads_typed_scoped_pages_without_private_transport_data() {
    let (app, token) = populated().await;
    for (prefix, key, total) in [
        ("/api/v1", "items", "total"),
        ("/3.1", "entries", "total_size"),
    ] {
        let path = format!("{prefix}/lists/dev.example.com/bounces");
        let response = call(&app, "GET", &format!("{path}?count=1"), Some(&token), None).await;
        assert_eq!(response.status(), StatusCode::OK);
        let first = response_json(response).await;
        assert_eq!(first[total], 2);
        assert_eq!(first[key][0]["recipient"], "One@Example.net");
        assert_eq!(first[key][0]["list_id"], "dev.example.com");
        assert_eq!(first[key][0]["processed"], false);
        assert_eq!(first[key][0]["smtp_stage"], "mail_from");
        assert_eq!(first[key][0]["smtp_code"], 554);
        let serialized = first.to_string();
        for secret in [
            "private",
            "secret",
            "token",
            "detail",
            "body",
            "external_id",
        ] {
            assert!(!serialized.contains(secret), "{serialized}");
        }
        let second = response_json(
            call(
                &app,
                "GET",
                &format!("{path}?count=1&page=2"),
                Some(&token),
                None,
            )
            .await,
        )
        .await;
        assert_eq!(second[key][0]["recipient"], "Two@Example.net");
        for field in ["smtp_stage", "smtp_code"] {
            assert_eq!(second[key][0].get(field), Some(&serde_json::Value::Null));
        }
        assert_ne!(first[key][0]["id"], second[key][0]["id"]);
        let last = response_json(
            call(
                &app,
                "GET",
                &format!("{path}?count=1&page=3"),
                Some(&token),
                None,
            )
            .await,
        )
        .await;
        assert_eq!(last[key].as_array().unwrap().len(), 0);
        assert_eq!(
            call(&app, "GET", &path, None, None).await.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(
                &app,
                "GET",
                &format!("{path}?count=101"),
                Some(&token),
                None
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
        for name in ["other", "missing"] {
            assert_eq!(
                call(
                    &app,
                    "GET",
                    &format!("{prefix}/lists/{name}.example.com/bounces"),
                    Some(&token),
                    None
                )
                .await
                .status(),
                StatusCode::FORBIDDEN
            );
        }
    }
    let (app, token, _) = setup(&["system:read"]).await;
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/lists/missing.example.com/bounces",
            Some(&token),
            None
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
}
