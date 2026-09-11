use super::cases::{at, audit_rollback};
use super::*;

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns one disposable schema"]
async fn postgres_maintenance_atomicity_and_concurrent_winner() {
    let url = std::env::var("TEST_POSTGRES_URL").expect("TEST_POSTGRES_URL required");
    let admin = Database::connect(&url, 1).await.unwrap();
    let schema = format!("bounce_maintenance_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(admin.pool())
        .await
        .unwrap();
    let sep = if url.contains('?') { '&' } else { '?' };
    let fixture_url = format!("{url}{sep}options=-csearch_path%3D{schema}");
    let result=tokio::spawn(async move {
        let db=Database::connect(&fixture_url,5).await.unwrap(); db.migrate().await.unwrap();
        db.domains().create("example.invalid","",None).await.unwrap();
        let id="maintenance.example.invalid".parse().unwrap();
        db.lists().create(NewList{list_id:id,display_name:String::new(),style:"legacy-default".into()}).await.unwrap();
        let id="maintenance.example.invalid".parse().unwrap();
        audit_rollback(&db,&id).await;
        let m=disabled(&db,&id,"Concurrent@Example.invalid").await;
        db.lists().update(&id,&json!({"bounce_you_are_disabled_warnings":3})).await.unwrap();
        let a=db.bounce_maintenance(); let b=db.bounce_maintenance();
        let (a,b)=tokio::join!(a.sweep_at(100,None,at()),b.sweep_at(100,None,at()));
        let (a,b)=(a.unwrap(),b.unwrap()); assert_eq!(a.warned+b.warned,1); assert_eq!(a.failed+b.failed,0);
        // Explicit reenable while maintenance is waiting on the preference lock.
        let mut tx=db.pool().begin().await.unwrap();
        sqlx::query("UPDATE preferences SET delivery_status='enabled' WHERE id=$1").bind(m.preferences_id.0.to_string()).execute(&mut *tx).await.unwrap();
        let other=db.clone();
        let task=tokio::spawn(async move {other.bounce_maintenance().sweep_at(100,None,at()+chrono::Duration::days(7)).await.unwrap()});
        tokio::time::timeout(std::time::Duration::from_secs(5),async {
            loop {
                let waiting:i64=sqlx::query_scalar("SELECT COUNT(*) FROM pg_stat_activity WHERE pid<>pg_backend_pid() AND wait_event_type='Lock' AND query LIKE 'UPDATE preferences SET id=id%'").fetch_one(db.pool()).await.unwrap();
                if waiting>0 {break;} tokio::task::yield_now().await;
            }
        }).await.unwrap();
        tx.commit().await.unwrap();
        let summary=task.await.unwrap(); assert_eq!(summary.warned,0); assert_eq!(summary.removed,0); assert_eq!(summary.failed,0);
        assert!(db.members().get(m.id).await.is_ok());
        db.pool().close().await;
    }).await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(admin.pool())
        .await
        .unwrap();
    admin.pool().close().await;
    result.unwrap();
}
