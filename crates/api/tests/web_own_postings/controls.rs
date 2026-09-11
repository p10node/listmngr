// Supplementary regression controls; the feature REDs live in web_own_postings.rs.
use super::{call, seed};
use axum::http::StatusCode;
use listmngr_core::{Config, Member};
use listmngr_db::{Database, NewUser, web_sessions::WebSession};
use sqlx::Row;

struct Fixture {
    db: Database,
    app: axum::Router,
    member: Member,
    session: WebSession,
}
impl Fixture {
    async fn new(db: Database) -> Self {
        let (db, member, session) = seed(db).await;
        let mut config = Config::default();
        config.site.base_url = "http://localhost".into();
        let app = listmngr_api::router(db.clone(), config);
        Self {
            db,
            app,
            member,
            session,
        }
    }
    async fn post(&self, extra: &str, origin: &str, csrf: &str) -> StatusCode {
        call(
            &self.app,
            "POST",
            &format!("/web/members/{}/preferences", self.member.id),
            &format!("listmngr_session={}", self.session.token),
            &format!("csrf={csrf}&delivery_mode=mime_digests&delivery_status=by_user{extra}"),
            origin,
        )
        .await
        .status()
    }
    async fn own(&self) -> Option<i32> {
        sqlx::query_scalar("SELECT receive_own_postings FROM preferences WHERE id=$1")
            .bind(self.member.preferences_id.0.to_string())
            .fetch_one(self.db.pool())
            .await
            .unwrap()
    }
    async fn snapshot(&self) -> Vec<String> {
        let rows = sqlx::query("SELECT id,acknowledge_posts,hide_address,preferred_language,receive_list_copy,receive_own_postings,delivery_mode,delivery_status FROM preferences ORDER BY id")
            .fetch_all(self.db.pool()).await.unwrap();
        rows.iter()
            .map(|r| {
                format!(
                    "{:?}",
                    (
                        r.get::<String, _>("id"),
                        r.get::<Option<i32>, _>("acknowledge_posts"),
                        r.get::<Option<i32>, _>("hide_address"),
                        r.get::<Option<String>, _>("preferred_language"),
                        r.get::<Option<i32>, _>("receive_list_copy"),
                        r.get::<Option<i32>, _>("receive_own_postings"),
                        r.get::<Option<String>, _>("delivery_mode"),
                        r.get::<Option<String>, _>("delivery_status")
                    )
                )
            })
            .collect()
    }
    async fn audits(&self) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT diff FROM audit_log WHERE action='preferences.update' ORDER BY id",
        )
        .fetch_all(self.db.pool())
        .await
        .unwrap()
    }
    async fn rejected(&self, extra: &str, origin: &str, csrf: &str, expected: StatusCode) {
        let before = self.snapshot().await;
        let audits = self.audits().await;
        assert_eq!(self.post(extra, origin, csrf).await, expected);
        assert_eq!(self.snapshot().await, before);
        assert_eq!(self.audits().await, audits);
    }
}

async fn values(f: &Fixture) {
    let inherited = uuid::Uuid::now_v7().to_string();
    sqlx::query("INSERT INTO preferences(id,receive_own_postings,receive_list_copy,preferred_language) VALUES($1,0,1,'fr')")
        .bind(&inherited).execute(f.db.pool()).await.unwrap();
    sqlx::query("UPDATE addresses SET preferences_id=$1 WHERE id=$2")
        .bind(&inherited)
        .bind(f.member.address_id.0.to_string())
        .execute(f.db.pool())
        .await
        .unwrap();
    let shared_before: Vec<_> = f
        .snapshot()
        .await
        .into_iter()
        .filter(|p| !p.contains(&f.member.preferences_id.0.to_string()))
        .collect();
    sqlx::query("UPDATE preferences SET acknowledge_posts=1,hide_address=1,preferred_language='vi',receive_list_copy=0 WHERE id=$1")
        .bind(f.member.preferences_id.0.to_string()).execute(f.db.pool()).await.unwrap();
    for (extra, expected) in [
        ("", None),
        ("&receive_own_postings=false", Some(0)),
        ("", Some(0)),
        ("&receive_own_postings=true", Some(1)),
        ("", Some(1)),
    ] {
        assert_eq!(
            f.post(extra, "http://localhost", &f.session.csrf).await,
            StatusCode::SEE_OTHER
        );
        assert_eq!(f.own().await, expected);
        let audit: serde_json::Value =
            serde_json::from_str(f.audits().await.last().unwrap()).unwrap();
        let mut expected_audit =
            serde_json::json!({"delivery_mode":"mime_digests","delivery_status":"by_user"});
        if !extra.is_empty() {
            expected_audit["receive_own_postings"] = serde_json::json!(expected == Some(1));
        }
        assert_eq!(audit, expected_audit);
        let p =
            f.db.preferences()
                .get(f.member.preferences_id)
                .await
                .unwrap();
        assert_eq!(p.acknowledge_posts, Some(true));
        assert_eq!(p.hide_address, Some(true));
        assert_eq!(p.preferred_language.as_deref(), Some("vi"));
        assert_eq!(p.receive_list_copy, Some(false));
    }
    f.db.browser_preferences(
        &f.session,
        f.member.id,
        listmngr_core::DeliveryMode::Regular,
        listmngr_core::DeliveryStatus::Enabled,
    )
    .await
    .unwrap();
    assert_eq!(f.own().await, Some(1));
    let shared_after: Vec<_> = f
        .snapshot()
        .await
        .into_iter()
        .filter(|p| !p.contains(&f.member.preferences_id.0.to_string()))
        .collect();
    assert_eq!(shared_before, shared_after);
}

async fn security(f: &Fixture) {
    for malformed in [
        "",
        "1",
        "0",
        "yes",
        "on",
        "TRUE",
        "null",
        "false&receive_own_postings=true",
    ] {
        f.rejected(
            &format!("&receive_own_postings={malformed}"),
            "http://localhost",
            &f.session.csrf,
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    }
    for origin in ["null", "", "https://foreign.invalid"] {
        f.rejected(
            "&receive_own_postings=false",
            origin,
            &f.session.csrf,
            StatusCode::FORBIDDEN,
        )
        .await;
    }
    f.rejected(
        "&receive_own_postings=false",
        "http://localhost",
        "wrong",
        StatusCode::FORBIDDEN,
    )
    .await;
    for status in ["by_bounces", "by_moderator", "unknown"] {
        sqlx::query("UPDATE preferences SET delivery_status=$1 WHERE id=$2")
            .bind(status)
            .bind(f.member.preferences_id.0.to_string())
            .execute(f.db.pool())
            .await
            .unwrap();
        f.rejected(
            "&receive_own_postings=false",
            "http://localhost",
            &f.session.csrf,
            StatusCode::FORBIDDEN,
        )
        .await;
    }
    sqlx::query("UPDATE preferences SET delivery_status='enabled' WHERE id=$1")
        .bind(f.member.preferences_id.0.to_string())
        .execute(f.db.pool())
        .await
        .unwrap();
    let foreign =
        f.db.users()
            .create(NewUser {
                email: "foreign@recover.invalid".into(),
                display_name: "Foreign".into(),
                password: "different secure fixture password".into(),
                server_owner: false,
            })
            .await
            .unwrap();
    sqlx::query("UPDATE addresses SET user_id=$1 WHERE id=$2")
        .bind(foreign.id.to_string())
        .bind(f.member.address_id.0.to_string())
        .execute(f.db.pool())
        .await
        .unwrap();
    f.rejected(
        "&receive_own_postings=false",
        "http://localhost",
        &f.session.csrf,
        StatusCode::FORBIDDEN,
    )
    .await;
    sqlx::query("UPDATE addresses SET user_id=$1 WHERE id=$2")
        .bind(f.session.user_id.unwrap().to_string())
        .bind(f.member.address_id.0.to_string())
        .execute(f.db.pool())
        .await
        .unwrap();
    f.db.addresses()
        .verify("own@recover.invalid", false)
        .await
        .unwrap();
    f.rejected(
        "&receive_own_postings=false",
        "http://localhost",
        &f.session.csrf,
        StatusCode::FORBIDDEN,
    )
    .await;
    f.db.addresses()
        .verify("own@recover.invalid", true)
        .await
        .unwrap();
}

async fn rollback(f: &Fixture, sqlite: bool) {
    let sql = if sqlite {
        "CREATE TRIGGER own_audit_fail BEFORE INSERT ON audit_log WHEN NEW.action='preferences.update' BEGIN SELECT RAISE(ABORT,'fixture'); END"
    } else {
        "CREATE FUNCTION own_audit_fail() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='preferences.update' THEN RAISE EXCEPTION 'fixture'; END IF; RETURN NEW; END $$"
    };
    sqlx::query(sql).execute(f.db.pool()).await.unwrap();
    if !sqlite {
        sqlx::query("CREATE TRIGGER own_audit_fail BEFORE INSERT ON audit_log FOR EACH ROW EXECUTE FUNCTION own_audit_fail()").execute(f.db.pool()).await.unwrap();
    }
    f.rejected(
        "&receive_own_postings=false",
        "http://localhost",
        &f.session.csrf,
        StatusCode::INTERNAL_SERVER_ERROR,
    )
    .await;
    sqlx::query(if sqlite {
        "DROP TRIGGER own_audit_fail"
    } else {
        "DROP TRIGGER own_audit_fail ON audit_log"
    })
    .execute(f.db.pool())
    .await
    .unwrap();
    assert_eq!(
        f.post(
            "&receive_own_postings=false",
            "http://localhost",
            &f.session.csrf
        )
        .await,
        StatusCode::SEE_OTHER
    );
    assert_eq!(f.own().await, Some(0));
    f.db.delete_web_session(&f.session).await.unwrap();
    f.rejected(
        "&receive_own_postings=true",
        "http://localhost",
        &f.session.csrf,
        StatusCode::UNAUTHORIZED,
    )
    .await;
}

async fn controls(db: Database, sqlite: bool) {
    let f = Fixture::new(db).await;
    values(&f).await;
    security(&f).await;
    rollback(&f, sqlite).await;
    f.db.pool().close().await;
}

#[tokio::test]
async fn sqlite_own_postings_controls() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    controls(db, true).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns isolated schema"]
async fn postgres_own_postings_controls() {
    let url = std::env::var("TEST_POSTGRES_URL").unwrap();
    let admin = Database::connect(&url, 1).await.unwrap();
    let schema = format!("web_own_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(admin.pool())
        .await
        .unwrap();
    let sep = if url.contains('?') { '&' } else { '?' };
    let fixture = format!("{url}{sep}options=-csearch_path%3D{schema}");
    let result = tokio::spawn(async move {
        let db = Database::connect(&fixture, 3).await.unwrap();
        db.migrate().await.unwrap();
        controls(db, false).await;
    })
    .await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(admin.pool())
        .await
        .unwrap();
    admin.pool().close().await;
    result.unwrap();
}
