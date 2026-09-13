use listmngr_core::Error;
use listmngr_db::{
    Database, NewList,
    mail_queue::{Lease, LeaseClock, NewMessage, Queue},
};
use std::sync::atomic::{AtomicI64, Ordering};
#[derive(Debug)]
struct Clock {
    remaining: AtomicI64,
    final_ms: AtomicI64,
}
impl Clock {
    const fn new(samples: i64, final_ms: i64) -> Self {
        Self {
            remaining: AtomicI64::new(samples),
            final_ms: AtomicI64::new(final_ms),
        }
    }
}
impl LeaseClock for Clock {
    fn now_ms(&self) -> i64 {
        if self.remaining.fetch_sub(1, Ordering::SeqCst) > 0 {
            101
        } else {
            self.final_ms.load(Ordering::SeqCst)
        }
    }
}
async fn fixture(db: &Database, scenario: &str) -> Lease {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    db.lists()
        .create(NewList {
            list_id: "clock.example.invalid".parse().unwrap(),
            display_name: "Clock".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let command = if let Some(action) = scenario.strip_prefix("confirm-") {
        if action == "leave" {
            db.members()
                .create(listmngr_db::NewMember {
                    list_id: "clock.example.invalid".parse().unwrap(),
                    email: "Case@example.invalid".into(),
                    display_name: String::new(),
                    role: listmngr_core::MemberRole::Member,
                    subscription_mode: listmngr_core::SubscriptionMode::AsAddress,
                })
                .await
                .unwrap();
        }
        sqlx::query("INSERT INTO subscription_workflows(id,list_id,email,original_email,action,token_hash,created_at,expires_at) VALUES('fixture','clock.example.invalid','case@example.invalid','Case@example.invalid',$1,$2,100,100000)").bind(action).bind(format!("{:x}",Sha256::digest([0_u8;32]))).execute(db.pool()).await.unwrap();
        serde_json::json!({"confirm":base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([0_u8;32])})
    } else {
        serde_json::json!(if scenario == "throttled" {
            "help"
        } else {
            scenario
        })
    };
    if scenario == "throttled" {
        sqlx::query("UPDATE subscription_rate SET requests=100,window_start=100 WHERE id=1")
            .execute(db.pool())
            .await
            .unwrap();
    }
    db.mail_queue().enqueue(NewMessage {raw:b"Subject: command\r\n\r\n".to_vec(), external_id:"command@example.invalid".into(), context:serde_json::json!({"list_id":"clock.example.invalid","envelope_sender":"Case@example.invalid","subscription_command":command}).to_string(),queue:Queue::In,max_attempts:3},100).await.unwrap();
    db.mail_queue()
        .claim(Queue::In, "owner", 100, 100)
        .await
        .unwrap()
        .unwrap()
}
// Serialize every column, including deleted membership/preferences, consumed tokens,
// rate counters, byte blobs and audits, so equal counts cannot hide partial rollback.
async fn snapshot(db: &Database) -> Vec<String> {
    use sqlx::{Column, Row, TypeInfo, ValueRef};
    let mut result = Vec::new();
    for table in [
        "subscription_workflows",
        "subscription_rate",
        "email_help_requests",
        "workflow_notices",
        "members",
        "addresses",
        "preferences",
        "queue_jobs",
        "delivery_recipients",
        "messages",
        "message_blobs",
        "audit_log",
        "held_messages",
        "moderation_log",
    ] {
        let rows = sqlx::query(&format!("SELECT * FROM {table}"))
            .fetch_all(db.pool())
            .await
            .unwrap();
        let mut records = Vec::new();
        for row in rows {
            let mut values = Vec::new();
            for col in row.columns() {
                let i = col.ordinal();
                let raw = row.try_get_raw(i).unwrap();
                let value = if raw.is_null() {
                    "NULL".into()
                } else {
                    match raw.type_info().name() {
                        "BIGINT" | "INTEGER" | "SMALLINT" => {
                            format!("{:?}", row.try_get::<i64, _>(i).unwrap())
                        }
                        "DOUBLE" | "REAL" => format!("{:?}", row.try_get::<f64, _>(i).unwrap()),
                        "BLOB" | "BYTEA" => format!("{:?}", row.try_get::<Vec<u8>, _>(i).unwrap()),
                        _ => format!("{:?}", row.try_get::<String, _>(i).unwrap()),
                    }
                };
                values.push(value);
            }
            records.push(format!("{values:?}"));
        }
        records.sort();
        result.push(format!("{table}:{records:?}"));
    }
    result
}
async fn complete(db: &Database, lease: &Lease, clock: &Clock) -> listmngr_core::Result<()> {
    db.workflows()
        .with_clock(clock)
        .request_from_lease(lease, 101)
        .await
}
async fn success(db: &Database, lease: &Lease, scenario: &str) {
    assert_eq!(
        db.mail_queue().job(lease.job.id).await.unwrap().state,
        listmngr_db::mail_queue::JobState::Done
    );
    let notices: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workflow_notices")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(notices, i64::from(scenario != "throttled"));
    if scenario.starts_with("confirm-") {
        let consumed: i64 = sqlx::query_scalar("SELECT consumed FROM subscription_workflows")
            .fetch_one(db.pool())
            .await
            .unwrap();
        let members: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM members")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(consumed, 1);
        assert_eq!(members, i64::from(scenario == "confirm-join"));
    }
}
#[tokio::test]
async fn sqlite_workflow_final_audit() {
    for scenario in [
        "join",
        "leave",
        "help",
        "throttled",
        "confirm-join",
        "confirm-leave",
    ] {
        for (final_ms, renewed, expires) in [
            (200, false, true),
            (199, false, false),
            (250, true, false),
            (401, true, true),
        ] {
            let db = Database::connect("sqlite::memory:", 1).await.unwrap();
            let lease = fixture(&db, scenario).await;
            if renewed {
                db.mail_queue().heartbeat(&lease, 101, 300).await.unwrap();
            }
            let before = snapshot(&db).await;
            let result = complete(&db, &lease, &Clock::new(2, final_ms)).await;
            if expires {
                assert!(
                    matches!(result, Err(Error::Conflict(_))),
                    "workflow final audit committed {scenario}: {result:?}"
                );
                assert_eq!(snapshot(&db).await, before);
                complete(&db, &lease, &Clock::new(0, 101)).await.unwrap();
            } else {
                result.unwrap();
            }
            success(&db, &lease, scenario).await;
            db.pool().close().await;
        }
    }
}

#[path = "workflow_final_audit/postgres.rs"]
mod postgres;
