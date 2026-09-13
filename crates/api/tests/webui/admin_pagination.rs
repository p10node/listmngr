use super::{call, fixture, login_as, member, text, user};
use axum::http::StatusCode;
use listmngr_core::MemberRole;

#[tokio::test]
async fn admin_pages_filter_authority_before_pagination() {
    let (db, app) = fixture().await;
    user(&db, "owner@example.com", false).await;
    member(&db, "owner@example.com", MemberRole::Owner).await;
    for i in 0..25 {
        db.lists()
            .create(listmngr_db::NewList {
                list_id: format!("aaa{i:02}.example.com").parse().unwrap(),
                display_name: "Not owned".into(),
                style: "legacy-default".into(),
            })
            .await
            .unwrap();
        member(
            &db,
            &format!("member{i:02}@example.com"),
            MemberRole::Member,
        )
        .await;
    }
    let cookie = login_as(&app, "owner@example.com").await;
    let index = text(call(&app, "GET", "/web/admin", &cookie, "").await).await;
    assert!(index.contains("/web/lists/public.example.com/members"));
    assert!(!index.contains("Not owned"));
    assert!(index.contains("&lt;script&gt;"));
    check_pages(&app, &cookie).await;
}

async fn check_pages(app: &axum::Router, cookie: &str) {
    let url = "/web/lists/public.example.com/members";
    let first = text(call(app, "GET", url, cookie, "").await).await;
    let second = text(call(app, "GET", &format!("{url}?page=1"), cookie, "").await).await;
    assert_eq!(first.matches("<article>").count(), 20);
    assert_eq!(second.matches("<article>").count(), 5);
    assert!(first.contains("member00@example.com"));
    assert!(!first.contains("member24@example.com"));
    assert!(second.contains("member24@example.com"));
    assert!(!second.contains("member00@example.com"));
    assert_eq!(
        call(app, "GET", &format!("{url}?page=10001"), cookie, "")
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
}
