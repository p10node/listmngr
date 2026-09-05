use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use listmngr_core::{Config, DomainId, ListId, MemberRole, SubscriptionMode, UserId};
use listmngr_db::{Database, NewList, NewMember, NewUser};
use tower::ServiceExt;

struct Identity {
    domain: DomainId,
    list: ListId,
    user: UserId,
}

async fn identity(db: &Database, label: &str) -> Identity {
    let host = format!("{label}-{}.invalid", uuid::Uuid::now_v7());
    let domain = db.domains().create(&host, "", None).await.unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: format!("scoped.{host}").parse().unwrap(),
            display_name: label.into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let email = format!("subscriber@{host}");
    let user = db
        .users()
        .create(NewUser {
            display_name: label.into(),
            email: email.clone(),
            password: "Orbit!Cobalt7-River$Quartz".into(),
            server_owner: false,
        })
        .await
        .unwrap();
    db.members()
        .create(NewMember {
            list_id: list.id.clone(),
            email,
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsUser,
            display_name: label.into(),
        })
        .await
        .unwrap();
    Identity {
        domain: domain.id,
        list: list.id,
        user: user.id,
    }
}

async fn get(app: &axum::Router, token: &str, path: &str) -> (StatusCode, serde_json::Value) {
    let mut request = Request::builder()
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    request.extensions_mut().insert(axum::extract::ConnectInfo(
        "127.0.0.1:4242".parse::<std::net::SocketAddr>().unwrap(),
    ));
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn assert_user_bounds(app: &axum::Router, token: &str, visible: UserId, hidden: UserId) {
    for prefix in ["/api/v1", "/3.1"] {
        for suffix in ["", "/preferences", "/all/preferences", "/addresses"] {
            let path = format!("{prefix}/users/{visible}{suffix}");
            assert_eq!(get(app, token, &path).await.0, StatusCode::OK, "{path}");
            let path = format!("{prefix}/users/{hidden}{suffix}");
            assert_eq!(
                get(app, token, &path).await.0,
                StatusCode::FORBIDDEN,
                "{path}"
            );
        }
        let (status, body) = get(app, token, &format!("{prefix}/users")).await;
        assert_eq!(status, StatusCode::OK);
        let rendered = body.to_string();
        assert!(rendered.contains(&visible.to_string()));
        assert!(!rendered.contains(&hidden.to_string()));
    }
}

#[tokio::test]
#[ignore = "requires disposable TEST_POSTGRES_URL; scripts/test-postgres.sh runs this explicitly"]
async fn postgres_scoped_user_routes_allow_inside_and_deny_outside_bounds() {
    let url = std::env::var("TEST_POSTGRES_URL").expect("TEST_POSTGRES_URL is mandatory");
    assert!(url.starts_with("postgres://") || url.starts_with("postgresql://"));
    let db = Database::connect(&url, 1).await.unwrap();
    db.migrate().await.unwrap();
    let visible = identity(&db, "visible").await;
    let hidden = identity(&db, "hidden").await;
    let mut config = Config::default();
    config.security.rate_limit.api = "1000/min".into();
    let app = listmngr_api::router(db.clone(), config);
    for (list, domain) in [(Some(&visible.list), None), (None, Some(visible.domain))] {
        let token = db
            .tokens()
            .create_scoped(
                visible.user,
                "pg-scope",
                &["system:read"],
                list,
                domain,
                None,
            )
            .await
            .unwrap();
        assert_user_bounds(&app, &token.token, visible.user, hidden.user).await;
    }
    for fixture in [visible, hidden] {
        for member in db
            .members()
            .roster(&fixture.list, MemberRole::Member)
            .await
            .unwrap()
        {
            db.members().delete(member.id).await.unwrap();
        }
        // Token list/domain bounds are restrictive FKs, owned by the user.
        db.users().delete(fixture.user).await.unwrap();
        db.lists().delete(&fixture.list).await.unwrap();
        db.domains().delete(fixture.list.mail_host()).await.unwrap();
    }
}
