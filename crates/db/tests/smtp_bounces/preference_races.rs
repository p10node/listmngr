use super::{
    fixture_at,
    score_cases::{finish, member, score},
    *,
};
use listmngr_core::{DeliveryStatus, MemberRole, Preferences, SmtpFailureStage};
use std::time::Duration;

async fn isolated(case: &'static str) {
    let url = std::env::var("TEST_POSTGRES_URL").expect("explicit disposable PostgreSQL URL");
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("preference_race_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let url = format!(
        "{url}{}options=-csearch_path%3D{schema}",
        if url.contains('?') { '&' } else { '?' }
    );
    let (db, lease) = fixture_at(&url).await;
    let writer = Database::connect(&url, 2).await.unwrap();
    let cleanup_db = db.clone();
    let cleanup_writer = writer.clone();
    let result = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(20), async {
            if case == "metadata" || case == "mode" {
                metadata_case(&db, &writer, case).await;
            } else {
                reason_case(&db, &writer, &lease, case).await;
            }
        })
        .await
        .unwrap();
    })
    .await;
    cleanup_db.pool().close().await;
    cleanup_writer.pool().close().await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    result.unwrap();
}

async fn waiting(db: &Database, pid: i32) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let blocked: bool = sqlx::query_scalar("SELECT cardinality(pg_blocking_pids($1)) > 0")
                .bind(pid)
                .fetch_one(db.pool())
                .await
                .unwrap();
            if blocked {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("worker must reach a real PostgreSQL lock barrier");
}

async fn disable_layer(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    layer: &str,
    user: listmngr_core::UserId,
) {
    if layer == "address_new" {
        let id = uuid::Uuid::now_v7().to_string();
        sqlx::query("INSERT INTO preferences(id,delivery_status) VALUES($1,'by_moderator')")
            .bind(&id)
            .execute(&mut **tx)
            .await
            .unwrap();
        sqlx::query("UPDATE addresses SET preferences_id=$1 WHERE email='mixed@example.com'")
            .bind(id)
            .execute(&mut **tx)
            .await
            .unwrap();
        return;
    }
    let query = match layer {
        "member" => {
            "UPDATE preferences SET delivery_status='by_moderator' WHERE id=(SELECT preferences_id FROM members)"
        }
        "address" => {
            "UPDATE preferences SET delivery_status='by_moderator' WHERE id=(SELECT preferences_id FROM addresses WHERE email='mixed@example.com')"
        }
        _ => {
            "UPDATE preferences SET delivery_status='by_moderator' WHERE id=(SELECT preferences_id FROM users WHERE id=$1)"
        }
    };
    let q = sqlx::query(query);
    let q = if layer == "user" {
        q.bind(user.to_string())
    } else {
        q
    };
    q.execute(&mut **tx).await.unwrap();
}

async fn reason_case(db: &Database, writer: &Database, lease: &Lease, layer: &str) {
    let user = db
        .users()
        .create(listmngr_db::NewUser {
            email: "Mixed@Example.com".into(),
            display_name: String::new(),
            password: "Fixture!Orbit-7Quartz".into(),
            server_owner: false,
        })
        .await
        .unwrap();
    member(db, MemberRole::Member, true).await;
    let m = db
        .members()
        .roster(&"test.example.com".parse().unwrap(), MemberRole::Member)
        .await
        .unwrap()
        .remove(0);
    if layer != "address_new" {
        db.preferences()
            .set_address("Mixed@Example.com", Preferences::default())
            .await
            .unwrap();
    }
    sqlx::query(
        "UPDATE members SET bounce_score=4,last_bounce_received='1969-12-31T00:00:00+00:00'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    db.mail_queue()
        .begin_delivery(lease, 103, &["Mixed@Example.com".into()])
        .await
        .unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(db.pool())
        .await
        .unwrap();
    // Model the uncommitted row writes performed by PreferencesRepo setters.
    // Holding the member preference also catches the old scorer's late write.
    let mut tx = writer.pool().begin().await.unwrap();
    sqlx::query("UPDATE preferences SET hide_address=1 WHERE id=$1")
        .bind(m.preferences_id.to_string())
        .execute(&mut *tx)
        .await
        .unwrap();
    disable_layer(&mut tx, layer, user.id).await;
    let work = finish(db, lease, SmtpFailureStage::Rcpt, 550);
    let release = async {
        waiting(writer, pid).await;
        tx.commit().await.unwrap();
    };
    let (result, ()) = tokio::join!(work, release);
    result.unwrap();
    assert_eq!(
        db.preferences()
            .resolve_member(m.id, "en")
            .await
            .unwrap()
            .delivery_status,
        Some(DeliveryStatus::ByModerator),
        "{layer}"
    );
    assert!((score(db).await - 4.0).abs() < f64::EPSILON);
    assert_eq!(
        db.preferences()
            .get(m.preferences_id)
            .await
            .unwrap()
            .hide_address,
        Some(true)
    );
    let audits: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log WHERE action IN ('bounce.disable','bounce.score')",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(audits, 0);
    assert_eq!(
        db.members()
            .get(m.id)
            .await
            .unwrap()
            .last_bounce_received
            .unwrap()
            .timestamp_millis(),
        -86_400_000
    );
}

async fn metadata_case(db: &Database, writer: &Database, case: &str) {
    member(db, MemberRole::Member, true).await;
    let m = db
        .members()
        .roster(&"test.example.com".parse().unwrap(), MemberRole::Member)
        .await
        .unwrap()
        .remove(0);
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(db.pool())
        .await
        .unwrap();
    // Own the same member -> preferences write boundary as threshold disablement.
    let mut tx = writer.pool().begin().await.unwrap();
    sqlx::query(
        "UPDATE members SET bounce_score=0,last_bounce_received='1970-01-01T00:00:00.104+00:00'",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query("UPDATE preferences SET delivery_status='by_bounces',hide_address=1 WHERE id=$1")
        .bind(m.preferences_id.to_string())
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("INSERT INTO audit_log(id,at,action,target_type,target_id,diff) VALUES($1,'1970-01-01T00:00:00+00:00','bounce.disable','member',$2,'{}')")
        .bind(uuid::Uuid::now_v7().to_string()).bind(m.id.to_string()).execute(&mut *tx).await.unwrap();
    let patch = if case == "mode" {
        serde_json::json!({"display_name":"updated", "delivery_mode":"mime_digests"})
    } else {
        serde_json::json!({"display_name":"updated"})
    };
    let repo = db.members();
    let work = repo.update(m.id, &patch);
    let release = async {
        waiting(writer, pid).await;
        tx.commit().await.unwrap();
    };
    let (result, ()) = tokio::join!(work, release);
    assert_eq!(result.unwrap().display_name, "updated");
    let prefs = db.preferences().get(m.preferences_id).await.unwrap();
    assert_eq!(prefs.delivery_status, Some(DeliveryStatus::ByBounces));
    assert_eq!(prefs.hide_address, Some(true));
    if case == "mode" {
        assert_eq!(
            prefs.delivery_mode,
            Some(listmngr_core::DeliveryMode::MimeDigests)
        );
    }
    assert!(score(db).await.abs() < f64::EPSILON);
    let audits: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='bounce.disable'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(audits, 1);
    // Explicit status mutation still works; omission is not a blanket refusal.
    db.members()
        .update(m.id, &serde_json::json!({"delivery_status":"by_user"}))
        .await
        .unwrap();
    assert_eq!(
        db.preferences()
            .get(m.preferences_id)
            .await
            .unwrap()
            .delivery_status,
        Some(DeliveryStatus::ByUser)
    );
}

#[tokio::test]
#[ignore = "requires explicit disposable TEST_POSTGRES_URL; owns schema"]
async fn postgres_metadata_patch_preserves_committed_disable() {
    for case in ["metadata", "mode"] {
        isolated(case).await;
    }
}

#[tokio::test]
#[ignore = "requires explicit disposable TEST_POSTGRES_URL; owns schema"]
async fn postgres_preference_disable_wins_before_scoring() {
    for layer in ["member", "address", "user", "address_new"] {
        isolated(layer).await;
    }
}
