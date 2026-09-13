use super::{call, csrf, fixture, list_settings, login_as, member, text, user};
use axum::http::StatusCode;

#[tokio::test]
async fn owner_subject_prefix_label_and_escaping() {
    let (db, app) = fixture().await;
    user(&db, "prefix-label@example.com", false).await;
    let role = member(
        &db,
        "prefix-label@example.com",
        listmngr_core::MemberRole::Owner,
    )
    .await;
    db.lists()
        .update(
            &role.list_id,
            &serde_json::json!({"subject_prefix":"  \"<script>& Tiếng Việt + ✉  "}),
        )
        .await
        .unwrap();
    let cookie = login_as(&app, "prefix-label@example.com").await;
    let html = text(call(&app, "GET", list_settings::URL, &cookie, "").await).await;
    assert!(html.contains("<label for=\"subject_prefix\">Subject prefix</label>"));
    assert!(html.contains("<input type=\"text\" id=\"subject_prefix\" name=\"subject_prefix\" value=\"  &quot;&lt;script&gt;&amp; Tiếng Việt + ✉  \""));
    assert!(!html.contains("<script>"));
    assert!(html.contains("Leave empty for no prefix"));
}

#[tokio::test]
async fn owner_subject_prefix_post() {
    let (db, app) = fixture().await;
    user(&db, "prefix-owner@example.com", false).await;
    let role = member(
        &db,
        "prefix-owner@example.com",
        listmngr_core::MemberRole::Owner,
    )
    .await;
    let cookie = login_as(&app, "prefix-owner@example.com").await;
    let html = text(call(&app, "GET", list_settings::URL, &cookie, "").await).await;
    let base = list_settings::form(&csrf(&html));
    let value = "  [Tiếng Việt + ✉]  ";
    let field = serde_urlencoded::to_string([("subject_prefix", value)]).unwrap();
    assert_eq!(
        call(
            &app,
            "POST",
            list_settings::URL,
            &cookie,
            &format!("{base}&{field}")
        )
        .await
        .status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        db.lists().get(&role.list_id).await.unwrap().subject_prefix,
        value
    );
}
